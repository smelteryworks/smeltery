//! The poller and the flaky worker under the real Watchfire supervisor, on paused Tokio time (minutes pass in
//! milliseconds) with an in-memory store and a fake HTTP transport: no network, no database.

use std::time::Duration;

use smeltery::watchfire::http::{FakeResponse, FakeTransport, Method};
use smeltery::watchfire::prelude::*;
use smeltery::watchfire::testing::Harness;

use headless_demo::app::agents::flaky::Flaky;
use headless_demo::app::agents::poller::{self, Poller};
use headless_demo::config::agents::{FlakyConfig, PollerConfig};

const URL: &str = "https://sensor.test/reading";

fn poller_config(url: &str) -> PollerConfig {
    PollerConfig {
        url: url.to_owned(),
        every: 30.secs(),
    }
}

fn flaky_config(failure_percent: i64, seed: u64) -> FlakyConfig {
    FlakyConfig {
        failure_percent,
        seed,
        batch: 5.secs(),
        batches_per_run: 6,
    }
}

fn messages(h: &Harness) -> Vec<String> {
    h.logs().into_iter().map(|l| l.message).collect()
}

#[tokio::test(start_paused = true)]
async fn the_poller_simulates_a_value_every_thirty_seconds() {
    let mut h = Harness::new(Poller::new(poller_config("")));
    h.start().await.unwrap();
    h.advance(95.secs()).await;
    let logs = messages(&h);
    // Ticks at 0, 30, 60 and 90 seconds.
    let polls: Vec<&String> = logs.iter().filter(|m| m.starts_with("poll ")).collect();
    assert_eq!(polls.len(), 4, "{logs:?}");
    assert_eq!(
        polls[0],
        &format!("poll 1: {} (simulated)", poller::simulate(0))
    );
    assert_eq!(h.state(), AgentState::Running);
    h.shutdown().await;
    assert_eq!(h.runs().await[0].counters.get("polls"), Some(&4));
}

#[tokio::test(start_paused = true)]
async fn the_poller_resumes_its_count_from_the_checkpoint() {
    let mut h = Harness::new(Poller::new(poller_config("")));
    h.start().await.unwrap();
    h.advance(65.secs()).await; // polls 1, 2, 3
    h.agents().restart(poller::NAME).await.unwrap();
    h.advance(1.secs()).await;
    let logs = messages(&h);
    assert!(
        logs.contains(&"resuming after 3 polls".to_owned()),
        "{logs:?}"
    );
    assert!(
        logs.iter().any(|m| m.starts_with("poll 4: ")),
        "the count goes on after the restart: {logs:?}"
    );
    assert_eq!(h.runs().await.len(), 2);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn the_poller_reads_numbers_over_http() {
    let fake = FakeTransport::new();
    fake.on(Method::GET, URL, FakeResponse::text("21.5")).on(
        Method::GET,
        URL,
        FakeResponse::json(&smeltery::json!({ "value": 22 })),
    );
    let mut h = Harness::new(Poller::new(poller_config(URL))).http(fake.clone());
    h.start().await.unwrap();
    h.advance(35.secs()).await;
    let logs = messages(&h);
    assert!(logs.contains(&"poll 1: 21.5 (http)".to_owned()), "{logs:?}");
    assert!(logs.contains(&"poll 2: 22 (http)".to_owned()), "{logs:?}");
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0].headers.get("user-agent").is_some_and(|v| v
            .to_str()
            .unwrap_or_default()
            .contains("smeltery-watchfire")),
        "the polite client names itself"
    );
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_failing_poll_restarts_the_poller_with_backoff() {
    let fake = FakeTransport::new();
    // 500 four times (the first try and three retries), then a reading.
    for _ in 0..4 {
        fake.on(Method::GET, URL, FakeResponse::status(500));
    }
    fake.on(Method::GET, URL, FakeResponse::text("7"));
    let mut h = Harness::new(Poller::new(poller_config(URL)))
        .http(fake.clone())
        .seed(3);
    h.start().await.unwrap();
    h.advance(2.mins()).await;
    assert!(h.transitions().contains(&AgentState::BackingOff));
    assert_eq!(h.restarts(), 1);
    let runs = h.runs().await;
    assert_eq!(runs[0].outcome, RunOutcome::Failed);
    assert!(
        runs[0].error.as_deref().unwrap_or_default().contains("500"),
        "{runs:?}"
    );
    assert!(messages(&h).contains(&"poll 1: 7 (http)".to_owned()));
    assert_eq!(h.state(), AgentState::Running);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn about_a_third_of_the_flaky_runs_fail_and_every_run_is_restarted() {
    let mut h = Harness::new(Flaky::new(flaky_config(30, 42))).seed(1);
    h.start().await.unwrap();
    h.advance(30.mins()).await;
    let runs = h.runs().await;
    let failed: Vec<_> = runs
        .iter()
        .filter(|r| r.outcome == RunOutcome::Failed)
        .collect();
    let completed = runs
        .iter()
        .filter(|r| r.outcome == RunOutcome::Completed)
        .count();
    assert!(completed >= 10, "{runs:?}");
    #[allow(clippy::cast_precision_loss)]
    let rate = failed.len() as f64 / (failed.len() + completed) as f64;
    assert!(
        (0.1..=0.5).contains(&rate),
        "{} of {} runs failed",
        failed.len(),
        runs.len()
    );
    assert!(failed.iter().all(|r| {
        r.error
            .as_deref()
            .unwrap_or_default()
            .contains("simulated failure")
    }));
    // A completed run has all 6 batches; a failed one stopped at its failing batch.
    assert!(
        runs.iter()
            .filter(|r| r.outcome == RunOutcome::Completed)
            .all(|r| r.counters.get("batches") == Some(&6))
    );
    // Every run ended in a backoff and a restart, and the restart limit was never reached.
    assert!(h.restarts() + 1 >= u64::try_from(runs.len()).unwrap());
    assert!(h.transitions().contains(&AgentState::BackingOff));
    assert_ne!(h.state(), AgentState::Failed);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn the_same_seed_gives_the_same_failures() {
    let mut outcomes = Vec::new();
    for _ in 0..2 {
        let mut h = Harness::new(Flaky::new(flaky_config(30, 7))).seed(5);
        h.start().await.unwrap();
        h.advance(5.mins()).await;
        let runs = h.runs().await;
        outcomes.push(
            runs.iter()
                .map(|r| (r.outcome, r.counters.get("batches").copied()))
                .collect::<Vec<_>>(),
        );
        h.shutdown().await;
    }
    assert_eq!(outcomes[0], outcomes[1]);
}

#[tokio::test(start_paused = true)]
async fn too_many_restarts_in_a_minute_mark_the_worker_failed() {
    // Every run fails at its only batch: a failure every 5 seconds plus at most 1+2+4+8+10 seconds of
    // backoff, so the sixth failure comes within a minute of the first restart.
    let mut config = flaky_config(100, 1);
    config.batches_per_run = 1;
    let mut h = Harness::new(Flaky::new(config)).seed(9);
    h.start().await.unwrap();
    h.advance(2.mins()).await;
    assert_eq!(h.state(), AgentState::Failed);
    assert_eq!(h.restarts(), 5);
    let runs = h.runs().await;
    assert_eq!(runs.len(), 6);
    assert!(runs.iter().all(|r| r.outcome == RunOutcome::Failed));
    assert!(
        h.status()
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("restarted 5 times"),
        "{:?}",
        h.status()
    );
    // Failed stays failed: no more runs.
    h.advance(5.mins()).await;
    assert_eq!(h.runs().await.len(), 6);
    h.shutdown().await;
}

#[test]
fn the_simulated_value_is_a_wave_around_twenty() {
    for poll in 0..100 {
        let v = poller::simulate(poll);
        assert!((15.0..=25.0).contains(&v), "{v}");
    }
    assert_eq!(poller::simulate(0), 20.0);
    assert!(Duration::from_secs(30) == poller_config("").every);
}
