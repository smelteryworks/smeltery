//! The `database` presence store (PubSub driver `database`): `presence_sockets` (one row per socket in a channel,
//! with its process and the time it was last marked alive) and `presence_users` (one row per member).
//!
//! The race rules (D-414): a join inserts its socket row, then the user row ignoring a conflict; the insert that
//! created the user row announces `member_added`. A leave deletes its socket row, then the user row only when no
//! socket row of that user is left in the channel (one statement); the delete that removed it announces
//! `member_removed`. Each statement commits on its own, so in every interleaving of two processes' joins and leaves
//! of one user, the user row exists exactly while a socket row does, and each change is announced once.
//! Every serving process marks its rows alive every 30 s (database clock) and removes the rows of processes unseen
//! for 90 s, announcing the members that left with them.

use smeltery_core::db::{Backend, Db};
use smeltery_core::{BoxFuture, Result};

use super::migrations::{SOCKETS, USERS};
use super::{Beat, Joined, Member, Memberships, Removed, STALE_AFTER, Store};

/// Rows one sweep removes at most (the rest at the next heartbeat).
const SWEEP_BATCH: u64 = 500;

/// The database's clock in Unix milliseconds, as an SQL expression.
fn now_sql(backend: Backend) -> &'static str {
    match backend {
        Backend::Postgres => "CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)",
        Backend::MySql => {
            "CAST(TIMESTAMPDIFF(MICROSECOND, '1970-01-01 00:00:00', UTC_TIMESTAMP(6)) DIV 1000 AS SIGNED)"
        }
        _ => "CAST(ROUND((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)",
    }
}

/// `n` placeholders of `backend`, from `first` (1-based).
fn ph(backend: Backend, n: usize) -> String {
    if backend == Backend::Postgres {
        format!("${n}")
    } else {
        "?".to_owned()
    }
}

/// What a test runs inside a heartbeat, between its touch and its reconcile (a join or a leave racing it).
#[cfg(test)]
type Hook = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

/// The database store of one process.
pub(crate) struct DatabaseStore {
    db: Db,
    /// This process's id (random), on its socket rows.
    process: String,
    #[cfg(test)]
    during_heartbeat: std::sync::Mutex<Option<Hook>>,
}

impl DatabaseStore {
    pub(crate) fn new(db: Db, process: String) -> Self {
        Self {
            db,
            process,
            #[cfg(test)]
            during_heartbeat: std::sync::Mutex::new(None),
        }
    }

    fn backend(&self) -> Backend {
        self.db.backend()
    }

    /// Join step 1: the socket row (kept when it exists: a heartbeat may have put it back first).
    pub(crate) async fn insert_socket(
        &self,
        channel: &str,
        socket: &str,
        user_id: &str,
    ) -> Result<()> {
        let b = self.backend();
        let values = format!(
            "(channel, socket_id, process, user_id, seen_at) VALUES ({}, {}, {}, {}, {})",
            ph(b, 1),
            ph(b, 2),
            ph(b, 3),
            ph(b, 4),
            now_sql(b)
        );
        let sql = match b {
            Backend::MySql => format!("INSERT IGNORE INTO {SOCKETS} {values}"),
            Backend::Postgres => format!("INSERT INTO {SOCKETS} {values} ON CONFLICT DO NOTHING"),
            _ => format!("INSERT OR IGNORE INTO {SOCKETS} {values}"),
        };
        self.db
            .execute_with(
                &sql,
                [
                    channel.into(),
                    socket.into(),
                    self.process.clone().into(),
                    user_id.into(),
                ],
            )
            .await?;
        Ok(())
    }

    /// Join step 2: the user row, unless it exists; whether this insert created it.
    pub(crate) async fn insert_user(&self, channel: &str, member: &Member) -> Result<bool> {
        let b = self.backend();
        let values = format!("VALUES ({}, {}, {})", ph(b, 1), ph(b, 2), ph(b, 3));
        let sql = match b {
            Backend::MySql => {
                format!("INSERT IGNORE INTO {USERS} (channel, user_id, user_info) {values}")
            }
            Backend::Postgres => {
                format!(
                    "INSERT INTO {USERS} (channel, user_id, user_info) {values} ON CONFLICT DO NOTHING"
                )
            }
            _ => format!("INSERT OR IGNORE INTO {USERS} (channel, user_id, user_info) {values}"),
        };
        let info: sea_orm_value::Value = member.info_text().into();
        let affected = self
            .db
            .execute_with(&sql, [channel.into(), member.user_id().into(), info])
            .await?;
        Ok(affected > 0)
    }

    /// Leave step 1: the socket row.
    pub(crate) async fn delete_socket(&self, channel: &str, socket: &str) -> Result<()> {
        let b = self.backend();
        let sql = format!(
            "DELETE FROM {SOCKETS} WHERE channel = {} AND socket_id = {}",
            ph(b, 1),
            ph(b, 2)
        );
        self.db
            .execute_with(&sql, [channel.into(), socket.into()])
            .await?;
        Ok(())
    }

    /// Leave step 2: the user row when no socket of the user is left in the channel; whether it was removed.
    pub(crate) async fn delete_user_if_gone(&self, channel: &str, user_id: &str) -> Result<bool> {
        let b = self.backend();
        let sql = format!(
            "DELETE FROM {USERS} WHERE channel = {} AND user_id = {} AND NOT EXISTS \
             (SELECT 1 FROM {SOCKETS} WHERE {SOCKETS}.channel = {} AND {SOCKETS}.user_id = {})",
            ph(b, 1),
            ph(b, 2),
            ph(b, 3),
            ph(b, 4)
        );
        let affected = self
            .db
            .execute_with(
                &sql,
                [
                    channel.into(),
                    user_id.into(),
                    channel.into(),
                    user_id.into(),
                ],
            )
            .await?;
        Ok(affected > 0)
    }

    async fn is_member(&self, channel: &str, user_id: &str) -> Result<bool> {
        let b = self.backend();
        let sql = format!(
            "SELECT 1 AS one FROM {USERS} WHERE channel = {} AND user_id = {}",
            ph(b, 1),
            ph(b, 2)
        );
        Ok(!self
            .db
            .query_with(&sql, [channel.into(), user_id.into()])
            .await?
            .is_empty())
    }

    async fn count(&self, channel: &str) -> Result<usize> {
        let b = self.backend();
        let sql = format!(
            "SELECT COUNT(*) AS n FROM {USERS} WHERE channel = {}",
            ph(b, 1)
        );
        let rows = self.db.query_with(&sql, [channel.into()]).await?;
        let n: i64 = rows
            .first()
            .and_then(|row| row.try_get("", "n").ok())
            .unwrap_or(0);
        Ok(usize::try_from(n).unwrap_or(usize::MAX))
    }

    async fn list(&self, channel: &str, max: usize) -> Result<Vec<Member>> {
        let b = self.backend();
        let sql = format!(
            "SELECT user_id, user_info FROM {USERS} WHERE channel = {} ORDER BY user_id LIMIT {max}",
            ph(b, 1)
        );
        let rows = self.db.query_with(&sql, [channel.into()]).await?;
        let mut members = Vec::with_capacity(rows.len());
        for row in rows {
            let user_id: String = row.try_get("", "user_id")?;
            let info: Option<String> = row.try_get("", "user_info")?;
            members.push(Member::from_stored(user_id, info.as_deref()));
        }
        Ok(members)
    }

    /// Remove the socket rows `rows` selects (`channel`, `socket_id`, `user_id`), then the members left without a
    /// socket.
    async fn remove_rows(
        &self,
        select: &str,
        values: Vec<sea_orm_value::Value>,
    ) -> Result<Vec<Removed>> {
        let rows = self.db.query_with(select, values).await?;
        let mut removed = Vec::new();
        for row in rows {
            let channel: String = row.try_get("", "channel")?;
            let socket: String = row.try_get("", "socket_id")?;
            let user_id: String = row.try_get("", "user_id")?;
            self.delete_socket(&channel, &socket).await?;
            if self.delete_user_if_gone(&channel, &user_id).await? {
                removed.push((channel, user_id));
            }
        }
        Ok(removed)
    }
}

impl DatabaseStore {
    /// Make this process's rows match `live`: rows no live socket holds go (a join cut off after its first step),
    /// live memberships without a row come back (the rows were swept while this process could not mark them).
    ///
    /// Each row is decided against `live` as it is at that moment: a join registers before its store call (its rows
    /// are kept), a leave is forgotten before its store call (its rows are not put back), and only memberships whose
    /// join answered are put back.
    async fn reconcile(&self, live: &Memberships, touched: u64, beat: &mut Beat) -> Result<()> {
        let stored_now = live.stored_count();
        if usize::try_from(touched).is_ok_and(|t| t < stored_now) {
            tracing::error!(
                touched,
                live = stored_now,
                "anvil: presence rows of this process were missing (swept while its heartbeat failed?); putting them \
                 back"
            );
        }
        let b = self.backend();
        let select = format!(
            "SELECT channel, socket_id, user_id FROM {SOCKETS} WHERE process = {}",
            ph(b, 1)
        );
        let rows = self
            .db
            .query_with(&select, [self.process.clone().into()])
            .await?;
        let mut stored = std::collections::HashSet::new();
        for row in rows {
            let channel: String = row.try_get("", "channel")?;
            let socket: String = row.try_get("", "socket_id")?;
            let user_id: String = row.try_get("", "user_id")?;
            if live.holds(&channel, &socket) {
                stored.insert((channel, socket));
                continue;
            }
            self.delete_socket(&channel, &socket).await?;
            if self.delete_user_if_gone(&channel, &user_id).await? {
                beat.removed.push((channel, user_id));
            }
        }
        for l in live.stored_list() {
            if stored.contains(&(l.channel.clone(), l.socket.clone())) {
                continue;
            }
            // Still there now (a leave forgets it before its own store call)?
            let Some(member) = live
                .stored(&l.channel, &l.socket)
                .filter(|now| *now == l.member)
            else {
                continue;
            };
            self.insert_socket(&l.channel, &l.socket, member.user_id())
                .await?;
            if self.insert_user(&l.channel, &member).await? {
                beat.added.push((l.channel.clone(), member));
            }
        }
        Ok(())
    }

    /// Remove members without any socket row (a leave cut off after its first step); the members removed. The
    /// delete is leave's own second step, so it races with joins and leaves exactly as they race with each other.
    async fn remove_orphans(&self) -> Result<Vec<Removed>> {
        let select = format!(
            "SELECT channel, user_id FROM {USERS} WHERE NOT EXISTS (SELECT 1 FROM {SOCKETS} WHERE \
             {SOCKETS}.channel = {USERS}.channel AND {SOCKETS}.user_id = {USERS}.user_id) ORDER BY id LIMIT \
             {SWEEP_BATCH}"
        );
        let rows = self.db.query_with(&select, []).await?;
        let mut removed = Vec::new();
        for row in rows {
            let channel: String = row.try_get("", "channel")?;
            let user_id: String = row.try_get("", "user_id")?;
            if self.delete_user_if_gone(&channel, &user_id).await? {
                removed.push((channel, user_id));
            }
        }
        Ok(removed)
    }
}

/// The value type `execute_with` binds (sea-orm's, through core's re-export).
mod sea_orm_value {
    pub(crate) use smeltery_core::db::prelude::Value;
}

impl Store for DatabaseStore {
    fn join<'a>(
        &'a self,
        channel: &'a str,
        socket: &'a str,
        member: &'a Member,
        max: usize,
    ) -> BoxFuture<'a, Result<Joined>> {
        Box::pin(async move {
            // The limit is checked before the insert: concurrent joins of new users in several processes can pass
            // it together (a soft limit, exceeded by at most the joins racing it).
            if !self.is_member(channel, member.user_id()).await?
                && self.count(channel).await? >= max
            {
                return Ok(Joined::Full);
            }
            self.insert_socket(channel, socket, member.user_id())
                .await?;
            let added = self.insert_user(channel, member).await?;
            let members = self.list(channel, max).await?;
            Ok(Joined::In { added, members })
        })
    }

    fn leave<'a>(
        &'a self,
        channel: &'a str,
        socket: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            self.delete_socket(channel, socket).await?;
            self.delete_user_if_gone(channel, user_id).await
        })
    }

    fn members<'a>(&'a self, channel: &'a str, max: usize) -> BoxFuture<'a, Result<Vec<Member>>> {
        Box::pin(self.list(channel, max))
    }

    fn heartbeat<'a>(&'a self, live: &'a Memberships) -> BoxFuture<'a, Result<Beat>> {
        Box::pin(async move {
            let b = self.backend();
            let touch = format!(
                "UPDATE {SOCKETS} SET seen_at = {} WHERE process = {}",
                now_sql(b),
                ph(b, 1)
            );
            let touched = self
                .db
                .execute_with(&touch, [self.process.clone().into()])
                .await?;
            let mut beat = Beat::default();
            #[cfg(test)]
            {
                let hook = self
                    .during_heartbeat
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some(hook) = hook {
                    hook().await;
                }
            }
            if usize::try_from(touched).ok() != Some(live.stored_count()) {
                self.reconcile(live, touched, &mut beat).await?;
            }
            let stale = i64::try_from(STALE_AFTER.as_millis()).unwrap_or(i64::MAX);
            let select = format!(
                "SELECT channel, socket_id, user_id FROM {SOCKETS} WHERE seen_at < {} - {stale} AND process <> {} \
                 ORDER BY id LIMIT {SWEEP_BATCH}",
                now_sql(b),
                ph(b, 1)
            );
            beat.removed.extend(
                self.remove_rows(&select, vec![self.process.clone().into()])
                    .await?,
            );
            beat.removed.extend(self.remove_orphans().await?);
            Ok(beat)
        })
    }

    fn clear(&self) -> BoxFuture<'_, Result<Vec<Removed>>> {
        Box::pin(async move {
            let b = self.backend();
            let select = format!(
                "SELECT channel, socket_id, user_id FROM {SOCKETS} WHERE process = {}",
                ph(b, 1)
            );
            self.remove_rows(&select, vec![self.process.clone().into()])
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::presence::{Live, migrations};
    use smeltery_core::db::migration::Schema;

    /// Two processes' stores on one SQLite file.
    async fn two() -> (tempfile::TempDir, DatabaseStore, DatabaseStore) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path()
                .join("db.sqlite")
                .display()
                .to_string()
                .replace(std::path::MAIN_SEPARATOR, "/")
        );
        let db = Db::connect(&url).await.unwrap();
        migrations::up(&Schema::new(&db)).await.unwrap();
        let a = DatabaseStore::new(db.clone(), "process-a".into());
        let b = DatabaseStore::new(db, "process-b".into());
        (dir, a, b)
    }

    const ROOM: &str = "presence-room.1";

    #[tokio::test]
    async fn joins_and_leaves_announce_once_per_user() {
        let (_dir, a, b) = two().await;
        let ada = Member::new(7).info(serde_json::json!({ "name": "Ada" }));
        let Joined::In { added, members } = a.join(ROOM, "1.1", &ada, 10).await.unwrap() else {
            panic!("in")
        };
        assert!(added);
        assert_eq!(members, vec![ada.clone()]);
        // The same user in another process: one member, no announcement.
        let Joined::In { added, members } = b.join(ROOM, "2.1", &ada, 10).await.unwrap() else {
            panic!("in")
        };
        assert!(!added);
        assert_eq!(members, vec![ada.clone()]);
        assert!(
            !a.leave(ROOM, "1.1", "7").await.unwrap(),
            "a socket is left"
        );
        assert!(b.leave(ROOM, "2.1", "7").await.unwrap(), "the last socket");
        assert!(a.members(ROOM, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_full_channel_lets_members_in_and_no_one_else() {
        let (_dir, a, b) = two().await;
        assert!(matches!(
            a.join(ROOM, "1.1", &Member::new(1), 1).await.unwrap(),
            Joined::In { .. }
        ));
        assert_eq!(
            b.join(ROOM, "2.1", &Member::new(2), 1).await.unwrap(),
            Joined::Full
        );
        assert!(matches!(
            b.join(ROOM, "2.2", &Member::new(1), 1).await.unwrap(),
            Joined::In { added: false, .. }
        ));
    }

    /// Every interleaving of one process's join (socket row, user row) with another's leave (socket row, user row)
    /// of the same user: the user row exists exactly while a socket row does, and each change is announced once.
    #[tokio::test]
    async fn interleaved_joins_and_leaves_keep_the_member_right() {
        let member = Member::new(9);
        // Steps: J1 insert socket, J2 insert user (A joins with socket 1.2); L1 delete socket, L2 delete user if
        // gone (B leaves with socket 2.1, joined before).
        let orders: [[&str; 4]; 6] = [
            ["J1", "J2", "L1", "L2"],
            ["J1", "L1", "J2", "L2"],
            ["J1", "L1", "L2", "J2"],
            ["L1", "J1", "J2", "L2"],
            ["L1", "J1", "L2", "J2"],
            ["L1", "L2", "J1", "J2"],
        ];
        for order in orders {
            let (_dir, a, b) = two().await;
            b.insert_socket(ROOM, "2.1", "9").await.unwrap();
            assert!(b.insert_user(ROOM, &member).await.unwrap());
            let (mut added, mut removed) = (0, 0);
            for step in order {
                match step {
                    "J1" => a.insert_socket(ROOM, "1.2", "9").await.unwrap(),
                    "J2" => added += usize::from(a.insert_user(ROOM, &member).await.unwrap()),
                    "L1" => b.delete_socket(ROOM, "2.1").await.unwrap(),
                    _ => removed += usize::from(b.delete_user_if_gone(ROOM, "9").await.unwrap()),
                }
            }
            // A's socket is in: the user is a member, whatever the order.
            assert_eq!(
                a.members(ROOM, 10).await.unwrap(),
                vec![member.clone()],
                "{order:?}"
            );
            // Announcements balance: a removal is followed by a new addition.
            assert_eq!(added, removed, "{order:?}");
            assert!(added <= 1, "{order:?}");
        }
    }

    #[tokio::test]
    async fn a_gone_process_is_swept_and_its_members_announced() {
        let (_dir, a, b) = two().await;
        a.join(ROOM, "1.1", &Member::new(7), 10).await.unwrap();
        b.join(ROOM, "2.1", &Member::new(8), 10).await.unwrap();
        b.join(ROOM, "2.2", &Member::new(7), 10).await.unwrap();
        // Process B stops marking its rows (it crashed long ago).
        a.db.execute(
            "UPDATE presence_sockets SET seen_at = seen_at - 100000 WHERE process = 'process-b'",
        )
        .await
        .unwrap();
        let live = Memberships::of(&[Live {
            channel: ROOM.into(),
            socket: "1.1".into(),
            member: Member::new(7),
        }]);
        let beat = a.heartbeat(&live).await.unwrap();
        assert_eq!(
            beat.removed,
            vec![(ROOM.to_owned(), "8".to_owned())],
            "7 still has a socket in A"
        );
        assert_eq!(a.members(ROOM, 10).await.unwrap(), vec![Member::new(7)]);
        // A's own rows are fresh; at shutdown it clears them.
        let beat = a.heartbeat(&live).await.unwrap();
        assert!(beat.removed.is_empty() && beat.added.is_empty());
        assert_eq!(
            a.clear().await.unwrap(),
            vec![(ROOM.to_owned(), "7".to_owned())]
        );
        assert!(a.members(ROOM, 10).await.unwrap().is_empty());
    }
}

#[cfg(test)]
mod cut_off {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::presence::{Live, migrations};
    use smeltery_core::db::migration::Schema;

    const ROOM: &str = "presence-room.1";

    async fn store() -> (tempfile::TempDir, DatabaseStore) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path()
                .join("db.sqlite")
                .display()
                .to_string()
                .replace(std::path::MAIN_SEPARATOR, "/")
        );
        let db = Db::connect(&url).await.unwrap();
        migrations::up(&Schema::new(&db)).await.unwrap();
        (dir, DatabaseStore::new(db, "process-a".into()))
    }

    #[tokio::test]
    async fn a_join_cut_after_its_socket_row_is_undone_by_the_heartbeat() {
        let (_dir, a) = store().await;
        // The socket's join stopped after step 1 (a timeout); the socket is not a member of anything.
        a.insert_socket(ROOM, "1.1", "7").await.unwrap();
        let beat = a.heartbeat(&Memberships::default()).await.unwrap();
        assert!(
            beat.removed.is_empty(),
            "never announced, nothing to announce"
        );
        // The user's real sockets come and go: the leave of the last one removes the member.
        let member = Member::new(7);
        a.join(ROOM, "1.2", &member, 10).await.unwrap();
        assert!(
            a.leave(ROOM, "1.2", "7").await.unwrap(),
            "no ghost socket holds the member"
        );
    }

    #[tokio::test]
    async fn a_leave_cut_after_its_socket_row_is_finished_by_the_heartbeat() {
        let (_dir, a) = store().await;
        let member = Member::new(7);
        a.join(ROOM, "1.1", &member, 10).await.unwrap();
        // The leave stopped after step 1: the member row is left without a socket.
        a.delete_socket(ROOM, "1.1").await.unwrap();
        let beat = a.heartbeat(&Memberships::default()).await.unwrap();
        assert_eq!(beat.removed, vec![(ROOM.to_owned(), "7".to_owned())]);
        assert!(a.members(ROOM, 10).await.unwrap().is_empty());
    }

    /// A row of another socket, held by no membership: the touch counts it, so the heartbeat reconciles.
    async fn stray_row(a: &DatabaseStore) {
        a.join(ROOM, "9.9", &Member::new(99), 10).await.unwrap();
    }

    /// PR-1: a join that registers and writes its rows while a heartbeat runs (after the touch, before the
    /// reconcile reads the rows) keeps them; the stray row goes.
    #[tokio::test]
    async fn a_join_racing_the_heartbeat_is_kept() {
        let (_dir, a) = store().await;
        let ada = Member::new(7);
        a.join(ROOM, "1.1", &ada, 10).await.unwrap();
        let live = std::sync::Arc::new(Memberships::of(&[Live {
            channel: ROOM.into(),
            socket: "1.1".into(),
            member: ada.clone(),
        }]));
        stray_row(&a).await;
        let racing = DatabaseStore::new(a.db.clone(), "process-a".into());
        let during = std::sync::Arc::clone(&live);
        *a.during_heartbeat.lock().unwrap() = Some(Box::new(move || {
            Box::pin(async move {
                // The join of socket 1.2 (user 8): registered first, then its store call.
                let bob = Member::new(8);
                during.joining(ROOM, "1.2", &bob);
                racing.join(ROOM, "1.2", &bob, 10).await.unwrap();
            })
        }));
        let beat = a.heartbeat(&live).await.unwrap();
        assert!(
            !beat.removed.contains(&(ROOM.to_owned(), "8".to_owned())),
            "the joining member is not announced as gone: {:?}",
            beat.removed
        );
        assert_eq!(beat.removed, vec![(ROOM.to_owned(), "99".to_owned())]);
        assert_eq!(
            a.members(ROOM, 10).await.unwrap(),
            vec![Member::new(7), Member::new(8)]
        );
    }

    /// PR-1: a leave that forgets its membership and deletes its rows while a heartbeat runs is not put back.
    #[tokio::test]
    async fn a_leave_racing_the_heartbeat_is_not_put_back() {
        let (_dir, a) = store().await;
        let ada = Member::new(7);
        a.join(ROOM, "1.1", &ada, 10).await.unwrap();
        let live = std::sync::Arc::new(Memberships::of(&[Live {
            channel: ROOM.into(),
            socket: "1.1".into(),
            member: ada.clone(),
        }]));
        stray_row(&a).await;
        let racing = DatabaseStore::new(a.db.clone(), "process-a".into());
        let during = std::sync::Arc::clone(&live);
        *a.during_heartbeat.lock().unwrap() = Some(Box::new(move || {
            Box::pin(async move {
                // The leave of socket 1.1: forgotten first, then its store call.
                during.forget(ROOM, "1.1");
                assert!(racing.leave(ROOM, "1.1", "7").await.unwrap());
            })
        }));
        let beat = a.heartbeat(&live).await.unwrap();
        assert!(beat.added.is_empty(), "no ghost: {:?}", beat.added);
        assert!(a.members(ROOM, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rows_swept_while_alive_come_back() {
        let (_dir, a) = store().await;
        let member = Member::new(7).info(serde_json::json!({ "name": "Ada" }));
        a.join(ROOM, "1.1", &member, 10).await.unwrap();
        // Another process swept this one (its heartbeat failed for 90 s): its rows are gone, its socket is not.
        a.db.execute("DELETE FROM presence_sockets").await.unwrap();
        a.db.execute("DELETE FROM presence_users").await.unwrap();
        let live = Memberships::of(&[Live {
            channel: ROOM.into(),
            socket: "1.1".into(),
            member: member.clone(),
        }]);
        let beat = a.heartbeat(&live).await.unwrap();
        assert_eq!(beat.added, vec![(ROOM.to_owned(), member.clone())]);
        assert_eq!(a.members(ROOM, 10).await.unwrap(), vec![member]);
        assert!(a.leave(ROOM, "1.1", "7").await.unwrap());
    }
}
