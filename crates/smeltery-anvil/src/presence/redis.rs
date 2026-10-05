//! The `redis` presence store (PubSub driver `redis`, feature `redis`): per channel a hash of members
//! (`user_id` → `user_info`) and per member a set of sockets; per process a set of its entries and a sorted set of
//! processes by their last heartbeat. Joins and leaves are Lua scripts (atomic), with the same rules as the database
//! store (D-414): the script that created the member announces `member_added`, the one that removed it
//! `member_removed`. A process unseen for 90 s is swept by the others.

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use smeltery_core::{BoxFuture, Error, Result};
use tokio::sync::OnceCell;

use super::{Beat, Joined, Member, Memberships, Removed, STALE_AFTER, STORE_TIMEOUT, Store};

/// Separates the parts of a process entry (never in a channel name, socket id or user id: control characters are
/// refused in all three).
const SEP: char = '\u{1f}';

const JOIN: &str = r"
if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 and redis.call('HLEN', KEYS[1]) >= tonumber(ARGV[4]) then
  return -1
end
redis.call('SADD', KEYS[2], ARGV[3])
redis.call('SADD', KEYS[3], ARGV[5])
return redis.call('HSETNX', KEYS[1], ARGV[1], ARGV[2])
";

const LEAVE: &str = r"
redis.call('SREM', KEYS[2], ARGV[2])
redis.call('SREM', KEYS[3], ARGV[3])
if redis.call('SCARD', KEYS[2]) == 0 then
  return redis.call('HDEL', KEYS[1], ARGV[1])
end
return 0
";

/// The Redis store of one process.
pub(crate) struct RedisStore {
    client: redis::Client,
    conn: OnceCell<ConnectionManager>,
    prefix: String,
    process: String,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl RedisStore {
    /// A store on the Redis server of `url`, keys under `<prefix>anvil:presence:`.
    pub(crate) fn new(url: &str, prefix: &str, process: String) -> Result<Self> {
        if url.starts_with("rediss:") && rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let client = redis::Client::open(url)
            .map_err(|e| Error::internal(format!("REDIS_URL is not a valid Redis URL: {e}")))?;
        Ok(Self {
            client,
            conn: OnceCell::new(),
            prefix: format!("{prefix}anvil:presence:"),
            process,
        })
    }

    async fn conn(&self) -> Result<ConnectionManager> {
        let conn = self
            .conn
            .get_or_try_init(|| async {
                let config = ConnectionManagerConfig::new()
                    .set_connection_timeout(Some(STORE_TIMEOUT))
                    .set_response_timeout(Some(STORE_TIMEOUT))
                    .set_number_of_retries(1);
                ConnectionManager::new_with_config(self.client.clone(), config)
                    .await
                    .map_err(Error::other)
            })
            .await?;
        Ok(conn.clone())
    }

    fn users_key(&self, channel: &str) -> String {
        format!("{}u:{channel}", self.prefix)
    }

    fn sockets_key(&self, channel: &str, user_id: &str) -> String {
        format!("{}s:{channel}{SEP}{user_id}", self.prefix)
    }

    fn process_key(&self, process: &str) -> String {
        format!("{}p:{process}", self.prefix)
    }

    fn processes_key(&self) -> String {
        format!("{}processes", self.prefix)
    }

    fn entry(channel: &str, socket: &str, user_id: &str) -> String {
        format!("{channel}{SEP}{socket}{SEP}{user_id}")
    }

    async fn leave_as(
        &self,
        process: &str,
        channel: &str,
        socket: &str,
        user_id: &str,
    ) -> Result<bool> {
        let mut conn = self.conn().await?;
        // EVAL (the script is short; no script cache to manage).
        let removed: i64 = redis::cmd("EVAL")
            .arg(LEAVE)
            .arg(3)
            .arg(self.users_key(channel))
            .arg(self.sockets_key(channel, user_id))
            .arg(self.process_key(process))
            .arg(user_id)
            .arg(socket)
            .arg(Self::entry(channel, socket, user_id))
            .query_async(&mut conn)
            .await
            .map_err(Error::other)?;
        Ok(removed > 0)
    }

    /// Remove every entry of `process`; the members that left.
    async fn sweep_process(&self, process: &str) -> Result<Vec<Removed>> {
        let mut conn = self.conn().await?;
        let entries: Vec<String> = redis::cmd("SMEMBERS")
            .arg(self.process_key(process))
            .query_async(&mut conn)
            .await
            .map_err(Error::other)?;
        let mut removed = Vec::new();
        for entry in entries {
            let mut parts = entry.split(SEP);
            let (Some(channel), Some(socket), Some(user_id)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if self.leave_as(process, channel, socket, user_id).await? {
                removed.push((channel.to_owned(), user_id.to_owned()));
            }
        }
        let _: i64 = redis::cmd("DEL")
            .arg(self.process_key(process))
            .query_async(&mut conn)
            .await
            .map_err(Error::other)?;
        let _: i64 = redis::cmd("ZREM")
            .arg(self.processes_key())
            .arg(process)
            .query_async(&mut conn)
            .await
            .map_err(Error::other)?;
        Ok(removed)
    }
}

impl Store for RedisStore {
    fn join<'a>(
        &'a self,
        channel: &'a str,
        socket: &'a str,
        member: &'a Member,
        max: usize,
    ) -> BoxFuture<'a, Result<Joined>> {
        Box::pin(async move {
            let mut conn = self.conn().await?;
            let _: i64 = redis::cmd("ZADD")
                .arg(self.processes_key())
                .arg(now_ms())
                .arg(&self.process)
                .query_async(&mut conn)
                .await
                .map_err(Error::other)?;
            let created: i64 = redis::cmd("EVAL")
                .arg(JOIN)
                .arg(3)
                .arg(self.users_key(channel))
                .arg(self.sockets_key(channel, member.user_id()))
                .arg(self.process_key(&self.process))
                .arg(member.user_id())
                .arg(member.info_text().unwrap_or_else(|| "null".to_owned()))
                .arg(socket)
                .arg(max)
                .arg(Self::entry(channel, socket, member.user_id()))
                .query_async(&mut conn)
                .await
                .map_err(Error::other)?;
            if created < 0 {
                return Ok(Joined::Full);
            }
            let members = self.members(channel, max).await?;
            Ok(Joined::In {
                added: created > 0,
                members,
            })
        })
    }

    fn leave<'a>(
        &'a self,
        channel: &'a str,
        socket: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.leave_as(&self.process, channel, socket, user_id))
    }

    fn members<'a>(&'a self, channel: &'a str, max: usize) -> BoxFuture<'a, Result<Vec<Member>>> {
        Box::pin(async move {
            let mut conn = self.conn().await?;
            let pairs: Vec<(String, String)> = redis::cmd("HGETALL")
                .arg(self.users_key(channel))
                .query_async(&mut conn)
                .await
                .map_err(Error::other)?;
            let mut members: Vec<Member> = pairs
                .into_iter()
                .map(|(user_id, info)| Member::from_stored(user_id, Some(info.as_str())))
                .collect();
            members.sort_by(|a, b| a.user_id().cmp(b.user_id()));
            members.truncate(max);
            Ok(members)
        })
    }

    fn heartbeat<'a>(&'a self, live: &'a Memberships) -> BoxFuture<'a, Result<Beat>> {
        Box::pin(async move {
            let mut conn = self.conn().await?;
            let mut beat = Beat::default();
            // This process's entries must match its live memberships: put back what a sweep took while its heartbeat
            // failed, remove what no socket holds (a join whose answer was lost).
            let entries: Vec<String> = redis::cmd("SMEMBERS")
                .arg(self.process_key(&self.process))
                .query_async(&mut conn)
                .await
                .map_err(Error::other)?;
            // Each entry is decided against the memberships as they are at that moment (a join registers before its
            // store call, a leave is forgotten before its own), never a copy made earlier.
            let stored_now = live.stored_count();
            if entries.len() != stored_now {
                if entries.len() < stored_now {
                    tracing::error!(
                        stored = entries.len(),
                        live = stored_now,
                        "anvil: presence entries of this process were missing (swept while its heartbeat failed?); \
                         putting them back"
                    );
                }
                for entry in &entries {
                    let mut parts = entry.split(SEP);
                    let (Some(channel), Some(socket), Some(user_id)) =
                        (parts.next(), parts.next(), parts.next())
                    else {
                        continue;
                    };
                    if !live.holds(channel, socket)
                        && self
                            .leave_as(&self.process, channel, socket, user_id)
                            .await?
                    {
                        beat.removed.push((channel.to_owned(), user_id.to_owned()));
                    }
                }
                for l in live.stored_list() {
                    let entry = Self::entry(&l.channel, &l.socket, l.member.user_id());
                    if entries.contains(&entry) {
                        continue;
                    }
                    // Still there now (a leave forgets it before its own store call)?
                    let Some(member) = live
                        .stored(&l.channel, &l.socket)
                        .filter(|now| *now == l.member)
                    else {
                        continue;
                    };
                    if let Joined::In { added: true, .. } = self
                        .join(&l.channel, &l.socket, &member, usize::MAX)
                        .await?
                    {
                        beat.added.push((l.channel.clone(), member));
                    }
                }
            }
            let now = now_ms();
            let _: i64 = redis::cmd("ZADD")
                .arg(self.processes_key())
                .arg(now)
                .arg(&self.process)
                .query_async(&mut conn)
                .await
                .map_err(Error::other)?;
            let stale = i64::try_from(STALE_AFTER.as_millis()).unwrap_or(i64::MAX);
            let dead: Vec<String> = redis::cmd("ZRANGEBYSCORE")
                .arg(self.processes_key())
                .arg("-inf")
                .arg(now.saturating_sub(stale))
                .query_async(&mut conn)
                .await
                .map_err(Error::other)?;
            for process in dead.iter().filter(|p| **p != self.process) {
                beat.removed.extend(self.sweep_process(process).await?);
            }
            Ok(beat)
        })
    }

    fn clear(&self) -> BoxFuture<'_, Result<Vec<Removed>>> {
        Box::pin(self.sweep_process(&self.process))
    }
}
