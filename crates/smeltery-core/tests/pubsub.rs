//! PubSub between two apps (two "processes" in one test): the `database` driver on SQLite here; PostgreSQL, MySQL
//! and Redis run the same suite when `DATABASE_URL_PG`, `DATABASE_URL_MYSQL` or `REDIS_URL` point at a server
//! (`cargo test -- --ignored`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
// Without a database or Redis feature every test is compiled out; the helpers stay for the other builds.
#![cfg_attr(
    not(any(
        feature = "sqlite",
        feature = "postgres",
        feature = "mysql",
        feature = "redis"
    )),
    allow(dead_code, unused_imports)
)]

use std::time::Duration;

use serde_json::json;
use smeltery_core::config::Settings;
use smeltery_core::pubsub::{Driver, Forward, PubSub, Subscription};
use smeltery_core::{App, AppBuilder};

fn settings(database_url: &str, driver: &str) -> Settings {
    let mut s = Settings::from_env();
    s.key = "pubsub-it-key-0123456789abcdef0123".into();
    s.database_url = database_url.into();
    s.pubsub_driver = driver.into();
    s.cache_store = "array".into();
    s.pubsub_poll_interval = Duration::from_millis(20);
    s
}

async fn recv(sub: &mut Subscription) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(10), sub.recv())
        .await
        .expect("a message within 10 s")
        .expect("open")
        .payload
        .clone()
}

/// Two `work` processes: what one publishes or forwards reaches the other, in order.
async fn two_processes(settings: Settings, expected: Driver) {
    let a = AppBuilder::new(settings.clone()).build().await.unwrap().app;
    if let Ok(db) = a.db() {
        let schema = smeltery_core::db::migration::Schema::new(&db);
        smeltery_core::pubsub::migrations::down(&schema)
            .await
            .unwrap();
        smeltery_core::pubsub::migrations::up(&schema)
            .await
            .unwrap();
    }
    let b = AppBuilder::new(settings).build().await.unwrap().app;
    for app in [&a, &b] {
        assert!(app.start_background().await.unwrap().is_none());
    }
    let (pa, pb) = (PubSub::of(&a).unwrap(), PubSub::of(&b).unwrap());
    assert_eq!(pa.driver(), Some(expected));
    let mut on_b = pb.subscribe("orders");
    // The receiver connects (Redis) or takes its starting point (database).
    tokio::time::sleep(Duration::from_millis(500)).await;
    for n in 0..20 {
        pa.publish("orders", &json!({ "n": n })).await.unwrap();
    }
    for n in 0..20 {
        assert_eq!(recv(&mut on_b).await["n"], n);
    }
    assert_eq!(pa.forward("orders", &json!("forwarded")), Forward::Queued);
    assert_eq!(recv(&mut on_b).await, json!("forwarded"));
    stop(&a).await;
    stop(&b).await;
}

async fn stop(app: &App) {
    app.shutdown();
}

#[cfg(feature = "sqlite")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_driver_on_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path()
            .join("db.sqlite")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    two_processes(settings(&url, "auto"), Driver::Database).await;
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL_PG"]
async fn database_driver_on_postgres() {
    let url = std::env::var("DATABASE_URL_PG").expect("DATABASE_URL_PG");
    two_processes(settings(&url, "database"), Driver::Database).await;
}

#[cfg(feature = "mysql")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL_MYSQL"]
async fn database_driver_on_mysql() {
    let url = std::env::var("DATABASE_URL_MYSQL").expect("DATABASE_URL_MYSQL");
    two_processes(settings(&url, "database"), Driver::Database).await;
}

#[cfg(feature = "redis")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs REDIS_URL"]
async fn redis_driver() {
    let mut s = settings("", "redis");
    s.redis_url = std::env::var("REDIS_URL").expect("REDIS_URL");
    s.cache_prefix = format!("t_pubsub_{}_", std::process::id());
    two_processes(s, Driver::Redis).await;
}

/// Under `auto` with Redis as the cache store, a `work` process picks Redis; with the server down, a publish
/// reports the error within the driver timeout instead of hanging.
#[cfg(feature = "redis")]
#[tokio::test]
async fn an_unreachable_redis_is_an_error_not_a_hang() {
    let mut s = settings("", "auto");
    s.cache_store = "redis".into();
    s.redis_url = "redis://127.0.0.1:1".into();
    let app = AppBuilder::new(s).build().await.unwrap().app;
    app.start_background().await.unwrap();
    let pubsub = PubSub::of(&app).unwrap();
    assert_eq!(pubsub.driver(), Some(Driver::Redis));
    let started = std::time::Instant::now();
    assert!(pubsub.publish("t", &json!(1)).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
    app.shutdown();
}
