//! The live counter: the `BumpCounter` job adds one in the database and pushes a refresh to the `live_counter`
//! Spark on the dashboard; the schedule dispatches the job every 5 seconds.

use std::time::{Duration, Instant};

use smeltery::db::factory::Factory;
use smeltery::json;
use smeltery::sparks::testing::{BroadcastSpy, TestSpark};
use smeltery::testing::TestApp;
use smeltery::watchfire::prelude::*;
use smeltery::watchfire::testing::{Harness, JobHarness};

use web_demo::app::jobs::bump_counter::BumpCounter;
use web_demo::app::models::Metric;
use web_demo::app::sparks::live_counter;
use web_demo::database::factories::user_factory::UserFactory;

fn value(app: &TestApp) -> i64 {
    app.block_on(Metric::current(&app.db(), live_counter::METRIC))
        .unwrap()
}

#[test]
fn the_job_increments_the_counter_and_pushes_a_refresh() {
    let app = TestApp::new(web_demo::build);
    let mut spy = BroadcastSpy::of(app.app()).expect("the app has Sparks");
    assert_eq!(value(&app), 0);
    app.block_on(async {
        let jobs = JobHarness::for_app(app.app().clone()).await;
        jobs.run(&BumpCounter {}).await.unwrap();
        jobs.run(&BumpCounter {}).await.unwrap();
    });
    assert_eq!(value(&app), 2);
    let pushes = spy.pushes();
    assert_eq!(pushes.len(), 2, "{pushes:?}");
    assert!(pushes.iter().all(|p| p.is_refresh_of("live_counter")));
}

#[test]
fn the_dashboard_shows_the_live_counter_and_refreshes_it() {
    let app = TestApp::new(web_demo::build);
    let user = app.block_on(UserFactory.create(&app.db())).unwrap();
    app.acting_as(user.id);
    app.block_on(Metric::increment(&app.db(), live_counter::METRIC))
        .unwrap();

    let page = app.get("/dashboard").text();
    assert!(page.contains("/_watchfire"), "{page}");
    let mut counter =
        TestSpark::from_html(&page, "live_counter").expect("the dashboard shows the live counter");
    assert_eq!(counter.data()["value"], 1);
    assert!(counter.html().contains(">1</p>"), "{}", counter.html());

    // What the page does on a pushed refresh: a `$refresh` update, which reads the new value.
    app.block_on(async {
        JobHarness::for_app(app.app().clone())
            .await
            .run(&BumpCounter {})
            .await
            .unwrap();
    });
    let res = counter.call("$refresh", json!([])).send(&app);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(counter.data()["value"], 2);
    assert!(counter.html().contains(">2</p>"), "{}", counter.html());
}

#[test]
fn the_live_counter_is_a_streamed_spark() {
    let app = TestApp::new(web_demo::build);
    let user = app.block_on(UserFactory.create(&app.db())).unwrap();
    app.acting_as(user.id);
    let page = app.get("/dashboard").text();
    // Streamed components subscribe the page to `/_sparks/stream`.
    assert!(page.contains("wire:stream"), "{page}");
}

/// The whole registration of `app/agents/mod.rs` on the test database: the schedule dispatches `BumpCounter`
/// every 5 seconds and a queue worker runs it. Real time (the queue lives in SQLite), so this test takes about
/// 5 seconds.
#[test]
fn the_schedule_runs_the_job_every_five_seconds() {
    let app = TestApp::new(web_demo::build);
    app.block_on(async {
        let mut w = Watchfire::new();
        web_demo::app::agents::register(&mut w);
        let mut h = Harness::from_watchfire(w).app(app.app().clone()).workers(1);
        h.start().await.unwrap();
        let started = Instant::now();
        loop {
            let value = Metric::current(&app.db(), live_counter::METRIC)
                .await
                .unwrap();
            if value >= 1 {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the scheduled job never ran"
            );
            h.advance(Duration::from_millis(100)).await;
        }
        assert!(
            started.elapsed() >= Duration::from_secs(4),
            "not before its period"
        );
        let runs = h.runs_of("queue#0").await;
        assert!(
            runs.iter()
                .any(|r| r.job.as_deref() == Some("bump_counter")
                    && r.outcome == RunOutcome::Completed),
            "{runs:?}"
        );
        h.shutdown().await;
    });
}
