//! The `redis` store (feature `redis`): one multiplexed connection through redis's
//! `ConnectionManager` (it reconnects by itself), opened on first use. Every call has the
//! `CACHE_TIMEOUT` budget. `add` and locks are `SET … NX PX`, counters `INCRBY`, lock release
//! a compare-and-delete Lua script, `pull` is `GETDEL`, `flush` is `SCAN MATCH <prefix>*` +
//! `UNLINK`.

use std::time::Duration;

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{Cmd, FromRedisValue};
use tokio::sync::OnceCell;

use super::CacheStore;
use crate::app::BoxFuture;
use crate::error::{Error, Result};

/// Delete the key only while it still holds this owner.
const RELEASE: &str = "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('del', KEYS[1]) else return 0 end";
/// Give the key a new time to live (ARGV[2] ms; 0: none) only while it still holds this owner.
const REFRESH: &str = "if redis.call('get', KEYS[1]) == ARGV[1] then if tonumber(ARGV[2]) > 0 then return redis.call('pexpire', KEYS[1], ARGV[2]) else redis.call('persist', KEYS[1]) return 1 end else return 0 end";
/// The value, or its size (an integer) when it is larger than ARGV[1] bytes: a large value never crosses the wire.
const GET_BOUNDED: &str = "local n = redis.call('strlen', KEYS[1]) if n > tonumber(ARGV[1]) then return n end return redis.call('get', KEYS[1])";

pub(crate) struct RedisStore {
    client: redis::Client,
    conn: OnceCell<ConnectionManager>,
    timeout: Duration,
    /// `CACHE_MAX_VALUE_BYTES`.
    max_value: usize,
}

fn px(cmd: &mut Cmd, ttl: Option<Duration>) {
    if let Some(ttl) = ttl {
        let ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1);
        cmd.arg("PX").arg(ms);
    }
}

/// `prefix` with the glob characters of `SCAN MATCH` escaped.
fn glob_escape(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('*');
    out
}

impl RedisStore {
    pub(crate) fn new(url: &str, timeout: Duration) -> Result<Self> {
        if url.starts_with("rediss:") && rustls::crypto::CryptoProvider::get_default().is_none() {
            // redis builds its TLS config with the process-wide provider; Smeltery uses ring.
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let client = redis::Client::open(url)
            .map_err(|e| Error::internal(format!("REDIS_URL is not a valid Redis URL: {e}")))?;
        Ok(Self {
            client,
            conn: OnceCell::new(),
            timeout,
            max_value: super::DEFAULT_MAX_VALUE_BYTES,
        })
    }

    /// The same store with this `CACHE_MAX_VALUE_BYTES`.
    pub(crate) fn max_value(mut self, bytes: usize) -> Self {
        self.max_value = bytes;
        self
    }

    fn timed_out(&self) -> Error {
        Error::internal(format!(
            "the redis cache store did not answer within {:?}",
            self.timeout
        ))
    }

    async fn conn(&self) -> Result<ConnectionManager> {
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
                .map_err(|_| self.timed_out())?
                .map_err(Error::other)
            })
            .await?;
        Ok(conn.clone())
    }

    async fn run<T: FromRedisValue>(&self, cmd: &Cmd) -> Result<T> {
        let mut conn = self.conn().await?;
        tokio::time::timeout(self.timeout, cmd.query_async(&mut conn))
            .await
            .map_err(|_| self.timed_out())?
            .map_err(Error::other)
    }

    async fn set(&self, key: &str, value: &str, ttl: Option<Duration>, nx: bool) -> Result<bool> {
        let mut cmd = redis::cmd("SET");
        cmd.arg(key).arg(value);
        if nx {
            cmd.arg("NX");
        }
        px(&mut cmd, ttl);
        let reply: Option<String> = self.run(&cmd).await?;
        Ok(reply.is_some())
    }
}

impl CacheStore for RedisStore {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move {
            let reply: redis::Value = self
                .run(
                    redis::cmd("EVAL")
                        .arg(GET_BOUNDED)
                        .arg(1)
                        .arg(key)
                        .arg(self.max_value),
                )
                .await?;
            match reply {
                redis::Value::Int(n) => Err(super::too_large(
                    u64::try_from(n).unwrap_or(0),
                    self.max_value,
                )),
                other => Option::<String>::from_redis_value(other).map_err(Error::other),
            }
        })
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.set(key, value, ttl, false).await.map(|_| ()) })
    }

    fn add<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move { self.set(key, value, ttl, true).await })
    }

    fn increment<'a>(&'a self, key: &'a str, by: i64) -> BoxFuture<'a, Result<i64>> {
        Box::pin(async move { self.run(redis::cmd("INCRBY").arg(key).arg(by)).await })
    }

    fn forget<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let removed: i64 = self.run(redis::cmd("DEL").arg(key)).await?;
            Ok(removed > 0)
        })
    }

    fn pull<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move { self.run(redis::cmd("GETDEL").arg(key)).await })
    }

    fn flush<'a>(&'a self, prefix: &'a str, keep: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let pattern = glob_escape(prefix);
            let mut cursor: u64 = 0;
            loop {
                let (next, keys): (u64, Vec<String>) = self
                    .run(
                        redis::cmd("SCAN")
                            .arg(cursor)
                            .arg("MATCH")
                            .arg(&pattern)
                            .arg("COUNT")
                            .arg(500),
                    )
                    .await?;
                let keys: Vec<String> = keys
                    .into_iter()
                    .filter(|key| super::flushed(key, prefix, keep))
                    .collect();
                if !keys.is_empty() {
                    let _: i64 = self.run(redis::cmd("UNLINK").arg(&keys)).await?;
                }
                if next == 0 {
                    return Ok(());
                }
                cursor = next;
            }
        })
    }

    fn acquire_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move { self.set(name, owner, ttl, true).await })
    }

    fn release_lock<'a>(&'a self, name: &'a str, owner: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let removed: i64 = self
                .run(redis::cmd("EVAL").arg(RELEASE).arg(1).arg(name).arg(owner))
                .await?;
            Ok(removed > 0)
        })
    }

    fn refresh_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let ms = ttl.map_or(0, |ttl| {
                u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1)
            });
            let refreshed: i64 = self
                .run(
                    redis::cmd("EVAL")
                        .arg(REFRESH)
                        .arg(1)
                        .arg(name)
                        .arg(owner)
                        .arg(ms),
                )
                .await?;
            Ok(refreshed > 0)
        })
    }

    fn force_release_lock<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let _: i64 = self.run(redis::cmd("DEL").arg(name)).await?;
            Ok(())
        })
    }

    fn lock_owner<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move { self.run(redis::cmd("GET").arg(name)).await })
    }
}

#[cfg(test)]
mod tests {
    use super::glob_escape;

    #[test]
    fn scan_patterns_escape_glob_characters() {
        assert_eq!(glob_escape("app_cache_"), "app_cache_*");
        assert_eq!(glob_escape("a*b?[c]\\"), "a\\*b\\?\\[c\\]\\\\*");
    }
}
