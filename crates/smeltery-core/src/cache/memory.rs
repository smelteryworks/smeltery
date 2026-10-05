//! The `memory` store: one moka cache for the whole process, bounded by
//! `CACHE_MEMORY_CAPACITY` entries, each entry expiring on its own time to live.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use moka::Expiry;
use moka::future::Cache as Moka;
use moka::ops::compute::{CompResult, Op};

use super::{CacheStore, LockMap, add_to, expires_at, live, now_ms};
use crate::app::BoxFuture;
use crate::error::{Error, Result};

#[derive(Clone)]
struct Entry {
    value: Arc<str>,
    /// Unix ms; `None` = kept until removed or evicted.
    expires: Option<u64>,
}

impl Entry {
    fn remaining(&self) -> Option<Duration> {
        self.expires
            .map(|e| Duration::from_millis(e.saturating_sub(now_ms())))
    }
}

/// Each entry expires at its own time (moka's per-entry `Expiry`).
struct ByEntry;

impl Expiry<String, Entry> for ByEntry {
    fn expire_after_create(&self, _: &String, value: &Entry, _: Instant) -> Option<Duration> {
        value.remaining()
    }

    fn expire_after_update(
        &self,
        _: &String,
        value: &Entry,
        _: Instant,
        _: Option<Duration>,
    ) -> Option<Duration> {
        value.remaining()
    }
}

pub(crate) struct MemoryStore {
    cache: Moka<String, Entry>,
    /// Locks are kept apart from the bounded cache: eviction must never drop a held lock.
    locks: LockMap,
}

impl MemoryStore {
    /// The process-wide store; the first caller's capacity sizes it.
    pub(crate) fn global(capacity: u64) -> Arc<dyn CacheStore> {
        static GLOBAL: OnceLock<Arc<MemoryStore>> = OnceLock::new();
        let store = GLOBAL.get_or_init(|| {
            Arc::new(MemoryStore {
                cache: Moka::builder()
                    .max_capacity(capacity.max(1))
                    .expire_after(ByEntry)
                    .build(),
                locks: LockMap::default(),
            })
        });
        Arc::clone(store) as Arc<dyn CacheStore>
    }
}

fn live_entry(entry: Option<&moka::Entry<String, Entry>>) -> Option<&Entry> {
    entry.map(moka::Entry::value).filter(|e| live(e.expires))
}

impl CacheStore for MemoryStore {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move {
            Ok(self
                .cache
                .get(key)
                .await
                .filter(|e| live(e.expires))
                .map(|e| e.value.to_string()))
        })
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let entry = Entry {
                value: value.into(),
                expires: expires_at(ttl),
            };
            self.cache.insert(key.to_owned(), entry).await;
            Ok(())
        })
    }

    fn add<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let entry = Entry {
                value: value.into(),
                expires: expires_at(ttl),
            };
            // `and_compute_with` runs one call per key at a time.
            let result = self
                .cache
                .entry(key.to_owned())
                .and_compute_with(|current| async move {
                    if live_entry(current.as_ref()).is_some() {
                        Op::Nop
                    } else {
                        Op::Put(entry)
                    }
                })
                .await;
            Ok(matches!(
                result,
                CompResult::Inserted(_) | CompResult::ReplacedWith(_)
            ))
        })
    }

    fn increment<'a>(&'a self, key: &'a str, by: i64) -> BoxFuture<'a, Result<i64>> {
        Box::pin(async move {
            let result = self
                .cache
                .entry(key.to_owned())
                .and_try_compute_with(|current| async move {
                    let current = live_entry(current.as_ref());
                    let next = add_to(current.map(|e| &*e.value), by)?;
                    Ok::<_, Error>(Op::Put(Entry {
                        value: next.to_string().into(),
                        expires: current.and_then(|e| e.expires),
                    }))
                })
                .await?;
            match result {
                CompResult::Inserted(e) | CompResult::ReplacedWith(e) => e
                    .value()
                    .value
                    .parse()
                    .map_err(|_| Error::internal("the cache counter was not stored")),
                _ => Err(Error::internal("the cache counter was not stored")),
            }
        })
    }

    fn forget<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            Ok(self
                .cache
                .remove(key)
                .await
                .is_some_and(|e| live(e.expires)))
        })
    }

    fn flush<'a>(&'a self, prefix: &'a str, keep: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let keys: Vec<Arc<String>> = self
                .cache
                .iter()
                .filter(|(key, _)| super::flushed(key, prefix, keep))
                .map(|(key, _)| key)
                .collect();
            for key in keys {
                self.cache.invalidate(key.as_str()).await;
            }
            self.locks.flush(prefix, keep);
            Ok(())
        })
    }

    fn acquire_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let taken = self.locks.acquire(name, owner, ttl);
        Box::pin(async move { Ok(taken) })
    }

    fn release_lock<'a>(&'a self, name: &'a str, owner: &'a str) -> BoxFuture<'a, Result<bool>> {
        let released = self.locks.release(name, owner);
        Box::pin(async move { Ok(released) })
    }

    fn refresh_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let refreshed = self.locks.refresh(name, owner, ttl);
        Box::pin(async move { Ok(refreshed) })
    }

    fn force_release_lock<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        self.locks.force_release(name);
        Box::pin(async { Ok(()) })
    }

    fn lock_owner<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        let owner = self.locks.owner(name);
        Box::pin(async move { Ok(owner) })
    }
}
