//! The cache: one conformance suite run against every store.
//!
//! array, null, memory, file and database (SQLite) run here. Redis, memcached, PostgreSQL and
//! MySQL run the same suite when `REDIS_URL`, `MEMCACHED_SERVERS`, `DATABASE_URL_PG` /
//! `DATABASE_URL_MYSQL` point at a server (`cargo test -- --ignored`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smeltery_core::cache::Cache;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use smeltery_core::cache::migrations;
use smeltery_core::config::Settings;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use smeltery_core::db::{Db, migration::Schema};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Profile {
    name: String,
    visits: u32,
    tags: Vec<String>,
}

/// What a store does differently.
#[derive(Clone, Copy)]
struct Caps {
    /// A time to live long enough to see an entry, and how long to wait until it is gone.
    ttl: Duration,
    wait: Duration,
    /// Counters go below zero (not on memcached).
    signed: bool,
    /// `flush` removes only this cache's prefix (not on memcached).
    flush_by_prefix: bool,
}

const FAST: Caps = Caps {
    ttl: Duration::from_millis(400),
    wait: Duration::from_millis(700),
    signed: true,
    flush_by_prefix: true,
};

fn unique(store: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "t_{store}_{}_{}_{}_",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn settings(store: &str) -> Settings {
    let mut s = Settings::from_env();
    s.cache_prefix = unique(store);
    s
}

/// The whole suite: every public operation, on one store.
async fn conformance(cache: Cache, caps: Caps) {
    round_trips(&cache).await;
    ttl_expiry(&cache, caps).await;
    add_semantics(&cache, caps).await;
    remember(&cache).await;
    counters(&cache, caps).await;
    concurrent_add(&cache).await;
    concurrent_increment(&cache).await;
    locks(&cache, caps).await;
    prefixes_and_flush(&cache, caps).await;
    long_and_similar_keys(&cache).await;
    flush_keeps_watchfire_coordination(&cache, caps).await;
}

async fn round_trips(cache: &Cache) {
    let hour = Duration::from_secs(3600);
    let profile = Profile {
        name: "Ada \"Lovelace\"\nsecond line ✓".into(),
        visits: 7,
        tags: vec!["a".into(), "b".into()],
    };
    assert_eq!(cache.get::<Profile>("profile").await.unwrap(), None);
    assert!(!cache.has("profile").await.unwrap());
    cache.put("profile", &profile, hour).await.unwrap();
    assert_eq!(
        cache.get::<Profile>("profile").await.unwrap(),
        Some(profile.clone())
    );
    assert!(cache.has("profile").await.unwrap());

    cache.put("text", "hello", hour).await.unwrap();
    cache.put("number", &42_i64, hour).await.unwrap();
    cache.put("none", &Option::<u8>::None, hour).await.unwrap();
    cache.forever("list", &vec![1, 2, 3]).await.unwrap();
    assert_eq!(
        cache.get::<String>("text").await.unwrap().as_deref(),
        Some("hello")
    );
    assert_eq!(cache.get::<i64>("number").await.unwrap(), Some(42));
    assert_eq!(cache.get::<Option<u8>>("none").await.unwrap(), Some(None));
    assert_eq!(
        cache.get::<Vec<i32>>("list").await.unwrap(),
        Some(vec![1, 2, 3])
    );

    // Overwrite, wrong type, forget, pull.
    cache.put("text", "again", hour).await.unwrap();
    assert_eq!(
        cache.get::<String>("text").await.unwrap().as_deref(),
        Some("again")
    );
    let err = cache.get::<Profile>("text").await.unwrap_err();
    assert!(err.to_string().contains("text"), "{err}");
    assert!(cache.forget("text").await.unwrap());
    assert!(!cache.forget("text").await.unwrap());
    assert_eq!(cache.get::<String>("text").await.unwrap(), None);
    assert_eq!(cache.pull::<i64>("number").await.unwrap(), Some(42));
    assert_eq!(cache.pull::<i64>("number").await.unwrap(), None);

    // A zero time to live removes the key.
    cache.put("list", &vec![9], Duration::ZERO).await.unwrap();
    assert_eq!(cache.get::<Vec<i32>>("list").await.unwrap(), None);

    // Keys a store might choke on.
    let odd = format!("odd key with spaces/%_*?[x]\n{}", "k".repeat(100));
    cache.put(&odd, &1, hour).await.unwrap();
    assert_eq!(cache.get::<i32>(&odd).await.unwrap(), Some(1));
    assert!(cache.forget(&odd).await.unwrap());
}

async fn ttl_expiry(cache: &Cache, caps: Caps) {
    cache.put("short", &1, caps.ttl).await.unwrap();
    cache.forever("long", &2).await.unwrap();
    assert_eq!(cache.get::<i32>("short").await.unwrap(), Some(1));
    tokio::time::sleep(caps.wait).await;
    assert_eq!(cache.get::<i32>("short").await.unwrap(), None);
    assert!(!cache.has("short").await.unwrap());
    assert_eq!(cache.get::<i32>("long").await.unwrap(), Some(2));
    // An expired key can be added again.
    assert!(cache.add("short", &3, caps.ttl).await.unwrap());
    assert_eq!(cache.get::<i32>("short").await.unwrap(), Some(3));
    cache.forget("long").await.unwrap();
}

async fn add_semantics(cache: &Cache, caps: Caps) {
    let hour = Duration::from_secs(3600);
    assert!(cache.add("added", "first", hour).await.unwrap());
    assert!(!cache.add("added", "second", hour).await.unwrap());
    assert_eq!(
        cache.get::<String>("added").await.unwrap().as_deref(),
        Some("first")
    );
    assert!(!cache.add("zero", "x", Duration::ZERO).await.unwrap());
    assert!(!cache.has("zero").await.unwrap());
    let _ = caps;
}

async fn remember(cache: &Cache) {
    let hour = Duration::from_secs(3600);
    let calls = AtomicU32::new(0);
    for _ in 0..3 {
        let value: Profile = cache
            .remember("remembered", hour, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Profile {
                    name: "computed".into(),
                    visits: 1,
                    tags: vec![],
                })
            })
            .await
            .unwrap();
        assert_eq!(value.name, "computed");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A failing computation stores nothing.
    let failed: smeltery_core::Result<u32> = cache
        .remember("failing", hour, || async {
            Err(smeltery_core::Error::internal("boom"))
        })
        .await;
    assert!(failed.is_err());
    assert!(!cache.has("failing").await.unwrap());

    // A value of another shape is computed again and replaced.
    cache.put("shape", "not a number", hour).await.unwrap();
    let n: u64 = cache
        .remember("shape", hour, || async { Ok(5) })
        .await
        .unwrap();
    assert_eq!(n, 5);
    assert_eq!(cache.get::<u64>("shape").await.unwrap(), Some(5));

    let forever: String = cache
        .remember_forever("forever", || async { Ok("kept".to_owned()) })
        .await
        .unwrap();
    assert_eq!(forever, "kept");
    assert_eq!(
        cache.get::<String>("forever").await.unwrap().as_deref(),
        Some("kept")
    );
}

async fn counters(cache: &Cache, caps: Caps) {
    assert_eq!(cache.increment("hits", 1).await.unwrap(), 1);
    assert_eq!(cache.increment("hits", 5).await.unwrap(), 6);
    assert_eq!(cache.decrement("hits", 2).await.unwrap(), 4);
    assert_eq!(cache.get::<i64>("hits").await.unwrap(), Some(4));
    if caps.signed {
        assert_eq!(cache.decrement("hits", 10).await.unwrap(), -6);
        assert_eq!(cache.decrement("below", 3).await.unwrap(), -3);
    } else {
        assert_eq!(cache.decrement("hits", 10).await.unwrap(), 0);
    }
    cache
        .put("word", "abc", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(cache.increment("word", 1).await.is_err());

    // A counter keeps its time to live.
    cache.put("limited", &10, caps.ttl).await.unwrap();
    assert_eq!(cache.increment("limited", 1).await.unwrap(), 11);
    tokio::time::sleep(caps.wait).await;
    assert_eq!(cache.get::<i64>("limited").await.unwrap(), None);
}

async fn concurrent_add(cache: &Cache) {
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..16 {
        let cache = cache.clone();
        tasks.spawn(async move {
            cache
                .add("race", &i, Duration::from_secs(60))
                .await
                .unwrap()
        });
    }
    let won = tasks.join_all().await.into_iter().filter(|w| *w).count();
    assert_eq!(won, 1, "exactly one add wins");
}

async fn concurrent_increment(cache: &Cache) {
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let cache = cache.clone();
        tasks.spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..5 {
                seen.push(cache.increment("together", 1).await.unwrap());
            }
            seen
        });
    }
    let seen: Vec<i64> = tasks.join_all().await.into_iter().flatten().collect();
    let distinct: HashSet<i64> = seen.iter().copied().collect();
    assert_eq!(distinct.len(), 40, "every increment returns its own value");
    assert_eq!(cache.get::<i64>("together").await.unwrap(), Some(40));
}

async fn locks(cache: &Cache, caps: Caps) {
    let minute = Duration::from_secs(60);
    let first = cache.lock("job", minute);
    let second = cache.lock("job", minute);
    assert_ne!(first.owner(), second.owner());
    assert!(first.get().await.unwrap());
    assert!(!second.get().await.unwrap());
    assert!(
        !first.get().await.unwrap(),
        "a held lock is not taken twice"
    );
    assert!(!second.release().await.unwrap(), "only the owner releases");
    assert_eq!(
        first.current_owner().await.unwrap().as_deref(),
        Some(first.owner())
    );
    assert!(first.is_owned().await.unwrap());
    assert!(first.release().await.unwrap());
    assert!(!first.release().await.unwrap());
    assert!(second.get().await.unwrap());

    // Released from elsewhere with the owner token.
    let restored = cache.restore_lock("job", second.owner(), minute);
    assert!(restored.release().await.unwrap());
    assert_eq!(second.current_owner().await.unwrap(), None);

    // A lock and a key of the same name stay apart.
    cache.put("job", "value", minute).await.unwrap();
    assert!(first.get().await.unwrap());
    assert_eq!(
        cache.get::<String>("job").await.unwrap().as_deref(),
        Some("value")
    );
    first.force_release().await.unwrap();
    assert!(second.get().await.unwrap());
    second.release().await.unwrap();

    // Expiry frees a lock nobody released.
    let short = cache.lock("short-lock", caps.ttl);
    assert!(short.get().await.unwrap());
    let other = cache.lock("short-lock", minute);
    assert!(!other.get().await.unwrap());
    tokio::time::sleep(caps.wait).await;
    assert!(other.get().await.unwrap());
    assert!(
        !short.release().await.unwrap(),
        "an expired holder cannot release"
    );
    other.release().await.unwrap();

    // Refresh: only the owner of a live hold extends it, by the full time to live from now.
    let held = cache.lock("renewed", caps.wait);
    let rival = cache.lock("renewed", caps.wait);
    assert!(
        !held.refresh().await.unwrap(),
        "nothing held, nothing refreshed"
    );
    assert!(held.get().await.unwrap());
    assert!(!rival.refresh().await.unwrap(), "only the owner refreshes");
    tokio::time::sleep(caps.wait / 2).await;
    assert!(held.refresh().await.unwrap());
    tokio::time::sleep(caps.wait * 3 / 4).await;
    // Past the first time to live: the refreshed hold still excludes the rival.
    assert!(!rival.get().await.unwrap(), "the refresh extended the hold");
    assert!(held.is_owned().await.unwrap());
    assert!(held.release().await.unwrap());
    let lapsed = cache.lock("lapsed", caps.ttl);
    assert!(lapsed.get().await.unwrap());
    tokio::time::sleep(caps.wait).await;
    assert!(
        !lapsed.refresh().await.unwrap(),
        "an expired hold is not extended"
    );
    let taker = cache.lock("lapsed", minute);
    assert!(taker.get().await.unwrap());
    assert!(!lapsed.refresh().await.unwrap(), "nor a hold taken over");
    assert!(taker.release().await.unwrap());

    // block: gives up after the timeout, or gets the lock once it is released.
    let holder = cache.lock("blocking", minute);
    assert!(holder.get().await.unwrap());
    let waiter = cache.lock("blocking", minute);
    let start = std::time::Instant::now();
    assert!(!waiter.block(Duration::from_millis(250)).await.unwrap());
    assert!(start.elapsed() >= Duration::from_millis(250));
    let releaser = holder.clone();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        releaser.release().await.unwrap()
    });
    assert!(waiter.block(Duration::from_secs(5)).await.unwrap());
    assert!(release.await.unwrap());
    assert!(waiter.release().await.unwrap());

    // Exclusive under concurrency.
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let cache = cache.clone();
        tasks.spawn(async move { cache.lock("crowd", minute).get().await.unwrap() });
    }
    let won = tasks.join_all().await.into_iter().filter(|w| *w).count();
    assert_eq!(won, 1);
    cache.lock("crowd", minute).force_release().await.unwrap();
}

async fn prefixes_and_flush(cache: &Cache, caps: Caps) {
    let hour = Duration::from_secs(3600);
    let other = cache.with_prefix(format!("{}other_", cache.prefix()));
    let third = cache.with_prefix(unique("third"));
    cache.put("shared-name", "mine", hour).await.unwrap();
    third.put("shared-name", "theirs", hour).await.unwrap();
    assert_eq!(
        cache.get::<String>("shared-name").await.unwrap().as_deref(),
        Some("mine")
    );
    assert_eq!(
        third.get::<String>("shared-name").await.unwrap().as_deref(),
        Some("theirs")
    );
    assert_eq!(other.get::<String>("shared-name").await.unwrap(), None);

    other.put("nested", &1, hour).await.unwrap();
    other.flush().await.unwrap();
    assert_eq!(other.get::<i32>("nested").await.unwrap(), None);
    if caps.flush_by_prefix {
        assert!(
            cache.has("shared-name").await.unwrap(),
            "flush keeps other prefixes"
        );
        assert!(third.has("shared-name").await.unwrap());
    }
    cache.flush().await.unwrap();
    assert!(!cache.has("shared-name").await.unwrap());
    assert!(!cache.has("profile").await.unwrap());
    if caps.flush_by_prefix {
        assert!(third.has("shared-name").await.unwrap());
    }
    third.flush().await.unwrap();
}

/// Keys built from user input: any length, and differing only by case or accents.
async fn long_and_similar_keys(cache: &Cache) {
    let hour = Duration::from_secs(3600);
    let head = "search:".to_owned() + &"q".repeat(250);
    let (a, b) = (format!("{head}-a"), format!("{head}-b"));
    cache.put(&a, &1, hour).await.unwrap();
    cache.put(&b, &2, hour).await.unwrap();
    assert_eq!(cache.get::<i32>(&a).await.unwrap(), Some(1));
    assert_eq!(cache.get::<i32>(&b).await.unwrap(), Some(2));
    assert!(!cache.add(&a, &3, hour).await.unwrap());
    assert_eq!(cache.increment(&format!("{head}-n"), 2).await.unwrap(), 2);
    let lock = cache.lock(&a, hour);
    assert!(lock.get().await.unwrap());
    assert!(
        cache.lock(&b, hour).get().await.unwrap(),
        "another long name is another lock"
    );
    assert!(lock.release().await.unwrap());

    for (i, (one, other)) in [
        ("profile:Admin", "profile:admin"),
        ("profile:admin", "profile:ádmin"),
    ]
    .into_iter()
    .enumerate()
    {
        cache.put(one, &1, hour).await.unwrap();
        cache.put(other, &2, hour).await.unwrap();
        assert_eq!(
            cache.get::<i32>(one).await.unwrap(),
            Some(1),
            "{one} / {other}"
        );
        assert_eq!(
            cache.get::<i32>(other).await.unwrap(),
            Some(2),
            "{one} / {other}"
        );
        let first = cache.lock(&format!("{one}-lock-{i}"), hour);
        assert!(first.get().await.unwrap());
        assert!(
            cache
                .lock(&format!("{other}-lock-{i}"), hour)
                .get()
                .await
                .unwrap(),
            "{one} and {other} are two locks"
        );
    }
    cache.flush_all().await.unwrap();
    assert_eq!(
        cache.get::<i32>(&a).await.unwrap(),
        None,
        "long keys flush by prefix"
    );
}

/// `flush` (`cache:clear`) keeps Watchfire's leases and schedule claims; `flush_all` removes them.
async fn flush_keeps_watchfire_coordination(cache: &Cache, caps: Caps) {
    let hour = Duration::from_secs(3600);
    let claim = "watchfire:schedule:report:2026-10-05T10:00";
    assert!(cache.add(claim, "process-a", hour).await.unwrap());
    let lease = cache.lock("watchfire:agent:crawler", hour);
    assert!(lease.get().await.unwrap());
    cache.put("plain", &1, hour).await.unwrap();
    let other_lock = cache.lock("report", hour);
    assert!(other_lock.get().await.unwrap());
    cache.flush().await.unwrap();
    assert!(!cache.has("plain").await.unwrap());
    if caps.flush_by_prefix {
        assert!(cache.has(claim).await.unwrap(), "the claim survives");
        assert!(lease.is_owned().await.unwrap(), "the lease survives");
        assert!(
            cache.lock("report", hour).get().await.unwrap(),
            "other locks are cleared"
        );
    }
    cache.flush_all().await.unwrap();
    assert!(!cache.has(claim).await.unwrap());
    assert!(!lease.is_owned().await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn array_store() {
    let cache = Cache::open("array", &settings("array"), None).unwrap();
    assert_eq!(cache.store_name(), "array");
    conformance(cache, FAST).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn array_stores_are_separate() {
    let s = settings("array");
    let a = Cache::open("array", &s, None).unwrap();
    let b = Cache::open("array", &s, None).unwrap();
    a.forever("k", &1).await.unwrap();
    assert_eq!(b.get::<i32>("k").await.unwrap(), None);
    // Another handle on the same store shares it.
    assert_eq!(
        a.store("array").unwrap().get::<i32>("k").await.unwrap(),
        Some(1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_store() {
    let cache = Cache::open("memory", &settings("memory"), None).unwrap();
    conformance(cache, FAST).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_store_is_process_wide() {
    let a = Cache::open("memory", &settings("memory"), None).unwrap();
    let b = Cache::open("memory", &Settings::from_env(), None)
        .unwrap()
        .with_prefix(a.prefix());
    a.forever("shared", &5).await.unwrap();
    assert_eq!(b.get::<i32>("shared").await.unwrap(), Some(5));
    a.flush().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = settings("file");
    s.cache_path = dir.path().join("cache");
    let cache = Cache::open("file", &s, None).unwrap();
    conformance(cache, FAST).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_store_shares_entries_and_survives_corrupt_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = settings("file");
    s.cache_path = dir.path().to_path_buf();
    // Two stores on one directory stand for two processes.
    let a = Cache::open("file", &s, None).unwrap();
    let b = Cache::open("file", &s, None).unwrap();
    a.forever("k", "v").await.unwrap();
    assert_eq!(b.get::<String>("k").await.unwrap().as_deref(), Some("v"));

    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..16 {
        let cache = if i % 2 == 0 { a.clone() } else { b.clone() };
        tasks.spawn(async move {
            cache
                .add("race", &i, Duration::from_secs(60))
                .await
                .unwrap()
        });
    }
    assert_eq!(tasks.join_all().await.into_iter().filter(|w| *w).count(), 1);
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..8 {
        let cache = if i % 2 == 0 { a.clone() } else { b.clone() };
        tasks.spawn(async move {
            for _ in 0..5 {
                cache.increment("n", 1).await.unwrap();
            }
        });
    }
    tasks.join_all().await;
    assert_eq!(a.get::<i64>("n").await.unwrap(), Some(40));

    // Garbage in every entry file: reads miss, writes repair.
    let mut files = 0;
    for shard in std::fs::read_dir(dir.path()).unwrap().flatten() {
        if shard.file_name().len() != 2 {
            continue;
        }
        for file in std::fs::read_dir(shard.path()).unwrap().flatten() {
            std::fs::write(file.path(), b"\xff\xfe not an entry").unwrap();
            files += 1;
        }
    }
    assert!(files >= 3);
    assert_eq!(a.get::<String>("k").await.unwrap(), None);
    assert_eq!(a.increment("n", 1).await.unwrap(), 1);
    a.forever("k", "again").await.unwrap();
    assert_eq!(
        b.get::<String>("k").await.unwrap().as_deref(),
        Some("again")
    );
    // A stale lock file from a crashed process does not block forever: it is just old.
    a.flush().await.unwrap();
    assert_eq!(a.get::<String>("k").await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn file_store_lock_files_hold_under_heavy_contention() {
    // Four stores on one directory stand for four processes: only the per-key lock files exclude them from each
    // other, so lock files are created and removed constantly while reads run beside the writes.
    let dir = tempfile::tempdir().unwrap();
    let mut s = settings("file");
    s.cache_path = dir.path().to_path_buf();
    let stores: Vec<Cache> = (0..4)
        .map(|_| Cache::open("file", &s, None).unwrap())
        .collect();
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..32 {
        let cache = stores[i % stores.len()].clone();
        tasks.spawn(async move {
            for _ in 0..20 {
                cache.increment("hot", 1).await.unwrap();
                let _ = cache.get::<i64>("hot").await.unwrap();
            }
        });
    }
    tasks.join_all().await;
    assert_eq!(stores[0].get::<i64>("hot").await.unwrap(), Some(640));
}

#[cfg(feature = "sqlite")]
async fn sqlite(url: &str) -> Db {
    let db = Db::connect(url).await.unwrap();
    migrations::up(&Schema::new(&db)).await.unwrap();
    db
}

#[cfg(feature = "sqlite")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn database_store_sqlite_memory() {
    let db = sqlite("sqlite::memory:").await;
    let cache = Cache::open("database", &settings("db"), Some(db)).unwrap();
    conformance(cache, FAST).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn database_store_sqlite_file() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("cache.sqlite").display()
    );
    let db = sqlite(&url).await;
    let cache = Cache::open("database", &settings("db"), Some(db.clone())).unwrap();
    conformance(cache.clone(), FAST).await;

    // A row that is not JSON: `get` reports it, `remember` replaces it.
    cache.forever("broken", &1).await.unwrap();
    db.execute(&format!(
        "UPDATE cache SET value = 'not json' WHERE key = '{}broken'",
        cache.prefix()
    ))
    .await
    .unwrap();
    assert!(cache.get::<i32>("broken").await.is_err());
    let n: i32 = cache
        .remember("broken", Duration::from_secs(60), || async { Ok(3) })
        .await
        .unwrap();
    assert_eq!(n, 3);

    migrations::down(&Schema::new(&db)).await.unwrap();
    assert!(!Schema::new(&db).has_table("cache").await.unwrap());
    assert!(!Schema::new(&db).has_table("cache_locks").await.unwrap());
}

#[tokio::test]
async fn database_store_needs_a_database() {
    let err = Cache::open("database", &settings("db"), None).unwrap_err();
    assert!(err.to_string().contains("DATABASE_URL"), "{err}");
    let err = Cache::open("nope", &settings("x"), None).unwrap_err();
    assert!(err.to_string().contains("not a cache store"), "{err}");
}

#[tokio::test]
async fn null_store_stores_nothing() {
    let cache = Cache::open("null", &settings("null"), None).unwrap();
    let hour = Duration::from_secs(3600);
    cache.put("k", &1, hour).await.unwrap();
    cache.forever("k", &1).await.unwrap();
    assert_eq!(cache.get::<i32>("k").await.unwrap(), None);
    assert!(!cache.has("k").await.unwrap());
    assert!(!cache.add("k", &1, hour).await.unwrap());
    assert!(!cache.forget("k").await.unwrap());
    assert_eq!(cache.pull::<i32>("k").await.unwrap(), None);
    assert_eq!(cache.increment("n", 3).await.unwrap(), 3);
    assert_eq!(cache.increment("n", 3).await.unwrap(), 3);
    let calls = AtomicU32::new(0);
    for _ in 0..2 {
        let v: u32 = cache
            .remember("r", hour, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            })
            .await
            .unwrap();
        assert_eq!(v, 1);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2, "nothing is remembered");
    let lock = cache.lock("l", hour);
    assert!(lock.get().await.unwrap());
    assert!(
        cache.lock("l", hour).get().await.unwrap(),
        "every lock is granted"
    );
    assert!(lock.refresh().await.unwrap(), "and refreshed");
    assert!(lock.release().await.unwrap());
    cache.flush().await.unwrap();
}

#[cfg(feature = "redis")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs REDIS_URL"]
async fn redis_store() {
    let mut s = settings("redis");
    s.redis_url = std::env::var("REDIS_URL").expect("REDIS_URL");
    let cache = Cache::open("redis", &s, None).unwrap();
    conformance(cache, FAST).await;
}

#[cfg(feature = "redis")]
#[tokio::test]
async fn redis_store_reports_an_unreachable_server() {
    let mut s = settings("redis");
    // A port nothing listens on: the call fails within the timeout instead of hanging.
    s.redis_url = "redis://127.0.0.1:1".into();
    s.cache_timeout = Duration::from_secs(2);
    let cache = Cache::open("redis", &s, None).unwrap();
    assert!(cache.get::<i32>("k").await.is_err());
}

#[cfg(feature = "memcached")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs MEMCACHED_SERVERS"]
async fn memcached_store() {
    let mut s = settings("memcached");
    s.memcached_servers = std::env::var("MEMCACHED_SERVERS").expect("MEMCACHED_SERVERS");
    let cache = Cache::open("memcached", &s, None).unwrap();
    // Memcached counts expiry in whole seconds and keeps unsigned counters.
    let caps = Caps {
        ttl: Duration::from_secs(1),
        wait: Duration::from_millis(2500),
        signed: false,
        flush_by_prefix: false,
    };
    conformance(cache, caps).await;
}

#[cfg(feature = "memcached")]
#[tokio::test]
async fn memcached_store_reports_an_unreachable_server() {
    let mut s = settings("memcached");
    s.memcached_servers = "127.0.0.1:1".into();
    s.cache_timeout = Duration::from_secs(2);
    let cache = Cache::open("memcached", &s, None).unwrap();
    assert!(cache.get::<i32>("k").await.is_err());
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL_PG"]
async fn database_store_postgres() {
    let db = Db::connect(&std::env::var("DATABASE_URL_PG").expect("DATABASE_URL_PG"))
        .await
        .unwrap();
    let schema = Schema::new(&db);
    migrations::down(&schema).await.unwrap();
    migrations::up(&schema).await.unwrap();
    let cache = Cache::open("database", &settings("pg"), Some(db)).unwrap();
    conformance(cache, FAST).await;
}

#[cfg(feature = "mysql")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL_MYSQL"]
async fn database_store_mysql() {
    let db = Db::connect(&std::env::var("DATABASE_URL_MYSQL").expect("DATABASE_URL_MYSQL"))
        .await
        .unwrap();
    let schema = Schema::new(&db);
    migrations::down(&schema).await.unwrap();
    migrations::up(&schema).await.unwrap();
    let cache = Cache::open("database", &settings("mysql"), Some(db)).unwrap();
    conformance(cache, FAST).await;
}

#[test]
fn test_apps_use_the_array_store_and_handlers_take_a_cache() {
    use smeltery_core::testing::TestApp;

    async fn visit(cache: Cache) -> smeltery_core::Result<String> {
        Ok(cache.increment("visits", 1).await?.to_string())
    }

    let app = TestApp::new(|app| {
        app.routes(|r| {
            r.get("/visit", visit);
        })
    });
    assert_eq!(app.get("/visit").text(), "1");
    assert_eq!(app.get("/visit").text(), "2");
    assert_eq!(app.app().cache().store_name(), "array");
    let count = app.block_on(async { app.app().cache().get::<i64>("visits").await.unwrap() });
    assert_eq!(count, Some(2));
    // A second app has its own array store.
    let other = TestApp::new(|app| app);
    assert_eq!(
        other.block_on(async { other.app().cache().get::<i64>("visits").await.unwrap() }),
        None
    );
}

#[tokio::test]
async fn cache_clear_flushes_the_default_or_named_store() {
    use smeltery_core::AppBuilder;
    use smeltery_core::console::dispatch;

    let dir = tempfile::tempdir().unwrap();
    let mut builder = AppBuilder::new(settings("console"));
    builder.settings_mut().cache_store = "file".into();
    builder.settings_mut().cache_path = dir.path().to_path_buf();
    let mut s = builder.settings().clone();
    s.cache_store = "file".into();
    let cache = Cache::open("file", &s, None).unwrap();
    cache.forever("k", &1).await.unwrap();

    let mut out = Vec::new();
    let code = dispatch(builder, &["cache:clear".to_owned()], &mut out)
        .await
        .unwrap();
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Cleared the `file` cache store.\n"
    );
    assert_eq!(cache.get::<i32>("k").await.unwrap(), None);

    let mut out = Vec::new();
    let builder = AppBuilder::new(settings("console"));
    dispatch(
        builder,
        &["cache:clear".to_owned(), "array".to_owned()],
        &mut out,
    )
    .await
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Cleared the `array` cache store.\n"
    );

    let builder = AppBuilder::new(settings("console"));
    let err = dispatch(
        builder,
        &["cache:clear".to_owned(), "nope".to_owned()],
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("not a cache store"));
}

#[tokio::test]
async fn cache_clear_keeps_watchfire_leases_unless_all() {
    use smeltery_core::AppBuilder;
    use smeltery_core::console::dispatch;

    let dir = tempfile::tempdir().unwrap();
    let mut s = settings("clear");
    s.cache_store = "file".into();
    s.cache_path = dir.path().to_path_buf();
    let cache = Cache::open("file", &s, None).unwrap();
    let hour = Duration::from_secs(3600);
    let lease = cache.lock("watchfire:agent:crawler", hour);
    assert!(lease.get().await.unwrap());
    assert!(
        cache
            .add("watchfire:schedule:job:1", "p", hour)
            .await
            .unwrap()
    );
    cache.forever("k", &1).await.unwrap();

    let run = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let builder = AppBuilder::new(s.clone());
        async move {
            let mut out = Vec::new();
            let code = dispatch(builder, &args, &mut out).await.unwrap();
            (code, String::from_utf8(out).unwrap())
        }
    };
    let (code, _) = run(&["cache:clear"]).await;
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(cache.get::<i32>("k").await.unwrap(), None);
    assert!(lease.is_owned().await.unwrap(), "the agent's lease is kept");
    assert!(cache.has("watchfire:schedule:job:1").await.unwrap());

    let (code, text) = run(&["cache:clear", "--all"]).await;
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert!(text.contains("leases and claims included"), "{text}");
    assert!(!lease.is_owned().await.unwrap());
    assert!(!cache.has("watchfire:schedule:job:1").await.unwrap());

    // Memcached cannot keep some keys while flushing: without --all nothing is touched.
    #[cfg(feature = "memcached")]
    {
        let mut out = Vec::new();
        let mut m = s.clone();
        m.memcached_servers = "127.0.0.1:1".into();
        let code = dispatch(
            AppBuilder::new(m),
            &["cache:clear".to_owned(), "memcached".to_owned()],
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(code, std::process::ExitCode::FAILURE);
        assert!(String::from_utf8(out).unwrap().contains("--all"));
    }
}

#[tokio::test]
async fn an_unusable_default_store_fails_every_call_with_the_reason() {
    use smeltery_core::AppBuilder;
    let mut builder = AppBuilder::new(settings("broken"));
    builder.settings_mut().cache_store = "database".into();
    builder.settings_mut().database_url = String::new();
    let app = builder.build().await.unwrap().app;
    let err = app.cache().get::<i32>("k").await.unwrap_err();
    assert!(err.to_string().contains("DATABASE_URL"), "{err}");
    assert!(app.cache().store("array").is_ok());
}

#[test]
fn settings_have_cache_defaults() {
    let s = Settings::from_env();
    assert!(s.cache_prefix.ends_with("_cache_"));
    assert!(s.cache_path.ends_with("storage/framework/cache"));
    assert_eq!(s.cache_table, "cache");
    assert_eq!(s.cache_memory_capacity, 10_000);
    assert_eq!(s.cache_timeout, Duration::from_secs(5));
    let debug = format!("{s:?}");
    assert!(debug.contains("cache_store"));
    let _ = Arc::new(s);
}

/// A store opened with `CACHE_MAX_VALUE_BYTES=1000`: a larger stored value is an error for `get` (without its
/// text) and a miss for `remember`, which stores a fresh one.
async fn oversized_values(cache: Cache) {
    let hour = Duration::from_secs(3600);
    let big = format!("SECRET{}", "x".repeat(2000));
    cache.put("big", &big, hour).await.unwrap();
    let err = cache.get::<String>("big").await.unwrap_err().to_string();
    assert!(err.contains("CACHE_MAX_VALUE_BYTES"), "{err}");
    assert!(!err.contains("SECRET"), "{err}");
    let fresh: String = cache
        .remember("big", hour, || async { Ok("small".to_owned()) })
        .await
        .unwrap();
    assert_eq!(fresh, "small");
    assert_eq!(
        cache.get::<String>("big").await.unwrap().as_deref(),
        Some("small")
    );
    cache.put("fits", &"y".repeat(900), hour).await.unwrap();
    assert!(cache.get::<String>("fits").await.unwrap().is_some());
}

fn small_limit(store: &str) -> Settings {
    let mut s = settings(store);
    s.cache_max_value_bytes = 1000;
    s
}

#[tokio::test]
async fn oversized_values_array_and_file() {
    oversized_values(Cache::open("array", &small_limit("array"), None).unwrap()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = small_limit("file");
    s.cache_path = dir.path().to_path_buf();
    oversized_values(Cache::open("file", &s, None).unwrap()).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn oversized_values_database() {
    let db = sqlite("sqlite::memory:").await;
    oversized_values(Cache::open("database", &small_limit("db"), Some(db)).unwrap()).await;
}

#[cfg(feature = "redis")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs REDIS_URL"]
async fn oversized_values_redis() {
    let mut s = small_limit("redis");
    s.redis_url = std::env::var("REDIS_URL").expect("REDIS_URL");
    oversized_values(Cache::open("redis", &s, None).unwrap()).await;
}
