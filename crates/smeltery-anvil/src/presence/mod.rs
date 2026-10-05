//! Presence channels: who is in a `presence-` channel, across the app's processes (D-414).
//!
//! Members are keyed by `user_id`: one user with three tabs is one member with three sockets. A user's first socket
//! in a channel announces `pusher_internal:member_added`, its last socket's leaving `pusher_internal:member_removed`.
//! The store follows the PubSub driver of the process (ANVIL.md §10 A3): in memory with `local`, the
//! `presence_sockets` / `presence_users` tables with `database`, Redis hashes and sets with `redis`.

pub(crate) mod database;
pub(crate) mod memory;
#[cfg(feature = "redis")]
pub(crate) mod redis;

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use smeltery_core::{BoxFuture, Error, Result};

/// The longest `user_id` of a member, in bytes.
pub const MAX_USER_ID: usize = 128;

/// How often a serving process marks its presence rows alive.
pub(crate) const HEARTBEAT: Duration = Duration::from_secs(30);

/// A process unseen for this long is gone: its members are removed.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(90);

/// The longest one store call may take.
pub(crate) const STORE_TIMEOUT: Duration = Duration::from_secs(5);

/// A member of a presence channel: the user's id (a string on the wire) and the display data every member of the
/// channel receives (`user_info`).
///
/// `user_info` reaches every member of the channel: give display fields only (a name, an avatar URL), never an email
/// address or anything private.
///
/// ```
/// use serde_json::json;
/// use smeltery::anvil::Member;
///
/// let member = Member::new(7).info(json!({ "name": "Ada" }));
/// assert_eq!(member.user_id(), "7");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    user_id: String,
    user_info: Option<Value>,
}

impl Member {
    /// A member with this user id (a number becomes its decimal string). A user id must be 1 to 128
    /// bytes without control characters: the auth endpoint answers 500 (and signs nothing) for a member whose id is
    /// not.
    pub fn new(user_id: impl std::fmt::Display) -> Self {
        Self {
            user_id: user_id.to_string(),
            user_info: None,
        }
    }

    /// The display data the other members receive (`user_info`).
    #[must_use]
    pub fn info(mut self, info: Value) -> Self {
        self.user_info = Some(info);
        self
    }

    /// The user id.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The display data.
    pub fn user_info(&self) -> Option<&Value> {
        self.user_info.as_ref()
    }

    /// The `channel_data` a presence subscription carries: `{"user_id":"7","user_info":{…}}`.
    pub(crate) fn channel_data(&self) -> String {
        let mut data = serde_json::json!({ "user_id": self.user_id });
        if let (Some(info), Some(object)) = (&self.user_info, data.as_object_mut()) {
            object.insert("user_info".into(), info.clone());
        }
        data.to_string()
    }

    /// From a subscription's `channel_data` (`user_id` a string or a number, `user_info` optional); `None` for any
    /// other shape.
    pub(crate) fn from_channel_data(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let object = value.as_object()?;
        let user_id = match object.get("user_id")? {
            Value::String(id) => id.clone(),
            Value::Number(id) => id.to_string(),
            _ => return None,
        };
        let member = Self {
            user_id,
            user_info: object.get("user_info").filter(|v| !v.is_null()).cloned(),
        };
        member.valid_id().then_some(member)
    }

    /// Whether the user id is 1 to [`MAX_USER_ID`] bytes without control characters.
    pub(crate) fn valid_id(&self) -> bool {
        !self.user_id.is_empty()
            && self.user_id.len() <= MAX_USER_ID
            && !self.user_id.chars().any(char::is_control)
    }

    /// The `user_info` as stored (JSON text), `None` without one.
    pub(crate) fn info_text(&self) -> Option<String> {
        self.user_info.as_ref().map(Value::to_string)
    }

    /// A member from stored columns.
    pub(crate) fn from_stored(user_id: String, info: Option<&str>) -> Self {
        Self {
            user_id,
            user_info: info.and_then(|text| serde_json::from_str(text).ok()),
        }
    }
}

/// What joining a presence channel did.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Joined {
    /// The socket is in; `added` when it is the user's first socket in the channel (announce `member_added`), and
    /// the members now (at most the channel's limit).
    In { added: bool, members: Vec<Member> },
    /// The channel holds its most members and the user is not one of them.
    Full,
}

/// A member a process removed: the channel and the user (announce `member_removed`).
pub(crate) type Removed = (String, String);

/// A presence membership a process knows its socket has.
#[derive(Debug, Clone)]
pub(crate) struct Live {
    pub(crate) channel: String,
    pub(crate) socket: String,
    pub(crate) member: Member,
}

/// One membership of a socket of this process.
#[derive(Debug, Clone)]
struct Entry {
    member: Member,
    /// The store's join answered: the rows exist (unless a sweep took them).
    stored: bool,
}

/// The presence memberships of this process's sockets: (channel, socket) → member. A join registers here before its
/// store call and is marked stored when the call answers; a leave is forgotten here before its store call. The
/// heartbeat decides each row against this map as it is at that moment (never a copy made earlier), so a join or a
/// leave running during a heartbeat is never undone or put back by it.
#[derive(Debug, Default)]
pub(crate) struct Memberships(Mutex<HashMap<(String, String), Entry>>);

impl Memberships {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Entry>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A join starts (before its store call).
    pub(crate) fn joining(&self, channel: &str, socket: &str, member: &Member) {
        self.map().insert(
            (channel.to_owned(), socket.to_owned()),
            Entry {
                member: member.clone(),
                stored: false,
            },
        );
    }

    /// The join's store call answered.
    pub(crate) fn joined(&self, channel: &str, socket: &str) {
        if let Some(entry) = self.map().get_mut(&(channel.to_owned(), socket.to_owned())) {
            entry.stored = true;
        }
    }

    /// Forget a membership (a leave, before its store call; a failed join).
    pub(crate) fn forget(&self, channel: &str, socket: &str) {
        self.map().remove(&(channel.to_owned(), socket.to_owned()));
    }

    /// Whether `socket` is in `channel` now (joined, or joining).
    pub(crate) fn holds(&self, channel: &str, socket: &str) -> bool {
        self.map()
            .contains_key(&(channel.to_owned(), socket.to_owned()))
    }

    /// The member of a stored membership, now.
    pub(crate) fn stored(&self, channel: &str, socket: &str) -> Option<Member> {
        self.map()
            .get(&(channel.to_owned(), socket.to_owned()))
            .filter(|e| e.stored)
            .map(|e| e.member.clone())
    }

    /// How many memberships are stored now.
    pub(crate) fn stored_count(&self) -> usize {
        self.map().values().filter(|e| e.stored).count()
    }

    /// The stored memberships now.
    pub(crate) fn stored_list(&self) -> Vec<Live> {
        self.map()
            .iter()
            .filter(|(_, e)| e.stored)
            .map(|((channel, socket), e)| Live {
                channel: channel.clone(),
                socket: socket.clone(),
                member: e.member.clone(),
            })
            .collect()
    }

    /// The channels `socket` is in, with its user id in each.
    pub(crate) fn of_socket(&self, socket: &str) -> Vec<(String, String)> {
        self.map()
            .iter()
            .filter(|((_, s), _)| s == socket)
            .map(|((channel, _), e)| (channel.clone(), e.member.user_id().to_owned()))
            .collect()
    }

    /// Memberships whose joins answered already (tests).
    #[cfg(test)]
    pub(crate) fn of(live: &[Live]) -> Self {
        let memberships = Self::default();
        for l in live {
            memberships.joining(&l.channel, &l.socket, &l.member);
            memberships.joined(&l.channel, &l.socket);
        }
        memberships
    }
}

/// What a heartbeat changed: members that left (gone processes, rows nobody holds) and members this process put
/// back (its rows had been lost).
#[derive(Debug, Default)]
pub(crate) struct Beat {
    pub(crate) removed: Vec<Removed>,
    pub(crate) added: Vec<(String, Member)>,
}

/// Where the members of presence channels live.
pub(crate) trait Store: Send + Sync + 'static {
    /// Add `socket` as `member` of `channel` (at most `max` distinct users).
    fn join<'a>(
        &'a self,
        channel: &'a str,
        socket: &'a str,
        member: &'a Member,
        max: usize,
    ) -> BoxFuture<'a, Result<Joined>>;

    /// Remove `socket` (of user `user_id`) from `channel`; whether that was the user's last socket there.
    fn leave<'a>(
        &'a self,
        channel: &'a str,
        socket: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<bool>>;

    /// The members of `channel` (at most `max`).
    fn members<'a>(&'a self, channel: &'a str, max: usize) -> BoxFuture<'a, Result<Vec<Member>>>;

    /// Mark this process's sockets alive and make its rows match `live` (the memberships its sockets have, read
    /// as they are when each row is decided: stored rows lost are put back, rows no socket holds go), and remove the
    /// sockets of processes unseen for [`STALE_AFTER`]; the members that left or came back.
    fn heartbeat<'a>(&'a self, live: &'a Memberships) -> BoxFuture<'a, Result<Beat>> {
        let _ = live;
        Box::pin(async { Ok(Beat::default()) })
    }

    /// Remove every socket of this process (at shutdown); the members that left with them.
    fn clear(&self) -> BoxFuture<'_, Result<Vec<Removed>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// A store call within [`STORE_TIMEOUT`] (outside a runtime, as in `TestSocket`, only the in-memory store answers,
/// at once: no timer).
pub(crate) async fn timed<T>(call: BoxFuture<'_, Result<T>>) -> Result<T> {
    if tokio::runtime::Handle::try_current().is_err() {
        return call.await;
    }
    tokio::time::timeout(STORE_TIMEOUT, call)
        .await
        .map_err(|_| Error::internal("the presence store did not answer in time"))?
}

/// The presence tables of the `database` store, for the app's migration (`presence_sockets`: one row per socket in
/// a channel; `presence_users`: one row per member).
///
/// ```
/// use smeltery::Result;
/// use smeltery::db::migration::{Migration, Schema};
///
/// pub struct CreatePresenceTables;
///
/// impl Migration for CreatePresenceTables {
///     fn name(&self) -> &'static str {
///         "2026_10_05_000001_create_presence_tables"
///     }
///
///     async fn up(&self, schema: &Schema) -> Result<()> {
///         smeltery::anvil::presence_migrations::up(schema).await
///     }
///
///     async fn down(&self, schema: &Schema) -> Result<()> {
///         smeltery::anvil::presence_migrations::down(schema).await
///     }
/// }
/// ```
pub mod migrations {
    use smeltery_core::Result;
    use smeltery_core::db::migration::Schema;

    pub(crate) const SOCKETS: &str = "presence_sockets";
    pub(crate) const USERS: &str = "presence_users";

    /// Create `presence_sockets` and `presence_users`.
    ///
    /// # Errors
    /// A table exists already, or a statement fails.
    pub async fn up(schema: &Schema) -> Result<()> {
        schema
            .create(SOCKETS, |t| {
                t.id();
                t.string_len("channel", 164);
                t.string_len("socket_id", 32);
                t.string_len("process", 64).index();
                t.string_len("user_id", 128);
                t.big_integer("seen_at").index();
            })
            .await?;
        schema
            .raw("CREATE UNIQUE INDEX presence_sockets_channel_socket ON presence_sockets (channel, socket_id)")
            .await?;
        schema
            .raw(
                "CREATE INDEX presence_sockets_channel_user ON presence_sockets (channel, user_id)",
            )
            .await?;
        schema
            .create(USERS, |t| {
                t.id();
                t.string_len("channel", 164);
                t.string_len("user_id", 128);
                t.text("user_info").nullable();
            })
            .await?;
        schema
            .raw("CREATE UNIQUE INDEX presence_users_channel_user ON presence_users (channel, user_id)")
            .await?;
        Ok(())
    }

    /// Drop both tables.
    ///
    /// # Errors
    /// A statement fails.
    pub async fn down(schema: &Schema) -> Result<()> {
        schema.drop_if_exists(USERS).await?;
        schema.drop_if_exists(SOCKETS).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_data_round_trips_and_ids_become_strings() {
        let member = Member::new(7).info(serde_json::json!({ "name": "Ada" }));
        let data = member.channel_data();
        assert_eq!(data, r#"{"user_id":"7","user_info":{"name":"Ada"}}"#);
        assert_eq!(Member::from_channel_data(&data), Some(member));
        assert_eq!(
            Member::from_channel_data(r#"{"user_id":9}"#),
            Some(Member::new(9))
        );
        for bad in [
            "",
            "[]",
            r#"{"user_info":{}}"#,
            r#"{"user_id":true}"#,
            r#"{"user_id":""}"#,
        ] {
            assert_eq!(Member::from_channel_data(bad), None, "{bad}");
        }
        let long = format!(r#"{{"user_id":"{}"}}"#, "x".repeat(MAX_USER_ID + 1));
        assert_eq!(Member::from_channel_data(&long), None);
    }
}
