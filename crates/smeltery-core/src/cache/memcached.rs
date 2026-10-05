//! The `memcached` store (feature `memcached`): the `memcache` crate's synchronous client (its
//! binary protocol, a small connection pool per server), run on blocking threads with the
//! `CACHE_TIMEOUT` budget around every call. The client is built on first use.
//!
//! `add` and lock acquiring are memcached's `add`; counters are `incr` / `decr` after an
//! `add` of `0` (memcached counters are unsigned: they stop at 0); lock release is a `cas`
//! that replaces the owner's entry with one that has already expired. Keys longer than
//! memcached's 250 bytes, or with spaces or control characters, are stored under their
//! SHA-256. Memcached cannot list keys, so `flush` empties the servers.

use std::sync::Arc;
use std::time::Duration;

use memcache::{Client, CommandError, MemcacheError};
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

use super::CacheStore;
use crate::app::BoxFuture;
use crate::error::{Error, Result};

/// Memcached reads an expiry above 30 days as a Unix time.
const MAX_RELATIVE: u64 = 30 * 24 * 60 * 60;
/// A Unix time in 1970: an entry stored with it has expired already.
const EXPIRED: u32 = 2_592_001;

pub(crate) struct MemcachedStore {
    servers: Vec<String>,
    client: OnceCell<Arc<Client>>,
    timeout: Duration,
}

fn exptime(ttl: Option<Duration>) -> u32 {
    let Some(ttl) = ttl else { return 0 };
    let secs = ttl.as_millis().div_ceil(1000).max(1);
    let secs = u64::try_from(secs).unwrap_or(u64::MAX);
    if secs <= MAX_RELATIVE {
        return u32::try_from(secs).unwrap_or(u32::MAX);
    }
    let now = super::now_ms() / 1000;
    u32::try_from(now.saturating_add(secs)).unwrap_or(u32::MAX)
}

/// The key memcached stores: the key itself when memcached accepts it, else its SHA-256.
fn wire_key(key: &str) -> String {
    let ok = key.len() <= 250 && key.bytes().all(|b| b > b' ' && b != 0x7f);
    if ok {
        key.to_owned()
    } else {
        let digest = Sha256::digest(key.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        format!("sha256:{hex}")
    }
}

fn is(e: &MemcacheError, kind: &CommandError) -> bool {
    matches!(e, MemcacheError::CommandError(c) if std::mem::discriminant(c) == std::mem::discriminant(kind))
}

/// `add` that answers `false` when the key exists.
fn add(
    client: &Client,
    key: &str,
    value: &str,
    exp: u32,
) -> std::result::Result<bool, MemcacheError> {
    match client.add(key, value, exp) {
        Ok(()) => Ok(true),
        Err(e) if is(&e, &CommandError::KeyExists) => Ok(false),
        Err(e) => Err(e),
    }
}

impl MemcachedStore {
    pub(crate) fn new(servers: &str, timeout: Duration) -> Result<Self> {
        let servers: Vec<String> = servers
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                if s.contains("://") {
                    s.to_owned()
                } else {
                    format!("memcache://{s}")
                }
            })
            .collect();
        if servers.is_empty() {
            return Err(Error::internal("MEMCACHED_SERVERS names no server"));
        }
        Ok(Self {
            servers,
            client: OnceCell::new(),
            timeout,
        })
    }

    fn timed_out(&self) -> Error {
        Error::internal(format!(
            "the memcached cache store did not answer within {:?}",
            self.timeout
        ))
    }

    /// Run `f` on a blocking thread within the timeout.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> std::result::Result<T, MemcacheError> + Send + 'static,
    ) -> Result<T> {
        tokio::time::timeout(self.timeout, tokio::task::spawn_blocking(f))
            .await
            .map_err(|_| self.timed_out())?
            .map_err(|e| Error::internal(format!("a memcached task failed: {e}")))?
            .map_err(Error::other)
    }

    async fn client(&self) -> Result<Arc<Client>> {
        let client = self
            .client
            .get_or_try_init(|| async {
                let servers = self.servers.clone();
                let timeout = self.timeout;
                self.blocking(move || {
                    Client::builder()
                        .add_server(servers)?
                        .with_max_pool_size(4)
                        .with_min_idle_conns(1)
                        .with_connection_timeout(timeout)
                        .with_read_timeout(timeout)
                        .with_write_timeout(timeout)
                        .build()
                        .map(Arc::new)
                })
                .await
            })
            .await?;
        Ok(Arc::clone(client))
    }

    async fn with<T: Send + 'static>(
        &self,
        key: &str,
        f: impl FnOnce(&Client, &str) -> std::result::Result<T, MemcacheError> + Send + 'static,
    ) -> Result<T> {
        let client = self.client().await?;
        let key = wire_key(key);
        self.blocking(move || f(&client, &key)).await
    }
}

impl CacheStore for MemcachedStore {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(self.with(key, |c, k| c.get::<String>(k)))
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<()>> {
        let value = value.to_owned();
        Box::pin(self.with(key, move |c, k| c.set(k, value.as_str(), exptime(ttl))))
    }

    fn add<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let value = value.to_owned();
        Box::pin(self.with(key, move |c, k| add(c, k, &value, exptime(ttl))))
    }

    fn increment<'a>(&'a self, key: &'a str, by: i64) -> BoxFuture<'a, Result<i64>> {
        Box::pin(async move {
            let next = self
                .with(key, move |c, k| {
                    // The binary protocol's `incr` on a missing key would store its initial
                    // value without adding: create the counter first.
                    add(c, k, "0", 0)?;
                    if by >= 0 {
                        c.increment(k, by.unsigned_abs())
                    } else {
                        c.decrement(k, by.unsigned_abs())
                    }
                })
                .await?;
            i64::try_from(next).map_err(|_| Error::internal("the cache counter overflowed"))
        })
    }

    fn forget<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.with(key, |c, k| c.delete(k)))
    }

    fn flush<'a>(&'a self, _prefix: &'a str, _keep: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let client = self.client().await?;
            self.blocking(move || client.flush()).await
        })
    }

    fn acquire_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let owner = owner.to_owned();
        Box::pin(self.with(name, move |c, k| add(c, k, &owner, exptime(ttl))))
    }

    fn release_lock<'a>(&'a self, name: &'a str, owner: &'a str) -> BoxFuture<'a, Result<bool>> {
        let owner = owner.to_owned();
        Box::pin(self.with(name, move |c, k| {
            let Some((held, _flags, Some(cas))) = c.get::<(String, u32, Option<u64>)>(k)? else {
                return Ok(false);
            };
            if held != owner {
                return Ok(false);
            }
            // Replace the entry only if nobody changed it since the read, with one that has
            // expired already: an atomic compare-and-delete.
            c.cas(k, "", EXPIRED, cas)
        }))
    }

    fn refresh_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let owner = owner.to_owned();
        Box::pin(self.with(name, move |c, k| {
            let Some((held, _flags, Some(cas))) = c.get::<(String, u32, Option<u64>)>(k)? else {
                return Ok(false);
            };
            if held != owner {
                return Ok(false);
            }
            // The same entry with a new expiry, only if nobody changed it since the read.
            c.cas(k, owner.as_str(), exptime(ttl), cas)
        }))
    }

    fn force_release_lock<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.with(name, |c, k| c.delete(k).map(|_| ())))
    }

    fn lock_owner<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(self.with(name, |c, k| c.get::<String>(k)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_or_spaced_keys_are_hashed() {
        assert_eq!(wire_key("app_cache_users"), "app_cache_users");
        assert!(wire_key("has space").starts_with("sha256:"));
        assert!(wire_key(&"k".repeat(251)).starts_with("sha256:"));
        assert!(wire_key(&"k".repeat(251)).len() <= 250);
    }

    #[test]
    fn expiry_is_relative_up_to_30_days() {
        assert_eq!(exptime(None), 0);
        assert_eq!(exptime(Some(Duration::from_millis(1))), 1);
        assert_eq!(exptime(Some(Duration::from_millis(1500))), 2);
        assert!(exptime(Some(Duration::from_secs(MAX_RELATIVE + 1))) > 1_700_000_000);
    }
}
