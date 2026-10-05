//! The Redis queue driver (feature `redis`): jobs on the Redis server of `REDIS_URL`, shared by every process
//! that uses the same server and `QUEUE_PREFIX`.
//!
//! Key layout, with `P` = `<QUEUE_PREFIX>{<queue>}:` (the queue is `default`; `{default}` is a hash tag, so every
//! key of the queue is in one hash slot):
//!
//! | Key | Type | Holds |
//! |---|---|---|
//! | `P id` | string | the last job id (`INCR`) |
//! | `P job:<id>` | hash | `job`, `payload`, `attempts`, `available_at`, `created_at`, `reserved_at` |
//! | `P ready` | sorted set | waiting jobs (due and delayed), score `available_at` (Unix ms) |
//! | `P reserved` | sorted set | jobs a worker holds, score the reservation time (Unix ms) |
//! | `P dead_id` | string | the last dead letter id (`INCR`) |
//! | `P dead:<id>` | hash | `job`, `payload`, `error`, `attempts`, `failed_at` |
//! | `P dead` | sorted set | dead letters, score their id |
//!
//! Members are ids padded to 20 digits, so members with the same score sort by id: the oldest available job,
//! then the lowest id, is reserved first, as with the database driver.
//!
//! Every operation is one Lua script (`EVALSHA`, loaded again with `SCRIPT LOAD` when the server answers
//! `NOSCRIPT`), which Redis runs atomically: a job is reserved by exactly one worker in any number of processes, and
//! a job is in exactly one of ready / reserved / dead letters. A reservation older than the stale cut-off
//! (`release_stale`) goes back to `ready` with its attempt counted, as in the database driver. `delete`, `retry`,
//! `release` and `dead_letter` act only while the caller's reservation (`reserved_at` + `attempts`) is the one held,
//! so a stale worker's outcome never lands on another worker's reservation. A script with nothing
//! to return answers `nil`, never `false` (a RESP3 connection would receive `false` as a boolean, not as a null). A
//! new id is never one whose job or dead letter still exists (the id counters can fall behind after Redis loses
//! recent writes).
//!
//! One multiplexed connection through redis's `ConnectionManager` (it reconnects by itself), opened on first use;
//! every call has the `WATCHFIRE_STORE_TIMEOUT` budget (here and in [`super::Queue::timed`]).

use std::time::Duration;

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{FromRedisValue, Script, ScriptInvocation};
use smeltery_core::BoxFuture;
use tokio::sync::OnceCell;

use super::{
    DeadLetter, Driver, JobId, QResult, QueueStats, REDIS_QUEUE as QUEUE, Reserved,
    redis_key_base as key_base,
};
use crate::error::StoreError;

/// Helpers defined once for the scripts below: `pad(n)` (`n` padded to 20 digits), `fresh(counter, prefix)`
/// (the next id from `counter` whose `prefix .. pad(id)` key does not exist: a counter that fell behind, after Redis
/// lost recent writes, never hands out the id of a job or dead letter that still exists) and
/// `held(k, reserved, m, at, attempts)` (member `m` is reserved and its job hash `k` still has the reservation
/// `at` / `attempts` the caller holds).
const HELPERS: &str =
    "local function pad(n) local s = tostring(n) return string.rep('0', 20 - #s) .. s end
local function fresh(counter, prefix)
  local id = redis.call('incr', counter)
  while redis.call('exists', prefix .. pad(id)) == 1 do id = redis.call('incr', counter) end
  return id
end
local function held(k, reserved, m, at, attempts)
  if not redis.call('zscore', reserved, m) then return false end
  local f = redis.call('hmget', k, 'reserved_at', 'attempts')
  return f[1] == at and f[2] == attempts
end
";

/// KEYS: id, ready. ARGV: job key prefix, job, payload, available_at, now. Returns the new id.
const PUSH: &str = "local id = fresh(KEYS[1], ARGV[1])
local m = pad(id)
redis.call('hset', ARGV[1] .. m, 'job', ARGV[2], 'payload', ARGV[3], 'attempts', 0, 'available_at', ARGV[4], 'created_at', ARGV[5])
redis.call('zadd', KEYS[2], ARGV[4], m)
return id";

/// KEYS: ready, reserved. ARGV: job key prefix, now, max payload bytes. Takes the first due job (lowest
/// `available_at`, then id), counts the attempt and marks it reserved at `now`. Returns
/// `{member, job, payload, attempts, size}` or nil; a payload over the limit is not read (`''`, its size given).
///
/// A member without its hash (an orphan: only after Redis lost writes) is dropped and the next one tried; at most 16
/// per call keep one call short, and the next call goes on where this one stopped.
const RESERVE: &str = "for _ = 1, 16 do
  local due = redis.call('zrangebyscore', KEYS[1], '-inf', ARGV[2], 'LIMIT', 0, 1)
  local m = due[1]
  if not m then return nil end
  redis.call('zrem', KEYS[1], m)
  local k = ARGV[1] .. m
  local job = redis.call('hget', k, 'job')
  if job then
    local attempts = redis.call('hincrby', k, 'attempts', 1)
    redis.call('hset', k, 'reserved_at', ARGV[2])
    redis.call('zadd', KEYS[2], ARGV[2], m)
    local size = redis.call('hstrlen', k, 'payload')
    local payload = ''
    if size <= tonumber(ARGV[3]) then
      payload = redis.call('hget', k, 'payload')
      size = 0
    end
    return {m, job, payload, attempts, size}
  end
end
return nil";

/// KEYS: ready, reserved. ARGV: job key prefix, member, reserved_at, attempts. Deletes the job while the caller's
/// reservation is the one held. Returns 1, or 0 when it is not.
const DELETE: &str = "local k = ARGV[1] .. ARGV[2]
if not held(k, KEYS[2], ARGV[2], ARGV[3], ARGV[4]) then return 0 end
redis.call('zrem', KEYS[1], ARGV[2])
redis.call('zrem', KEYS[2], ARGV[2])
redis.call('del', k)
return 1";

/// KEYS: ready, reserved. ARGV: job key prefix, member, available_at, reserved_at, attempts. Unreserves the job and
/// makes it available at `available_at` (the attempt stays counted); nothing (0) when the caller's reservation is not
/// the one held.
const RETRY: &str = "local k = ARGV[1] .. ARGV[2]
if not held(k, KEYS[2], ARGV[2], ARGV[4], ARGV[5]) then return 0 end
redis.call('hset', k, 'available_at', ARGV[3])
redis.call('zrem', KEYS[2], ARGV[2])
redis.call('zadd', KEYS[1], ARGV[3], ARGV[2])
return 1";

/// KEYS: ready, reserved. ARGV: job key prefix, member, reserved_at, attempts. Gives a reserved job back without
/// counting the attempt; nothing (0) when the caller's reservation is not the one held.
const RELEASE: &str = "local k = ARGV[1] .. ARGV[2]
if not held(k, KEYS[2], ARGV[2], ARGV[3], ARGV[4]) then return 0 end
local at = redis.call('hget', k, 'available_at')
if not at then return 0 end
redis.call('zrem', KEYS[2], ARGV[2])
redis.call('hincrby', k, 'attempts', -1)
redis.call('zadd', KEYS[1], at, ARGV[2])
return 1";

/// KEYS: ready, reserved, dead id, dead. ARGV: job key prefix, member, dead key prefix, error, now, reserved_at,
/// attempts. Adds the dead letter (the job's stored name, payload and attempts) and removes the job in one step,
/// while the caller's reservation is the one held. Returns the dead letter's id, or 0 when the reservation is not
/// held.
const DEAD_LETTER: &str = "local k = ARGV[1] .. ARGV[2]
if not held(k, KEYS[2], ARGV[2], ARGV[6], ARGV[7]) then return 0 end
local f = redis.call('hmget', k, 'job', 'payload', 'attempts')
local id = fresh(KEYS[3], ARGV[3])
local d = pad(id)
redis.call('hset', ARGV[3] .. d, 'job', f[1], 'payload', f[2] or '', 'error', ARGV[4], 'attempts', f[3], 'failed_at', ARGV[5])
redis.call('zadd', KEYS[4], id, d)
redis.call('zrem', KEYS[1], ARGV[2])
redis.call('zrem', KEYS[2], ARGV[2])
redis.call('del', k)
return id";

/// KEYS: ready, reserved. ARGV: job key prefix, before. Reservations made before `before` go back to `ready` at
/// their `available_at`; the attempt still counts. Returns how many.
const RELEASE_STALE: &str =
    "local stale = redis.call('zrangebyscore', KEYS[2], '-inf', '(' .. ARGV[2])
local n = 0
for _, m in ipairs(stale) do
  redis.call('zrem', KEYS[2], m)
  local at = redis.call('hget', ARGV[1] .. m, 'available_at')
  if at then
    redis.call('zadd', KEYS[1], at, m)
    n = n + 1
  end
end
return n";

/// KEYS: ready, reserved, dead. Returns the three counts.
const STATS: &str = "return {redis.call('zcard', KEYS[1]), redis.call('zcard', KEYS[2]), redis.call('zcard', KEYS[3])}";

/// KEYS: dead. ARGV: dead key prefix, limit (at least 1). Newest first.
const DEAD_LETTERS: &str = "local ids = redis.call('zrevrange', KEYS[1], 0, tonumber(ARGV[2]) - 1)
local out = {}
for _, d in ipairs(ids) do
  local f = redis.call('hmget', ARGV[1] .. d, 'job', 'payload', 'error', 'attempts', 'failed_at')
  if f[1] then table.insert(out, {d, f[1], f[2], f[3], f[4], f[5]}) end
end
return out";

/// KEYS: dead. ARGV: dead key prefix, member. Removes the dead letter and returns it; nil when another caller took
/// it first or there is none.
const TAKE_DEAD: &str = "if redis.call('zrem', KEYS[1], ARGV[2]) == 0 then return nil end
local k = ARGV[1] .. ARGV[2]
local f = redis.call('hmget', k, 'job', 'payload', 'error', 'attempts', 'failed_at')
redis.call('del', k)
if not f[1] then return nil end
return {ARGV[2], f[1], f[2], f[3], f[4], f[5]}";

/// KEYS: dead, id, ready. ARGV: dead key prefix, member, job key prefix, now. Moves a dead letter back to the
/// queue (attempts start over) in one step. Returns the new job id; nil when there is no such dead letter.
const REQUEUE_DEAD: &str = "if redis.call('zrem', KEYS[1], ARGV[2]) == 0 then return nil end
local k = ARGV[1] .. ARGV[2]
local f = redis.call('hmget', k, 'job', 'payload')
redis.call('del', k)
if not f[1] then return nil end
local id = fresh(KEYS[2], ARGV[3])
local m = pad(id)
redis.call('hset', ARGV[3] .. m, 'job', f[1], 'payload', f[2], 'attempts', 0, 'available_at', ARGV[4], 'created_at', ARGV[4])
redis.call('zadd', KEYS[3], ARGV[4], m)
return id";

/// The full source of a script: the helpers, then `body`.
fn source(body: &str) -> String {
    format!("{HELPERS}{body}")
}

/// The driver's scripts, hashed once.
struct Scripts {
    push: Script,
    reserve: Script,
    delete: Script,
    retry: Script,
    release: Script,
    dead_letter: Script,
    release_stale: Script,
    stats: Script,
    dead_letters: Script,
    take_dead: Script,
    requeue_dead: Script,
}

impl Scripts {
    fn new() -> Self {
        let script = |body: &str| Script::new(&source(body));
        Self {
            push: script(PUSH),
            reserve: script(RESERVE),
            delete: script(DELETE),
            retry: script(RETRY),
            release: script(RELEASE),
            dead_letter: script(DEAD_LETTER),
            release_stale: script(RELEASE_STALE),
            stats: script(STATS),
            dead_letters: script(DEAD_LETTERS),
            take_dead: script(TAKE_DEAD),
            requeue_dead: script(REQUEUE_DEAD),
        }
    }
}

/// The keys of one queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Keys {
    id: String,
    ready: String,
    reserved: String,
    job: String,
    dead_id: String,
    dead: String,
    dead_job: String,
}

impl Keys {
    /// The keys under `prefix` (`QUEUE_PREFIX`) for `queue`.
    pub(crate) fn new(prefix: &str, queue: &str) -> Self {
        let base = key_base(prefix, queue);
        Self {
            id: format!("{base}id"),
            ready: format!("{base}ready"),
            reserved: format!("{base}reserved"),
            job: format!("{base}job:"),
            dead_id: format!("{base}dead_id"),
            dead: format!("{base}dead"),
            dead_job: format!("{base}dead:"),
        }
    }
}

/// A job or dead letter id as a sorted-set member: padded to 20 digits, as the scripts' `pad`.
fn member(id: i64) -> String {
    format!("{id:020}")
}

fn id_of(member: &str, op: &'static str) -> QResult<i64> {
    member
        .parse()
        .map_err(|_| StoreError::new(op, format!("not a job id: `{member}`")))
}

fn number(text: &str, op: &'static str) -> QResult<i64> {
    text.parse()
        .map_err(|_| StoreError::new(op, format!("not a number: `{text}`")))
}

/// A dead letter as the scripts return it: `{member, job, payload, error, attempts, failed_at}`.
type DeadRow = (String, String, String, String, String, String);

fn dead_letter_of(row: DeadRow, op: &'static str) -> QResult<DeadLetter> {
    let (id, job, payload, error, attempts, failed_at) = row;
    Ok(DeadLetter {
        id: id_of(&id, op)?,
        job,
        payload,
        error,
        attempts: u32::try_from(number(&attempts, op)?).unwrap_or(0),
        failed_at_ms: number(&failed_at, op)?,
    })
}

pub(crate) struct RedisDriver {
    client: redis::Client,
    conn: OnceCell<ConnectionManager>,
    timeout: Duration,
    keys: Keys,
    scripts: Scripts,
}

impl std::fmt::Debug for RedisDriver {
    // The client holds the URL, password included: never print it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisDriver")
            .field("keys", &self.keys)
            .finish_non_exhaustive()
    }
}

impl RedisDriver {
    /// A driver for the server at `url` (`REDIS_URL`) with keys under `prefix` (`QUEUE_PREFIX`). Connects on first
    /// use.
    ///
    /// # Errors
    /// `url` is not a Redis URL (the message never contains the URL).
    pub(crate) fn new(url: &str, prefix: &str, timeout: Duration) -> Result<Self, String> {
        if url.starts_with("rediss:") && rustls::crypto::CryptoProvider::get_default().is_none() {
            // redis builds its TLS config with the process-wide provider; Smeltery uses ring.
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let client = redis::Client::open(url)
            .map_err(|e| format!("REDIS_URL is not a valid Redis URL: {e}"))?;
        Ok(Self {
            client,
            conn: OnceCell::new(),
            timeout,
            keys: Keys::new(prefix, QUEUE),
            scripts: Scripts::new(),
        })
    }

    fn timed_out(&self, op: &'static str) -> StoreError {
        StoreError::new(
            op,
            format!("the Redis server did not answer within {:?}", self.timeout),
        )
    }

    async fn conn(&self, op: &'static str) -> QResult<ConnectionManager> {
        let conn = self
            .conn
            .get_or_try_init(|| async {
                let config = ConnectionManagerConfig::new()
                    .set_connection_timeout(Some(self.timeout))
                    .set_response_timeout(Some(self.timeout))
                    .set_number_of_retries(2);
                tokio::time::timeout(
                    self.timeout,
                    ConnectionManager::new_with_config(self.client.clone(), config),
                )
                .await
                .map_err(|_| self.timed_out(op))?
                .map_err(|e| StoreError::new(op, e))
            })
            .await?;
        Ok(conn.clone())
    }

    /// Run a script: `EVALSHA`, and on `NOSCRIPT` `SCRIPT LOAD` + `EVALSHA` again (redis's `ScriptInvocation`).
    async fn run<T: FromRedisValue>(
        &self,
        op: &'static str,
        call: &ScriptInvocation<'_>,
    ) -> QResult<T> {
        let mut conn = self.conn(op).await?;
        tokio::time::timeout(self.timeout, call.invoke_async(&mut conn))
            .await
            .map_err(|_| self.timed_out(op))?
            .map_err(|e| StoreError::new(op, e))
    }
}

impl Driver for RedisDriver {
    fn push<'a>(
        &'a self,
        job: &'a str,
        payload: &'a str,
        available_at: i64,
        now: i64,
    ) -> BoxFuture<'a, QResult<JobId>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.push.prepare_invoke();
            call.key(&k.id)
                .key(&k.ready)
                .arg(&k.job)
                .arg(job)
                .arg(payload)
                .arg(available_at)
                .arg(now);
            self.run("push", &call).await
        })
    }

    fn reserve(&self, now: i64, max_payload: u64) -> BoxFuture<'_, QResult<Option<Reserved>>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.reserve.prepare_invoke();
            call.key(&k.ready)
                .key(&k.reserved)
                .arg(&k.job)
                .arg(now)
                .arg(max_payload);
            let found: Option<(String, String, String, i64, i64)> =
                self.run("reserve", &call).await?;
            found
                .map(|(m, job, payload, attempts, size)| {
                    Ok(Reserved {
                        id: id_of(&m, "reserve")?,
                        job,
                        payload,
                        attempts: u32::try_from(attempts).unwrap_or(0),
                        reserved_at: now,
                        oversized: (size > 0).then(|| u64::try_from(size).unwrap_or(0)),
                    })
                })
                .transpose()
        })
    }

    fn delete<'a>(&'a self, job: &'a Reserved) -> BoxFuture<'a, QResult<bool>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.delete.prepare_invoke();
            call.key(&k.ready)
                .key(&k.reserved)
                .arg(&k.job)
                .arg(member(job.id))
                .arg(job.reserved_at)
                .arg(job.attempts);
            Ok(self.run::<i64>("delete", &call).await? > 0)
        })
    }

    fn retry<'a>(&'a self, job: &'a Reserved, available_at: i64) -> BoxFuture<'a, QResult<bool>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.retry.prepare_invoke();
            call.key(&k.ready)
                .key(&k.reserved)
                .arg(&k.job)
                .arg(member(job.id))
                .arg(available_at)
                .arg(job.reserved_at)
                .arg(job.attempts);
            Ok(self.run::<i64>("retry", &call).await? > 0)
        })
    }

    fn release<'a>(&'a self, job: &'a Reserved) -> BoxFuture<'a, QResult<bool>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.release.prepare_invoke();
            call.key(&k.ready)
                .key(&k.reserved)
                .arg(&k.job)
                .arg(member(job.id))
                .arg(job.reserved_at)
                .arg(job.attempts);
            Ok(self.run::<i64>("release", &call).await? > 0)
        })
    }

    fn dead_letter<'a>(
        &'a self,
        job: &'a Reserved,
        error: &'a str,
        now: i64,
    ) -> BoxFuture<'a, QResult<bool>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.dead_letter.prepare_invoke();
            call.key(&k.ready)
                .key(&k.reserved)
                .key(&k.dead_id)
                .key(&k.dead)
                .arg(&k.job)
                .arg(member(job.id))
                .arg(&k.dead_job)
                .arg(super::stored_error(error))
                .arg(now)
                .arg(job.reserved_at)
                .arg(job.attempts);
            Ok(self.run::<i64>("dead_letter", &call).await? > 0)
        })
    }

    fn release_stale(&self, before: i64) -> BoxFuture<'_, QResult<u64>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.release_stale.prepare_invoke();
            call.key(&k.ready).key(&k.reserved).arg(&k.job).arg(before);
            let n: i64 = self.run("release_stale", &call).await?;
            Ok(u64::try_from(n).unwrap_or(0))
        })
    }

    fn stats(&self) -> BoxFuture<'_, QResult<QueueStats>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.stats.prepare_invoke();
            call.key(&k.ready).key(&k.reserved).key(&k.dead);
            let (pending, reserved, dead): (u64, u64, u64) = self.run("stats", &call).await?;
            Ok(QueueStats {
                pending,
                reserved,
                dead,
            })
        })
    }

    fn dead_letters(&self, limit: u32) -> BoxFuture<'_, QResult<Vec<DeadLetter>>> {
        Box::pin(async move {
            if limit == 0 {
                // `ZREVRANGE 0 -1` would be every dead letter.
                return Ok(Vec::new());
            }
            let k = &self.keys;
            let mut call = self.scripts.dead_letters.prepare_invoke();
            call.key(&k.dead).arg(&k.dead_job).arg(limit);
            let rows: Vec<DeadRow> = self.run("dead_letters", &call).await?;
            rows.into_iter()
                .map(|row| dead_letter_of(row, "dead_letters"))
                .collect()
        })
    }

    fn take_dead(&self, id: i64) -> BoxFuture<'_, QResult<Option<DeadLetter>>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.take_dead.prepare_invoke();
            call.key(&k.dead).arg(&k.dead_job).arg(member(id));
            let row: Option<DeadRow> = self.run("take_dead", &call).await?;
            row.map(|row| dead_letter_of(row, "take_dead")).transpose()
        })
    }

    fn requeue_dead(&self, id: i64, now: i64) -> BoxFuture<'_, QResult<Option<JobId>>> {
        Box::pin(async move {
            let k = &self.keys;
            let mut call = self.scripts.requeue_dead.prepare_invoke();
            call.key(&k.dead)
                .key(&k.id)
                .key(&k.ready)
                .arg(&k.dead_job)
                .arg(member(id))
                .arg(&k.job)
                .arg(now);
            self.run("requeue_dead", &call).await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::super::tests::{ANY, not_held};
    use super::*;

    #[test]
    fn keys_are_under_the_prefix_and_queue() {
        let k = Keys::new("shop_queue_", "default");
        // `{default}` is the hash tag: every key of the queue is in one hash slot (Redis Cluster runs a script only
        // on keys of one slot).
        assert_eq!(k.id, "shop_queue_{default}:id");
        assert_eq!(k.ready, "shop_queue_{default}:ready");
        assert_eq!(k.reserved, "shop_queue_{default}:reserved");
        assert_eq!(k.job, "shop_queue_{default}:job:");
        assert_eq!(k.dead_id, "shop_queue_{default}:dead_id");
        assert_eq!(k.dead, "shop_queue_{default}:dead");
        assert_eq!(k.dead_job, "shop_queue_{default}:dead:");
        // Two apps with their own prefixes share no key.
        let other = Keys::new("blog_queue_", "default");
        assert_ne!(k.ready, other.ready);
    }

    #[test]
    fn members_sort_like_ids_and_round_trip() {
        // Same score: Redis orders members by bytes, so padding keeps id order (9 before 10).
        assert!(member(9) < member(10));
        assert!(member(99_999) < member(100_000));
        assert_eq!(member(5), "00000000000000000005");
        assert_eq!(member(5).len(), 20);
        assert_eq!(id_of(&member(123), "t").unwrap(), 123);
        assert_eq!(id_of(&member(i64::MAX), "t").unwrap(), i64::MAX);
        assert!(id_of("x", "t").is_err());
        // An id that cannot exist (negative) never matches a stored member.
        assert_ne!(member(-1), member(1));
    }

    #[test]
    fn dead_rows_parse() {
        let row = (
            member(7),
            "a".to_owned(),
            "{}".to_owned(),
            "boom".to_owned(),
            "3".to_owned(),
            "1700".to_owned(),
        );
        let dead = dead_letter_of(row, "t").unwrap();
        assert_eq!(
            (
                dead.id,
                dead.attempts,
                dead.failed_at_ms,
                dead.error.as_str()
            ),
            (7, 3, 1700, "boom")
        );
        let bad = (
            member(7),
            "a".into(),
            "{}".into(),
            "e".into(),
            "x".into(),
            "1".into(),
        );
        assert!(dead_letter_of(bad, "t").is_err());
    }

    #[test]
    fn scripts_have_the_helpers_and_use_only_their_declared_keys() {
        let full = source(PUSH);
        assert!(full.starts_with("local function pad(n)"), "{full}");
        assert!(
            full.contains("local function fresh(counter, prefix)"),
            "{full}"
        );
        // Hashed once, sent as EVALSHA: a SHA-1 in hex.
        assert_eq!(Script::new(&full).get_hash().len(), 40);
        // Each script reads no KEYS beyond what its caller passes.
        for (body, n) in [
            (PUSH, 2),
            (RESERVE, 2),
            (DELETE, 2),
            (RETRY, 2),
            (RELEASE, 2),
            (DEAD_LETTER, 4),
            (RELEASE_STALE, 2),
            (STATS, 3),
            (DEAD_LETTERS, 1),
            (TAKE_DEAD, 1),
            (REQUEUE_DEAD, 3),
        ] {
            assert!(body.contains(&format!("KEYS[{n}]")), "{body}");
            assert!(!body.contains(&format!("KEYS[{}]", n + 1)), "{body}");
        }
    }

    /// Sweep W7-02: every script that ends a reservation checks first that the caller's reservation (`reserved_at` +
    /// `attempts`, written by `RESERVE`) is the one held; the behaviour runs against a server in `redis_driver_contract`
    /// (`stale_holders_change_nothing`). W7-03: `RESERVE` measures the payload before reading it, and the dead letter
    /// copies the stored payload instead of taking it from the caller.
    #[test]
    fn scripts_that_end_a_reservation_check_the_holder_first() {
        assert!(HELPERS.contains("local function held(k, reserved, m, at, attempts)"));
        assert!(HELPERS.contains("return f[1] == at and f[2] == attempts"));
        for (body, call) in [
            (
                DELETE,
                "if not held(k, KEYS[2], ARGV[2], ARGV[3], ARGV[4]) then return 0 end",
            ),
            (
                RETRY,
                "if not held(k, KEYS[2], ARGV[2], ARGV[4], ARGV[5]) then return 0 end",
            ),
            (
                RELEASE,
                "if not held(k, KEYS[2], ARGV[2], ARGV[3], ARGV[4]) then return 0 end",
            ),
            (
                DEAD_LETTER,
                "if not held(k, KEYS[2], ARGV[2], ARGV[6], ARGV[7]) then return 0 end",
            ),
        ] {
            let mut lines = body.lines();
            assert!(
                lines
                    .next()
                    .unwrap()
                    .starts_with("local k = ARGV[1] .. ARGV[2]"),
                "{body}"
            );
            assert_eq!(lines.next().unwrap(), call, "{body}");
        }
        assert!(RESERVE.contains("redis.call('hset', k, 'reserved_at', ARGV[2])"));
        assert!(RESERVE.contains("local size = redis.call('hstrlen', k, 'payload')"));
        assert!(RESERVE.contains("if size <= tonumber(ARGV[3]) then"));
        assert!(DEAD_LETTER.contains("redis.call('hmget', k, 'job', 'payload', 'attempts')"));
    }

    /// Review L1: every script that hands out a new id takes it through `fresh`, which skips ids whose key exists
    /// (a counter that fell behind never overwrites a live job or dead letter); no script calls `incr` itself.
    #[test]
    fn new_ids_never_reuse_a_live_key() {
        assert!(HELPERS.contains(
            "while redis.call('exists', prefix .. pad(id)) == 1 do id = redis.call('incr', counter) end"
        ));
        for (body, call) in [
            (PUSH, "fresh(KEYS[1], ARGV[1])"),
            (REQUEUE_DEAD, "fresh(KEYS[2], ARGV[3])"),
            (DEAD_LETTER, "fresh(KEYS[3], ARGV[3])"),
        ] {
            assert!(body.contains(call), "{body}");
            // `fresh` checks the same key prefix the script then writes under.
            let prefix_arg = call.trim_end_matches(')').rsplit(", ").next().unwrap();
            assert!(
                body.contains(&format!("redis.call('hset', {prefix_arg} .. ")),
                "{body}"
            );
        }
        for body in [
            PUSH,
            RESERVE,
            DELETE,
            RETRY,
            RELEASE,
            DEAD_LETTER,
            RELEASE_STALE,
            STATS,
            DEAD_LETTERS,
            TAKE_DEAD,
            REQUEUE_DEAD,
        ] {
            assert!(!body.contains("'incr'"), "{body}");
        }
    }

    #[test]
    fn a_bad_url_is_refused_without_echoing_it() {
        let err = RedisDriver::new("not a url :secret@", "p_", Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("REDIS_URL"), "{err}");
        assert!(!err.contains("secret"), "{err}");
        let driver = RedisDriver::new(
            "redis://:hunter2@127.0.0.1:6379/0",
            "p_",
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(!format!("{driver:?}").contains("hunter2"));
    }

    /// No server on the address: the call fails within its budget instead of hanging (a port nothing listens on,
    /// on this machine).
    #[tokio::test]
    async fn an_unreachable_server_is_an_error_within_the_budget() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let driver = RedisDriver::new(
            &format!("redis://127.0.0.1:{port}"),
            "p_",
            Duration::from_millis(500),
        )
        .unwrap();
        let started = std::time::Instant::now();
        let err = driver.stats().await.unwrap_err();
        assert_eq!(err.operation(), "stats");
        assert!(started.elapsed() < Duration::from_secs(10), "{err}");
    }

    // ----- Against a Redis server (`REDIS_URL`): `cargo test -p smeltery-watchfire --features redis --lib
    // queue::redis -- --ignored`. Each test uses its own random prefix; its `Server` guard deletes the prefix's keys
    // when it is dropped, also when the test panics.

    /// A test's prefix on the server of `REDIS_URL`; dropping it deletes every key under the prefix.
    struct Server {
        url: String,
        prefix: String,
    }

    impl Server {
        fn new() -> Self {
            let mut bytes = [0_u8; 8];
            getrandom::fill(&mut bytes).unwrap();
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            Self {
                url: std::env::var("REDIS_URL").expect("REDIS_URL"),
                prefix: format!("smeltery_test_{hex}_queue_"),
            }
        }

        fn driver(&self) -> RedisDriver {
            RedisDriver::new(&self.url, &self.prefix, Duration::from_secs(5)).unwrap()
        }

        /// A plain (blocking) connection for the test's own commands; usable in `drop`.
        fn plain(&self) -> redis::RedisResult<redis::Connection> {
            redis::Client::open(self.url.as_str())?
                .get_connection_with_timeout(Duration::from_secs(5))
        }

        /// The keys under the prefix (the prefix is hex and `_`: no glob characters).
        fn keys(&self) -> redis::RedisResult<Vec<String>> {
            let mut conn = self.plain()?;
            let mut all = Vec::new();
            let mut cursor: u64 = 0;
            loop {
                let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(format!("{}*", self.prefix))
                    .arg("COUNT")
                    .arg(500)
                    .query(&mut conn)?;
                all.extend(keys);
                if next == 0 {
                    return Ok(all);
                }
                cursor = next;
            }
        }

        fn del(&self, key: &str) {
            let mut conn = self.plain().unwrap();
            let _: i64 = redis::cmd("DEL").arg(key).query(&mut conn).unwrap();
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            // Best effort, also while unwinding from a failed assertion: never panic here.
            let Ok(keys) = self.keys() else { return };
            if keys.is_empty() {
                return;
            }
            if let Ok(mut conn) = self.plain() {
                let _: redis::RedisResult<i64> = redis::cmd("DEL").arg(&keys).query(&mut conn);
            }
        }
    }

    /// The database driver's behavioural contract (order, delays, attempts, retry, release, stale release, dead
    /// letters and their ids, stats), run against Redis.
    #[tokio::test]
    #[ignore = "needs REDIS_URL"]
    async fn redis_driver_contract() {
        let server = Server::new();
        super::super::tests::driver_contract(&server.driver()).await;
    }

    /// Equal `available_at`: the lower id first, also past 9 → 10 (members sort as padded strings).
    #[tokio::test]
    #[ignore = "needs REDIS_URL"]
    async fn redis_reserves_in_id_order_and_edges_match_the_database_driver() {
        let server = Server::new();
        let driver = server.driver();
        let mut ids = Vec::new();
        for i in 0..12 {
            ids.push(driver.push("a", &format!("{i}"), 5, 0).await.unwrap());
        }
        let mut held = Vec::new();
        for id in &ids {
            let job = driver.reserve(5, ANY).await.unwrap().unwrap();
            assert_eq!(job.id, *id);
            held.push(job);
        }
        // Retrying or releasing a job that is gone changes nothing; releasing an unreserved job neither.
        assert!(driver.delete(&held[0]).await.unwrap());
        assert!(!driver.retry(&held[0], 0).await.unwrap());
        assert!(!driver.release(&held[0]).await.unwrap());
        assert!(driver.reserve(i64::MAX, ANY).await.unwrap().is_none());
        let fresh = driver.push("b", "{}", 0, 0).await.unwrap();
        assert!(!driver.release(&not_held(fresh)).await.unwrap());
        let got = driver.reserve(0, ANY).await.unwrap().unwrap();
        assert_eq!(
            (got.id, got.attempts),
            (fresh, 1),
            "release of an unreserved job does nothing"
        );
        // Limits: 0 dead letters is none; an unknown dead letter is neither taken nor requeued.
        assert!(driver.dead_letter(&got, "x", 1).await.unwrap());
        assert!(driver.dead_letters(0).await.unwrap().is_empty());
        assert!(driver.take_dead(424_242).await.unwrap().is_none());
        assert!(driver.requeue_dead(424_242, 1).await.unwrap().is_none());
        // Everything else is reserved or dead now: a job with a negative `available_at` (a clock before 1970 in a
        // test) is the only one due, and a negative `failed_at` and a long error are stored as given / cut.
        assert!(driver.reserve(i64::MAX, ANY).await.unwrap().is_none());
        let negative = driver.push("c", "{}", -5, -5).await.unwrap();
        assert!(
            driver.reserve(-6, ANY).await.unwrap().is_none(),
            "not due yet"
        );
        let job = driver.reserve(-5, ANY).await.unwrap().unwrap();
        assert_eq!((job.id, job.job.as_str()), (negative, "c"));
        let big = "é".repeat(10_000);
        assert!(driver.dead_letter(&job, &big, -5).await.unwrap());
        let dead = driver.dead_letters(1).await.unwrap();
        assert_eq!((dead[0].job.as_str(), dead[0].failed_at_ms), ("c", -5));
        assert!(dead[0].error.contains("truncated"));
    }

    /// Deleting every job and dead letter leaves no key behind but the two id counters; every key carries the
    /// `{default}` hash tag.
    #[tokio::test]
    #[ignore = "needs REDIS_URL"]
    async fn redis_leaves_no_keys_behind() {
        let server = Server::new();
        let driver = server.driver();
        let id = driver.push("a", "{}", 0, 0).await.unwrap();
        let job = driver.reserve(0, ANY).await.unwrap().unwrap();
        assert!(driver.dead_letter(&job, "boom", 1).await.unwrap());
        let tagged = format!("{}{{default}}:", server.prefix);
        let keys = server.keys().unwrap();
        assert!(keys.iter().all(|k| k.starts_with(&tagged)), "{keys:?}");
        let dead = driver.dead_letters(1).await.unwrap()[0].id;
        let again = driver.requeue_dead(dead, 2).await.unwrap().unwrap();
        assert!(again > id);
        let held = driver.reserve(2, ANY).await.unwrap().unwrap();
        assert_eq!(held.id, again);
        assert!(driver.delete(&held).await.unwrap());
        let mut keys = server.keys().unwrap();
        keys.sort();
        assert_eq!(
            keys,
            [format!("{tagged}dead_id"), format!("{tagged}id")],
            "only the id counters stay"
        );
    }

    /// Review L1: the id counters lost (Redis restarted without its last writes, or failed over to a replica that
    /// lagged) while jobs and dead letters survive: new ids skip the live ones instead of overwriting them.
    #[tokio::test]
    #[ignore = "needs REDIS_URL"]
    async fn redis_a_lost_counter_never_overwrites_a_live_job() {
        let server = Server::new();
        let driver = server.driver();
        let tagged = format!("{}{{default}}:", server.prefix);
        let first = driver.push("first", "{\"n\":1}", 0, 0).await.unwrap();
        let second = driver.push("second", "{\"n\":2}", 0, 0).await.unwrap();
        let held = driver.reserve(0, ANY).await.unwrap().unwrap();
        assert_eq!(held.id, first);
        server.del(&format!("{tagged}id"));
        let third = driver.push("third", "{\"n\":3}", 0, 0).await.unwrap();
        assert!(third > second, "{third} reused a live id");
        // The held job and the waiting one are untouched.
        let next = driver.reserve(0, ANY).await.unwrap().unwrap();
        assert_eq!((next.id, next.job.as_str()), (second, "second"));
        assert!(driver.delete(&held).await.unwrap());
        let last = driver.reserve(0, ANY).await.unwrap().unwrap();
        assert_eq!((last.id, last.job.as_str()), (third, "third"));

        // The same for dead letters, and for a dead letter queued again.
        assert!(driver.dead_letter(&next, "a", 1).await.unwrap());
        assert!(driver.dead_letter(&last, "b", 1).await.unwrap());
        let dead = driver.dead_letters(10).await.unwrap();
        assert_eq!(dead.len(), 2);
        server.del(&format!("{tagged}dead_id"));
        server.del(&format!("{tagged}id"));
        let job = driver.push("fourth", "{}", 0, 0).await.unwrap();
        let job = driver
            .reserve(0, ANY)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("job {job}"));
        assert!(driver.dead_letter(&job, "c", 2).await.unwrap());
        let after = driver.dead_letters(10).await.unwrap();
        assert_eq!(after.len(), 3, "{after:?}");
        let errors: Vec<&str> = after.iter().map(|d| d.error.as_str()).collect();
        assert_eq!(errors, ["c", "b", "a"], "newest first, none overwritten");
        let requeued = driver.requeue_dead(after[2].id, 3).await.unwrap().unwrap();
        let back = driver.reserve(3, ANY).await.unwrap().unwrap();
        assert_eq!((back.id, back.job.as_str()), (requeued, "second"));
    }

    /// Two drivers (two connections, as two processes) with four workers each take 200 jobs: each job is reserved
    /// exactly once. Concurrent `requeue_dead` / `take_dead` of one dead letter: exactly one caller wins.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs REDIS_URL"]
    async fn redis_concurrent_workers_reserve_each_job_once() {
        const JOBS: i64 = 200;
        let server = Server::new();
        let a = Arc::new(server.driver());
        let b = Arc::new(server.driver());
        for n in 0..JOBS {
            a.push("count", &n.to_string(), 0, 0).await.unwrap();
        }
        let seen: Arc<std::sync::Mutex<HashMap<i64, u32>>> = Arc::default();
        let busy = Arc::new(AtomicU32::new(0));
        let mut tasks = tokio::task::JoinSet::new();
        for w in 0..8 {
            let driver = Arc::clone(if w % 2 == 0 { &a } else { &b });
            let seen = Arc::clone(&seen);
            let busy = Arc::clone(&busy);
            tasks.spawn(async move {
                let mut took = 0;
                while let Some(job) = driver.reserve(1, ANY).await.unwrap() {
                    *seen.lock().unwrap().entry(job.id).or_insert(0) += 1;
                    took += 1;
                    tokio::task::yield_now().await;
                    assert!(driver.delete(&job).await.unwrap());
                }
                if took > 0 {
                    busy.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
        while let Some(done) = tasks.join_next().await {
            done.unwrap();
        }
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), usize::try_from(JOBS).unwrap());
        assert!(seen.values().all(|n| *n == 1), "{seen:?}");
        assert!(busy.load(Ordering::SeqCst) >= 2);
        assert_eq!(
            a.stats().await.unwrap(),
            QueueStats {
                pending: 0,
                reserved: 0,
                dead: 0
            }
        );

        let id = a.push("x", "{}", 0, 0).await.unwrap();
        let job = a.reserve(0, ANY).await.unwrap().unwrap();
        assert_eq!(job.id, id);
        assert!(a.dead_letter(&job, "boom", 1).await.unwrap());
        let dead = a.dead_letters(1).await.unwrap()[0].id;
        let (x, y, z) = tokio::join!(
            a.requeue_dead(dead, 2),
            b.requeue_dead(dead, 2),
            b.take_dead(dead)
        );
        let wins = usize::from(x.unwrap().is_some())
            + usize::from(y.unwrap().is_some())
            + usize::from(z.unwrap().is_some());
        assert_eq!(wins, 1);
    }

    /// The whole path through a worker: a job dispatched to the Redis queue runs once, a failing one retries and
    /// lands in the dead letters, and the dashboard's retry takes it back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs REDIS_URL"]
    async fn redis_queue_runs_jobs_through_workers() {
        use serde::{Deserialize, Serialize};

        use crate::error::AgentError;
        use crate::queue::{Job, JobCtx, Queue};
        use crate::time::Clock;

        static RUNS: AtomicU32 = AtomicU32::new(0);
        static FAILS: AtomicU32 = AtomicU32::new(0);

        #[derive(Serialize, Deserialize)]
        struct Once;
        impl Job for Once {
            const NAME: &'static str = "once";
            async fn handle(&self, _: JobCtx) -> Result<(), AgentError> {
                RUNS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        #[derive(Serialize, Deserialize)]
        struct Fails;
        impl Job for Fails {
            const NAME: &'static str = "fails";
            fn max_attempts(&self) -> u32 {
                2
            }
            fn backoff(&self, _: u32) -> Duration {
                Duration::from_millis(10)
            }
            async fn handle(&self, _: JobCtx) -> Result<(), AgentError> {
                FAILS.fetch_add(1, Ordering::SeqCst);
                Err(AgentError::msg("nope"))
            }
        }

        let server = Server::new();
        let mut w = crate::registry::Watchfire::new();
        w.job::<Once>();
        w.job::<Fails>();
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.env = "testing".to_owned();
        settings.database_url = String::new();
        let app = smeltery_core::AppBuilder::new(settings)
            .build()
            .await
            .unwrap()
            .app;
        let queue = Queue::redis(
            &server.url,
            &server.prefix,
            Clock::new(),
            Duration::from_secs(5),
        )
        .unwrap();
        app.insert_service(queue.clone());
        let mut h = crate::testing::Harness::from_watchfire(w)
            .app(app.clone())
            .workers(2);
        h.start().await.unwrap();
        assert_eq!(h.agents().queue().unwrap().driver(), "redis");
        Once.dispatch(&app).await.unwrap();
        Fails.dispatch(&app).await.unwrap();
        let mut dead = Vec::new();
        for _ in 0..400 {
            dead = queue.dead_letters(10).await.unwrap();
            if RUNS.load(Ordering::SeqCst) == 1 && dead.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
        assert_eq!(FAILS.load(Ordering::SeqCst), 2);
        assert_eq!(
            (dead.len(), dead[0].job.as_str(), dead[0].attempts),
            (1, "fails", 2)
        );
        assert!(queue.retry_dead(dead[0].id).await.unwrap().is_some());
        for _ in 0..400 {
            if FAILS.load(Ordering::SeqCst) == 4 && queue.stats().await.unwrap().dead == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(FAILS.load(Ordering::SeqCst), 4, "attempts start over");
        let stats = queue.stats().await.unwrap();
        assert_eq!((stats.pending, stats.reserved, stats.dead), (0, 0, 1));
        assert!(
            queue
                .delete_dead(queue.dead_letters(1).await.unwrap()[0].id)
                .await
                .unwrap()
        );
        h.shutdown().await;
    }
}
