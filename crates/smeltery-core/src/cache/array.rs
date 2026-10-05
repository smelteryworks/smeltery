//! The `array` store (one map per app, for tests) and the `null` store (stores nothing).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use super::{CacheStore, LockMap, add_to, expires_at, live};
use crate::app::BoxFuture;
use crate::error::Result;

/// Entries in a map owned by one app (or one [`Cache::open`](super::Cache::open)).
#[derive(Default)]
pub(crate) struct ArrayStore {
    entries: Mutex<HashMap<String, (String, Option<u64>)>>,
    locks: LockMap,
}

impl ArrayStore {
    fn with<T>(&self, f: impl FnOnce(&mut HashMap<String, (String, Option<u64>)>) -> T) -> T {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut entries)
    }
}

impl CacheStore for ArrayStore {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        let value = self.with(|map| {
            map.get(key)
                .filter(|(_, expires)| live(*expires))
                .map(|(value, _)| value.clone())
        });
        Box::pin(async move { Ok(value) })
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<()>> {
        self.with(|map| map.insert(key.to_owned(), (value.to_owned(), expires_at(ttl))));
        Box::pin(async { Ok(()) })
    }

    fn add<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let added = self.with(|map| {
            if map.get(key).is_some_and(|(_, expires)| live(*expires)) {
                return false;
            }
            map.insert(key.to_owned(), (value.to_owned(), expires_at(ttl)));
            true
        });
        Box::pin(async move { Ok(added) })
    }

    fn increment<'a>(&'a self, key: &'a str, by: i64) -> BoxFuture<'a, Result<i64>> {
        let result = self.with(|map| {
            let current = map.get(key).filter(|(_, expires)| live(*expires));
            let expires = current.and_then(|(_, expires)| *expires);
            let next = add_to(current.map(|(value, _)| value.as_str()), by)?;
            map.insert(key.to_owned(), (next.to_string(), expires));
            Ok(next)
        });
        Box::pin(async move { result })
    }

    fn forget<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>> {
        let removed = self.with(|map| map.remove(key).is_some_and(|(_, e)| live(e)));
        Box::pin(async move { Ok(removed) })
    }

    fn flush<'a>(&'a self, prefix: &'a str, keep: &'a [String]) -> BoxFuture<'a, Result<()>> {
        self.with(|map| map.retain(|key, _| !super::flushed(key, prefix, keep)));
        self.locks.flush(prefix, keep);
        Box::pin(async { Ok(()) })
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

/// The `null` store: every read misses, `add` stores nothing and answers `false`,
/// `increment` answers `by` (a counter that starts from 0 each time), and every lock is
/// granted.
pub(crate) struct NullStore;

impl CacheStore for NullStore {
    fn get<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }

    fn put<'a>(&'a self, _: &'a str, _: &'a str, _: Option<Duration>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn add<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn increment<'a>(&'a self, _: &'a str, by: i64) -> BoxFuture<'a, Result<i64>> {
        Box::pin(async move { Ok(by) })
    }

    fn forget<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn flush<'a>(&'a self, _: &'a str, _: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn acquire_lock<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(true) })
    }

    fn release_lock<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(true) })
    }

    fn refresh_lock<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(true) })
    }

    fn force_release_lock<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn lock_owner<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }
}
