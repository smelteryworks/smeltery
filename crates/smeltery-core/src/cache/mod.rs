//! The cache: typed values with a time to live, counters and atomic locks, over one of several
//! stores.
//!
//! Handlers take a [`Cache`] argument; other code calls [`App::cache`]. The default store is
//! `CACHE_STORE` (`database` unless set), every key gets `CACHE_PREFIX` in front, and
//! [`Cache::store`] picks another store by name.
//!
//! ```
//! # async fn demo() -> smeltery_core::Result<()> {
//! use std::time::Duration;
//! use smeltery_core::cache::Cache;
//! use smeltery_core::config::Settings;
//!
//! let cache = Cache::open("array", &Settings::from_env(), None)?;
//! cache.put("greeting", "hello", Duration::from_secs(60)).await?;
//! assert_eq!(cache.get::<String>("greeting").await?.as_deref(), Some("hello"));
//!
//! let total: u64 = cache
//!     .remember("stats.total", Duration::from_secs(300), || async { Ok(42) })
//!     .await?;
//! assert_eq!(total, 42);
//!
//! assert!(cache.add("once", &true, Duration::from_secs(60)).await?);
//! assert!(!cache.add("once", &true, Duration::from_secs(60)).await?);
//! assert_eq!(cache.increment("hits", 1).await?, 1);
//!
//! let lock = cache.lock("reports", Duration::from_secs(30));
//! if lock.get().await? {
//!     // Only one holder at a time, across every process sharing the store.
//!     lock.release().await?;
//! }
//! # Ok(())
//! # }
//! # tokio::runtime::Runtime::new().unwrap().block_on(demo()).unwrap();
//! ```
//!
//! | Store | Where entries live | Shared between processes |
//! |---|---|---|
//! | `database` | the `cache` and `cache_locks` tables (see [`migrations`]) | yes |
//! | `redis` (feature `redis`) | a Redis server (`REDIS_URL`) | yes |
//! | `memcached` (feature `memcached`) | memcached servers (`MEMCACHED_SERVERS`) | yes |
//! | `file` | one file per key under `CACHE_PATH` | yes, on one machine |
//! | `memory` | the process memory, up to `CACHE_MEMORY_CAPACITY` entries | no |
//! | `array` | the memory of one [`Cache`] / app (what `TestApp` uses) | no |
//! | `null` | nowhere: every read misses | no |
//!
//! Values are stored as JSON. `add`, `increment` / `decrement` and locks are atomic within a
//! store, also between processes for the shared stores.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::app::{App, BoxFuture};
use crate::config::Settings;
use crate::db::Db;
use crate::error::{Error, Result};

mod array;
mod database;
mod file;
#[cfg(feature = "memcached")]
mod memcached;
mod memory;
#[cfg(feature = "redis")]
mod redis;

pub use crate::rate_limit::{RateLimit, RateLimiter};
pub use database::migrations;

/// The store names [`Cache::store`] understands.
pub const STORES: &[&str] = &[
    "array",
    "database",
    "file",
    "memcached",
    "memory",
    "null",
    "redis",
];

/// What every store does. Keys arrive with the prefix applied; values are JSON text.
///
/// `ttl` is `None` for entries kept until removed. Lock names live apart from keys in the
/// stores that have a separate lock table or map, and as `lock:<name>` keys in the others.
pub(crate) trait CacheStore: Send + Sync + 'static {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<String>>>;
    fn put<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<()>>;
    /// Store only when the key is absent or expired; `true` when stored.
    fn add<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>>;
    /// Add `by` to the integer under `key` (a missing key counts as 0, kept until removed) and
    /// return the new value. The entry keeps its time to live.
    fn increment<'a>(&'a self, key: &'a str, by: i64) -> BoxFuture<'a, Result<i64>>;
    /// Remove the entry; `true` when a live one was there.
    fn forget<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>>;
    /// Read and remove.
    fn pull<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move {
            let value = self.get(key).await?;
            if value.is_some() {
                self.forget(key).await?;
            }
            Ok(value)
        })
    }
    /// Remove every entry and lock whose name starts with `prefix` but with none of `keep` (stores that cannot
    /// list keys remove everything).
    fn flush<'a>(&'a self, prefix: &'a str, keep: &'a [String]) -> BoxFuture<'a, Result<()>>;
    /// Take lock `name` for `owner` when nobody holds it (or the holder's time ran out).
    fn acquire_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>>;
    /// Free lock `name` when `owner` holds it; `true` when it did.
    fn release_lock<'a>(&'a self, name: &'a str, owner: &'a str) -> BoxFuture<'a, Result<bool>>;
    /// Hold lock `name` for `ttl` from now when `owner` holds it (a live hold); `true` when it
    /// did. Atomic: an expired or foreign hold is never extended.
    fn refresh_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>>;
    /// Free lock `name` whoever holds it.
    fn force_release_lock<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<()>>;
    /// The current holder of lock `name`.
    fn lock_owner<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>>;
}

/// The start of the keys and lock names (after the cache prefix) of Watchfire's coordination: agent leases,
/// process leases and schedule claims. [`Cache::flush`] (`cache:clear`) keeps them; [`Cache::flush_all`]
/// (`cache:clear --all`) removes them too.
pub const RESERVED_PREFIX: &str = "watchfire:";

/// The default of `CACHE_MAX_VALUE_BYTES`: 16 MiB, the largest cached value (JSON text) a [`Cache`] reads back. A
/// larger value, which only a store another party writes to would hold, reads as an error ([`Cache::get`]) or a
/// miss ([`Cache::remember`]); the file, database and Redis stores check the size before reading the value.
pub const DEFAULT_MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;

/// A stored value larger than the limit (an [`Error::Other`] holding it, so `remember` can tell it apart).
#[derive(Debug)]
pub(crate) struct TooLarge {
    pub(crate) bytes: u64,
    pub(crate) limit: usize,
}

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a cached value holds {} bytes, more than CACHE_MAX_VALUE_BYTES ({})",
            self.bytes, self.limit
        )
    }
}

impl std::error::Error for TooLarge {}

/// The error for a stored value of `bytes` over `limit`.
pub(crate) fn too_large(bytes: u64, limit: usize) -> Error {
    Error::other(TooLarge { bytes, limit })
}

fn is_too_large(e: &Error) -> bool {
    matches!(e, Error::Other(inner) if inner.is::<TooLarge>())
}

/// Whether `flush(prefix, keep)` removes `key`.
pub(crate) fn flushed(key: &str, prefix: &str, keep: &[String]) -> bool {
    key.starts_with(prefix) && !keep.iter().any(|k| key.starts_with(k.as_str()))
}

/// About one call in a hundred (at random): when a write also cleans up expired entries, so stores that do not
/// expire entries on their own (database, file) stay bounded, also when every write uses a new key (`add` of
/// unique keys, e.g. one per scheduled tick).
pub(crate) fn sometimes() -> bool {
    crate::crypto::random_bytes(1)
        .ok()
        .and_then(|b| b.first().copied())
        .is_some_and(|b| b < 3)
}

/// Unix time in milliseconds.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The expiry time (Unix ms) of an entry stored now with `ttl`; `None` = never.
pub(crate) fn expires_at(ttl: Option<Duration>) -> Option<u64> {
    ttl.map(|ttl| {
        let ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1);
        now_ms().saturating_add(ms)
    })
}

/// Whether an entry with this expiry is still live.
pub(crate) fn live(expires: Option<u64>) -> bool {
    expires.is_none_or(|e| e > now_ms())
}

/// `current + by` for a stored counter.
pub(crate) fn add_to(current: Option<&str>, by: i64) -> Result<i64> {
    let current = match current {
        None => 0,
        Some(text) => text
            .trim()
            .parse::<i64>()
            .map_err(|_| Error::internal("the cache entry is not an integer"))?,
    };
    current
        .checked_add(by)
        .ok_or_else(|| Error::internal("the cache counter overflowed"))
}

/// The stores of one app, created on first use.
pub(crate) struct Stores {
    settings: Settings,
    db: Option<Db>,
    open: Mutex<HashMap<String, Arc<dyn CacheStore>>>,
}

impl Stores {
    fn new(settings: Settings, db: Option<Db>) -> Self {
        Self {
            settings,
            db,
            open: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, name: &str) -> Result<Arc<dyn CacheStore>> {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(store) = open.get(name) {
            return Ok(Arc::clone(store));
        }
        let store = self.make(name)?;
        open.insert(name.to_owned(), Arc::clone(&store));
        Ok(store)
    }

    fn make(&self, name: &str) -> Result<Arc<dyn CacheStore>> {
        let s = &self.settings;
        Ok(match name {
            "array" => Arc::new(array::ArrayStore::default()),
            "null" => Arc::new(array::NullStore),
            "memory" => memory::MemoryStore::global(s.cache_memory_capacity),
            "file" => Arc::new(
                file::FileStore::new(s.cache_path.clone()).max_value(s.cache_max_value_bytes),
            ),
            "database" => {
                let db = self.db.clone().ok_or_else(|| {
                    Error::internal(
                        "the database cache store needs a database: set DATABASE_URL, or pick \
                         another CACHE_STORE",
                    )
                })?;
                Arc::new(
                    database::DatabaseStore::new(db, &s.cache_table, s.cache_timeout)
                        .max_value(s.cache_max_value_bytes),
                )
            }
            #[cfg(feature = "redis")]
            "redis" => Arc::new(
                redis::RedisStore::new(&s.redis_url, s.cache_timeout)?
                    .max_value(s.cache_max_value_bytes),
            ),
            #[cfg(not(feature = "redis"))]
            "redis" => {
                return Err(Error::internal(
                    "the redis cache store needs the `redis` feature of smeltery",
                ));
            }
            #[cfg(feature = "memcached")]
            "memcached" => Arc::new(memcached::MemcachedStore::new(
                &s.memcached_servers,
                s.cache_timeout,
            )?),
            #[cfg(not(feature = "memcached"))]
            "memcached" => {
                return Err(Error::internal(
                    "the memcached cache store needs the `memcached` feature of smeltery",
                ));
            }
            other => {
                return Err(Error::internal(format!(
                    "`{other}` is not a cache store (one of: {})",
                    STORES.join(", ")
                )));
            }
        })
    }
}

/// A store that could not be opened: every call fails with the reason.
struct Broken(String);

impl Broken {
    fn fail<'a, T: Send + 'a>(&'a self) -> BoxFuture<'a, Result<T>> {
        Box::pin(async move { Err(Error::internal(self.0.clone())) })
    }
}

impl CacheStore for Broken {
    fn get<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        self.fail()
    }
    fn put<'a>(&'a self, _: &'a str, _: &'a str, _: Option<Duration>) -> BoxFuture<'a, Result<()>> {
        self.fail()
    }
    fn add<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        self.fail()
    }
    fn increment<'a>(&'a self, _: &'a str, _: i64) -> BoxFuture<'a, Result<i64>> {
        self.fail()
    }
    fn forget<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<bool>> {
        self.fail()
    }
    fn flush<'a>(&'a self, _: &'a str, _: &'a [String]) -> BoxFuture<'a, Result<()>> {
        self.fail()
    }
    fn acquire_lock<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        self.fail()
    }
    fn release_lock<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<bool>> {
        self.fail()
    }
    fn refresh_lock<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        self.fail()
    }
    fn force_release_lock<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<()>> {
        self.fail()
    }
    fn lock_owner<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        self.fail()
    }
}

/// A handle to one cache store: cheap to clone, and a handler argument.
///
/// ```
/// use std::time::Duration;
/// use smeltery_core::cache::Cache;
/// use smeltery_core::Result;
///
/// async fn dashboard(cache: Cache) -> Result<String> {
///     let count: u64 = cache
///         .remember("posts.count", Duration::from_secs(60), || async { Ok(12) })
///         .await?;
///     Ok(format!("{count} posts"))
/// }
/// ```
///
/// Keys get the store's prefix (`CACHE_PREFIX`) in front. A value that does not deserialize
/// into the requested type is an error from [`get`](Self::get) and a miss for
/// [`remember`](Self::remember), which then computes and stores it again.
#[derive(Clone)]
pub struct Cache {
    store: Arc<dyn CacheStore>,
    stores: Arc<Stores>,
    name: Arc<str>,
    prefix: Arc<str>,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("store", &self.name)
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl App {
    /// The app's cache, on the default store (`CACHE_STORE`).
    ///
    /// The stores are opened on first use; a default store that cannot be opened (an unknown
    /// name, a missing feature or database) makes every call fail with the reason.
    pub fn cache(&self) -> Cache {
        let stores =
            self.service_or_insert_with(|| Stores::new(self.settings().clone(), self.db().ok()));
        Cache::default_of(stores)
    }
}

impl Cache {
    fn default_of(stores: Arc<Stores>) -> Self {
        let name = stores.settings.cache_store.clone();
        let store = stores
            .get(&name)
            .unwrap_or_else(|e| Arc::new(Broken(e.to_string())));
        Self {
            prefix: stores.settings.cache_prefix.as_str().into(),
            name: name.into(),
            store,
            stores,
        }
    }

    /// A cache on store `store` (one of [`STORES`]) outside any app, with the prefix, paths and
    /// servers from `settings`. The database store needs `db`.
    ///
    /// # Errors
    /// An unknown store name, a store whose feature is off, or the database store without
    /// `db`.
    pub fn open(store: &str, settings: &Settings, db: Option<Db>) -> Result<Self> {
        let mut settings = settings.clone();
        store.clone_into(&mut settings.cache_store);
        let stores = Arc::new(Stores::new(settings, db));
        let opened = stores.get(store)?;
        Ok(Self {
            prefix: stores.settings.cache_prefix.as_str().into(),
            name: store.into(),
            store: opened,
            stores,
        })
    }

    /// The same app's store named `name` (`"redis"`, `"file"` …, see [`STORES`]), with the same
    /// prefix.
    ///
    /// # Errors
    /// An unknown store name, a store whose feature is off, or the database store without a
    /// database.
    pub fn store(&self, name: &str) -> Result<Self> {
        Ok(Self {
            store: self.stores.get(name)?,
            stores: Arc::clone(&self.stores),
            name: name.into(),
            prefix: Arc::clone(&self.prefix),
        })
    }

    /// The same store with another key prefix.
    pub fn with_prefix(&self, prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into().into(),
            ..self.clone()
        }
    }

    /// The store's name.
    pub fn store_name(&self) -> &str {
        &self.name
    }

    /// The key prefix.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn key(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }

    /// The value of `raw`. The error names the key, the type and where the JSON went wrong, never serde's
    /// message itself, which can quote the stored value (it reaches the log through `remember`).
    fn decode<T: DeserializeOwned>(&self, key: &str, raw: &str) -> Result<T> {
        let limit = self.stores.settings.cache_max_value_bytes;
        if raw.len() > limit {
            return Err(too_large(
                u64::try_from(raw.len()).unwrap_or(u64::MAX),
                limit,
            ));
        }
        serde_json::from_str(raw).map_err(|e| {
            let kind = match e.classify() {
                serde_json::error::Category::Io => "an I/O error",
                serde_json::error::Category::Syntax => "invalid JSON",
                serde_json::error::Category::Data => "a value of another shape",
                serde_json::error::Category::Eof => "truncated JSON",
            };
            Error::internal(format!(
                "the cache entry `{key}` is not a `{}`: {kind} at line {} column {}",
                std::any::type_name::<T>(),
                e.line(),
                e.column()
            ))
        })
    }

    /// The value under `key`, or `None` when it is missing or expired.
    ///
    /// # Errors
    /// The store fails, or the value does not deserialize into `T`.
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.store.get(&self.key(key)).await? {
            None => Ok(None),
            Some(raw) => self.decode(key, &raw).map(Some),
        }
    }

    /// Whether a live value is stored under `key`.
    ///
    /// # Errors
    /// The store fails.
    pub async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.store.get(&self.key(key)).await?.is_some())
    }

    /// Store `value` under `key` for `ttl`. A zero `ttl` removes the key instead.
    ///
    /// # Errors
    /// The value does not serialize, or the store fails.
    pub async fn put<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
        ttl: Duration,
    ) -> Result<()> {
        if ttl.is_zero() {
            self.store.forget(&self.key(key)).await?;
            return Ok(());
        }
        let raw = serde_json::to_string(value)?;
        self.store.put(&self.key(key), &raw, Some(ttl)).await
    }

    /// Store `value` under `key` until it is removed.
    ///
    /// # Errors
    /// The value does not serialize, or the store fails.
    pub async fn forever<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        let raw = serde_json::to_string(value)?;
        self.store.put(&self.key(key), &raw, None).await
    }

    /// Store `value` only when `key` holds no live value; `true` when it was stored. Atomic
    /// within the store. A zero `ttl` stores nothing.
    ///
    /// # Errors
    /// The value does not serialize, or the store fails.
    pub async fn add<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
        ttl: Duration,
    ) -> Result<bool> {
        if ttl.is_zero() {
            return Ok(false);
        }
        let raw = serde_json::to_string(value)?;
        self.store.add(&self.key(key), &raw, Some(ttl)).await
    }

    /// The value under `key`; on a miss, `compute` it, store it for `ttl` and return it.
    ///
    /// # Errors
    /// `compute` fails (nothing is stored), or the store fails.
    pub async fn remember<T, F, Fut>(&self, key: &str, ttl: Duration, compute: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        self.remember_inner(key, Some(ttl), compute).await
    }

    /// Like [`remember`](Self::remember), keeping the value until it is removed.
    ///
    /// # Errors
    /// `compute` fails (nothing is stored), or the store fails.
    pub async fn remember_forever<T, F, Fut>(&self, key: &str, compute: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        self.remember_inner(key, None, compute).await
    }

    async fn remember_inner<T, F, Fut>(
        &self,
        key: &str,
        ttl: Option<Duration>,
        compute: F,
    ) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let full = self.key(key);
        let stored = match self.store.get(&full).await {
            // Too large to read: computed again and replaced, like a value of another shape.
            Err(e) if is_too_large(&e) => {
                tracing::warn!(key, error = %e, "recomputing a cache entry");
                None
            }
            other => other?,
        };
        if let Some(raw) = stored {
            match self.decode::<T>(key, &raw) {
                Ok(value) => return Ok(value),
                // A value of another shape (e.g. from before a deploy) is computed again.
                Err(e) => tracing::warn!(error = %e, "recomputing a cache entry"),
            }
        }
        let value = compute().await?;
        if ttl.is_none_or(|t| !t.is_zero()) {
            let raw = serde_json::to_string(&value)?;
            self.store.put(&full, &raw, ttl).await?;
        }
        Ok(value)
    }

    /// Remove `key`; `true` when a live value was there.
    ///
    /// # Errors
    /// The store fails.
    pub async fn forget(&self, key: &str) -> Result<bool> {
        self.store.forget(&self.key(key)).await
    }

    /// The value under `key`, removing it.
    ///
    /// # Errors
    /// The store fails, or the value does not deserialize into `T` (it is removed anyway).
    pub async fn pull<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.store.pull(&self.key(key)).await? {
            None => Ok(None),
            Some(raw) => self.decode(key, &raw).map(Some),
        }
    }

    /// Add `by` to the integer under `key` and return the new value. A missing key starts at 0
    /// and is kept until removed; an existing entry keeps its time to live. Atomic within the
    /// store. On memcached counters do not go below 0 (memcached's own rule).
    ///
    /// # Errors
    /// The entry is not an integer, the counter overflows, or the store fails.
    pub async fn increment(&self, key: &str, by: i64) -> Result<i64> {
        self.store.increment(&self.key(key), by).await
    }

    /// Subtract `by` from the integer under `key` (see [`increment`](Self::increment)).
    ///
    /// # Errors
    /// The entry is not an integer, the counter overflows, or the store fails.
    pub async fn decrement(&self, key: &str, by: i64) -> Result<i64> {
        let by = by
            .checked_neg()
            .ok_or_else(|| Error::internal("the cache counter overflowed"))?;
        self.store.increment(&self.key(key), by).await
    }

    /// Remove every entry and lock under this cache's prefix, except Watchfire's leases and schedule claims
    /// (names starting with [`RESERVED_PREFIX`]): clearing the cache then never lets a singleton agent run in
    /// two processes or a scheduled tick run twice. On memcached, which cannot list keys, it empties the
    /// servers, those included.
    ///
    /// # Errors
    /// The store fails.
    pub async fn flush(&self) -> Result<()> {
        let keep = [
            format!("{}{RESERVED_PREFIX}", self.prefix),
            format!("{}lock:{RESERVED_PREFIX}", self.prefix),
        ];
        self.store.flush(&self.prefix, &keep).await
    }

    /// Like [`flush`](Self::flush), removing Watchfire's leases and claims too (`cache:clear --all`). Running
    /// processes take their agents again within `WATCHFIRE_LEASE_TTL`; meanwhile a singleton agent can run in
    /// two processes, and a tick of the current minute can run again.
    ///
    /// # Errors
    /// The store fails.
    pub async fn flush_all(&self) -> Result<()> {
        self.store.flush(&self.prefix, &[]).await
    }

    /// An atomic lock named `name`, held at most `ttl` (zero: until released). Locks on a
    /// shared store (database, redis, memcached, file) exclude every process using it.
    ///
    /// ```
    /// # async fn demo(cache: smeltery_core::cache::Cache) -> smeltery_core::Result<()> {
    /// use std::time::Duration;
    ///
    /// let lock = cache.lock("import", Duration::from_secs(60));
    /// if lock.block(Duration::from_secs(5)).await? {
    ///     // … the import …
    ///     lock.release().await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn lock(&self, name: &str, ttl: Duration) -> Lock {
        self.restore_lock(name, &owner_token(), ttl)
    }

    /// The lock `name` as held by `owner` (from [`Lock::owner`]), e.g. to release it in
    /// another process or job.
    pub fn restore_lock(&self, name: &str, owner: &str, ttl: Duration) -> Lock {
        Lock {
            store: Arc::clone(&self.store),
            name: format!("{}lock:{name}", self.prefix),
            owner: owner.to_owned(),
            ttl: (!ttl.is_zero()).then_some(ttl),
        }
    }
}

/// A fresh lock owner token: random, unique per lock.
fn owner_token() -> String {
    crate::crypto::random_token(32).unwrap_or_else(|_| {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    })
}

/// An atomic lock from [`Cache::lock`].
#[derive(Clone)]
pub struct Lock {
    store: Arc<dyn CacheStore>,
    name: String,
    owner: String,
    ttl: Option<Duration>,
}

impl std::fmt::Debug for Lock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lock")
            .field("name", &self.name)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl Lock {
    /// Take the lock when it is free; `true` when this owner now holds it.
    ///
    /// # Errors
    /// The store fails.
    pub async fn get(&self) -> Result<bool> {
        self.store
            .acquire_lock(&self.name, &self.owner, self.ttl)
            .await
    }

    /// Try to take the lock until `timeout` has passed (every 100 ms); `true` when taken.
    ///
    /// # Errors
    /// The store fails.
    pub async fn block(&self, timeout: Duration) -> Result<bool> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.get().await? {
                return Ok(true);
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            tokio::time::sleep((deadline - now).min(Duration::from_millis(100))).await;
        }
    }

    /// Hold the lock for its full time to live again, counted from now, when this owner still
    /// holds it; `true` when it did. A lock that expired (or that another owner took since) is
    /// not extended: long work renews its lock with this well within the time to live, and
    /// stops when it answers `false`.
    ///
    /// ```
    /// # async fn demo(cache: smeltery_core::cache::Cache) -> smeltery_core::Result<()> {
    /// use std::time::Duration;
    ///
    /// let lock = cache.lock("import", Duration::from_secs(30));
    /// if lock.get().await? {
    ///     // … a step of the import …
    ///     assert!(lock.refresh().await?, "still ours for another 30 s");
    ///     lock.release().await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// The store fails.
    pub async fn refresh(&self) -> Result<bool> {
        self.store
            .refresh_lock(&self.name, &self.owner, self.ttl)
            .await
    }

    /// Free the lock when this owner holds it; `true` when it did.
    ///
    /// # Errors
    /// The store fails.
    pub async fn release(&self) -> Result<bool> {
        self.store.release_lock(&self.name, &self.owner).await
    }

    /// Free the lock whoever holds it.
    ///
    /// # Errors
    /// The store fails.
    pub async fn force_release(&self) -> Result<()> {
        self.store.force_release_lock(&self.name).await
    }

    /// This lock's owner token (pass it to [`Cache::restore_lock`] elsewhere).
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Who holds the lock now, if anyone.
    ///
    /// # Errors
    /// The store fails.
    pub async fn current_owner(&self) -> Result<Option<String>> {
        self.store.lock_owner(&self.name).await
    }

    /// Whether this owner holds the lock now.
    ///
    /// # Errors
    /// The store fails.
    pub async fn is_owned(&self) -> Result<bool> {
        Ok(self.current_owner().await?.as_deref() == Some(self.owner.as_str()))
    }
}

impl axum::extract::FromRequestParts<App> for Cache {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(app.cache())
    }
}

/// Locks kept in process memory (the array and memory stores).
#[derive(Default)]
pub(crate) struct LockMap {
    locks: Mutex<HashMap<String, (String, Option<u64>)>>,
}

impl LockMap {
    pub(crate) fn acquire(&self, name: &str, owner: &str, ttl: Option<Duration>) -> bool {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        if locks.len() > 1024 {
            locks.retain(|_, (_, expires)| live(*expires));
        }
        if locks.get(name).is_some_and(|(_, expires)| live(*expires)) {
            return false;
        }
        locks.insert(name.to_owned(), (owner.to_owned(), expires_at(ttl)));
        true
    }

    pub(crate) fn release(&self, name: &str, owner: &str) -> bool {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        let held = locks
            .get(name)
            .is_some_and(|(o, expires)| o == owner && live(*expires));
        if held {
            locks.remove(name);
        }
        held
    }

    pub(crate) fn refresh(&self, name: &str, owner: &str, ttl: Option<Duration>) -> bool {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        match locks.get_mut(name) {
            Some((o, expires)) if o == owner && live(*expires) => {
                *expires = expires_at(ttl);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn force_release(&self, name: &str) {
        self.locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
    }

    pub(crate) fn owner(&self, name: &str) -> Option<String> {
        self.locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .filter(|(_, expires)| live(*expires))
            .map(|(owner, _)| owner.clone())
    }

    pub(crate) fn flush(&self, prefix: &str, keep: &[String]) {
        self.locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|name, _| !flushed(name, prefix, keep));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(limit: usize) -> Cache {
        let mut s = Settings::from_env();
        s.cache_max_value_bytes = limit;
        Cache::open("array", &s, None).unwrap()
    }

    #[test]
    fn decode_errors_never_quote_the_cached_value() {
        let cache = cache(DEFAULT_MAX_VALUE_BYTES);
        let err = cache
            .decode::<u32>("profile", "\"SECRET-VALUE\"")
            .unwrap_err()
            .to_string();
        assert!(!err.contains("SECRET-VALUE"), "{err}");
        assert!(err.contains("profile") && err.contains("u32"), "{err}");
        let err = cache
            .decode::<Vec<u8>>("list", "[1, \"SECRET\"")
            .unwrap_err()
            .to_string();
        assert!(!err.contains("SECRET"), "{err}");
    }

    #[test]
    fn oversized_values_are_refused_before_parsing() {
        let cache = cache(64);
        let raw = format!("\"{}\"", "a".repeat(64));
        let err = cache.decode::<String>("big", &raw).unwrap_err();
        assert!(is_too_large(&err), "{err}");
        assert!(err.to_string().contains("CACHE_MAX_VALUE_BYTES"), "{err}");
        let fits = format!("\"{}\"", "a".repeat(16));
        assert_eq!(cache.decode::<String>("small", &fits).unwrap().len(), 16);
        assert_eq!(
            Settings::from_env().cache_max_value_bytes,
            DEFAULT_MAX_VALUE_BYTES
        );
    }

    #[test]
    fn flush_keeps_reserved_names() {
        let keep = [
            "app_watchfire:".to_owned(),
            "app_lock:watchfire:".to_owned(),
        ];
        assert!(flushed("app_users", "app_", &keep));
        assert!(flushed("app_lock:report", "app_", &keep));
        assert!(!flushed("app_watchfire:agent:x", "app_", &keep));
        assert!(!flushed("app_lock:watchfire:agent:x", "app_", &keep));
        assert!(!flushed("other_users", "app_", &keep));
        assert!(flushed("app_watchfire:agent:x", "app_", &[]));
    }
}
