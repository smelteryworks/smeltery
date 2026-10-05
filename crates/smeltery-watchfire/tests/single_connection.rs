//! An agent and its supervisor sharing a database pool of one connection (in-memory SQLite, as in `TestApp`), on
//! real time.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use smeltery_core::AppBuilder;
use smeltery_core::config::Settings;
use smeltery_core::db::migration::Schema;
use smeltery_core::db::prelude::TransactionTrait as _;
use smeltery_watchfire::prelude::*;
use smeltery_watchfire::testing::Harness;
use tokio::sync::mpsc;

/// The supervisor writes the agent's status (a heartbeat changed) while the agent's run waits for the only
/// connection. The run asked first, so the pool hands the connection to the run: the supervisor must keep polling
/// the run while it writes, or the two wait for each other until the pool's acquire timeout (5 s) and the run's
/// query fails.
#[tokio::test]
async fn a_run_waiting_for_the_only_connection_is_polled_while_the_supervisor_writes_its_status() {
    let mut settings = Settings::from_env();
    settings.database_url = "sqlite::memory:".to_owned();
    let app = AppBuilder::new(settings).build().await.unwrap().app;
    let db = app.db().unwrap();
    smeltery_watchfire::migrations::up(&Schema::new(&db))
        .await
        .unwrap();
    db.execute("CREATE TABLE notes (body TEXT)").await.unwrap();

    let (go_tx, mut go_rx) = mpsc::channel::<()>(1);
    let (done_tx, mut done_rx) = mpsc::channel::<(Duration, Result<(), String>)>(1);
    let agent = agent_fn("reader", move |ctx: AgentCtx| {
        let db = ctx.db().unwrap();
        // One run only: the supervisor would call the closure again on a restart.
        let mut go = std::mem::replace(&mut go_rx, mpsc::channel(1).1);
        let done = done_tx.clone();
        async move {
            while let Some(()) = tokio::select! {
                go = go.recv() => go,
                () = ctx.cancelled() => None,
            } {
                // A new heartbeat: the supervisor's next health check writes the status.
                ctx.heartbeat();
                let started = Instant::now();
                let result = db
                    .execute("SELECT count(*) FROM notes")
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string());
                let _ = done.send((started.elapsed(), result)).await;
            }
            Ok(())
        }
    });
    // Health checks every 500 ms (a quarter of the heartbeat timeout).
    let mut h = Harness::new(agent)
        .config(AgentConfig::default().heartbeat_timeout(Duration::from_secs(2)))
        .app(app.clone());
    h.start().await.unwrap();
    // Let the first status writes finish.
    tokio::time::sleep(Duration::from_millis(700)).await;

    // Hold the only connection, then let the run ask for it: the run waits first, the supervisor's status write
    // (at its next health check) second.
    let held = db.conn().begin().await.unwrap();
    go_tx.send(()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let released = Instant::now();
    held.commit().await.unwrap();

    let (waited, result) = tokio::time::timeout(Duration::from_secs(20), done_rx.recv())
        .await
        .expect("the run reports its query")
        .expect("the run is alive");
    let after_release = released.elapsed();
    assert_eq!(result, Ok(()), "the run's query (waited {waited:?})");
    assert!(
        after_release < Duration::from_secs(3),
        "the run got the connection {after_release:?} after it was released"
    );
    // Still the same database.
    db.execute("SELECT count(*) FROM notes").await.unwrap();
    // And the run goes on.
    go_tx.send(()).await.unwrap();
    let (_, result) = tokio::time::timeout(Duration::from_secs(10), done_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result, Ok(()));
    h.shutdown().await;
}
