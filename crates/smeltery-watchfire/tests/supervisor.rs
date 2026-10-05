//! Supervision through the public API, on paused Tokio time with the in-memory store.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use smeltery_watchfire::prelude::*;
use smeltery_watchfire::testing::Harness;
use smeltery_watchfire::{Backoff, Error, Health, RunRecord};

/// What a test agent does in each run.
#[derive(Clone, Copy)]
enum Mode {
    /// Tick every second until cancelled.
    Loop,
    /// Fail after `n` seconds.
    FailAfter(u64),
    /// Return Ok after `n` seconds.
    CompleteAfter(u64),
    /// Panic at once.
    Panic,
    /// Ignore cancellation and never return.
    Stubborn,
    /// Wait for cancellation without heartbeating.
    Silent,
}

struct TestAgent {
    mode: Mode,
    runs: Arc<AtomicU32>,
}

fn agent(mode: Mode) -> (TestAgent, Arc<AtomicU32>) {
    let runs = Arc::new(AtomicU32::new(0));
    (
        TestAgent {
            mode,
            runs: Arc::clone(&runs),
        },
        runs,
    )
}

impl Agent for TestAgent {
    fn name(&self) -> String {
        "worker".into()
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            Mode::Loop => {
                let mut ticker = ctx.interval(1.secs());
                while ticker.tick().await {}
                Ok(())
            }
            Mode::FailAfter(n) => {
                ctx.sleep(n.secs()).await;
                Err(AgentError::msg("boom"))
            }
            Mode::CompleteAfter(n) => {
                ctx.sleep(n.secs()).await;
                Ok(())
            }
            Mode::Panic => panic!("agent exploded"),
            Mode::Stubborn => {
                std::future::pending::<()>().await;
                Ok(())
            }
            Mode::Silent => {
                ctx.cancelled().await;
                Ok(())
            }
        }
    }
}

fn outcomes(runs: &[RunRecord]) -> Vec<RunOutcome> {
    runs.iter().map(|r| r.outcome).collect()
}

#[tokio::test(start_paused = true)]
async fn autostart_stop_and_start_again() {
    let (a, runs) = agent(Mode::Loop);
    let mut h = Harness::new(a);
    h.start().await.unwrap();
    h.advance(1.secs()).await;
    assert_eq!(h.state(), AgentState::Running);
    let stopped = h.stop().await.unwrap();
    assert_eq!(stopped.state, AgentState::Stopped);
    assert!(matches!(h.stop().await, Err(Error::NotRunning { .. })));
    h.agents().start("worker").await.unwrap();
    h.advance(1.secs()).await;
    assert_eq!(h.state(), AgentState::Running);
    assert!(matches!(
        h.agents().start("worker").await,
        Err(Error::AlreadyRunning { .. })
    ));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(
        h.transitions(),
        [
            AgentState::Starting,
            AgentState::Running,
            AgentState::Stopping,
            AgentState::Stopped,
            AgentState::Starting,
            AgentState::Running,
        ]
    );
    let records = h.runs().await;
    assert_eq!(
        outcomes(&records),
        [RunOutcome::Stopped, RunOutcome::Running]
    );
    assert_eq!(records[0].run_id, 1);
    assert_eq!(records[1].run_id, 2);
    h.shutdown().await;
    assert_eq!(
        outcomes(&h.runs().await),
        [RunOutcome::Stopped, RunOutcome::Stopped]
    );
    assert!(matches!(
        h.agents().start("worker").await,
        Err(Error::ShuttingDown)
    ));
}

#[tokio::test(start_paused = true)]
async fn on_failure_restarts_with_jittered_exponential_backoff() {
    let (a, runs) = agent(Mode::FailAfter(1));
    let mut h = Harness::new(a)
        .config(AgentConfig::default().backoff(2.secs()..=8.secs()))
        .seed(3);
    h.start().await.unwrap();
    h.advance(1.secs()).await;
    // First failure: backing off at most 2 s.
    assert_eq!(h.state(), AgentState::BackingOff);
    let first = h.next_backoff().unwrap();
    assert!(first <= 2.secs(), "{first:?}");
    let mut ceilings = Vec::new();
    for attempt in 0..6 {
        let delay = h.next_backoff().unwrap();
        let ceiling = Backoff::new(2.secs()..=8.secs()).ceiling(attempt);
        assert!(
            delay <= ceiling,
            "attempt {attempt}: {delay:?} > {ceiling:?}"
        );
        ceilings.push(ceiling);
        // Wait out the delay and the next 1 s run.
        h.advance(delay + 1.secs()).await;
    }
    assert_eq!(ceilings.last(), Some(&8.secs()), "the cap holds");
    assert_eq!(runs.load(Ordering::SeqCst), 7);
    assert_eq!(h.restarts(), 6);
    let records = h.runs().await;
    assert!(records.iter().all(|r| r.outcome == RunOutcome::Failed));
    assert_eq!(records[0].error.as_deref(), Some("boom"));
    assert_eq!(h.status().last_error.as_deref(), Some("boom"));
    h.shutdown().await;
    assert_eq!(h.state(), AgentState::Stopped);
}

#[tokio::test(start_paused = true)]
async fn never_policy_leaves_the_agent_failed() {
    let (a, runs) = agent(Mode::FailAfter(1));
    let mut h = Harness::new(a).config(AgentConfig::default().restart(Restart::Never));
    h.start().await.unwrap();
    h.advance(10.secs()).await;
    assert_eq!(h.state(), AgentState::Failed);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn on_failure_does_not_restart_a_completed_run_but_always_does() {
    let (a, runs) = agent(Mode::CompleteAfter(1));
    let mut h = Harness::new(a);
    h.start().await.unwrap();
    h.advance(10.secs()).await;
    assert_eq!(h.state(), AgentState::Completed);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    h.shutdown().await;

    let (a, runs) = agent(Mode::CompleteAfter(1));
    let mut h = Harness::new(a).config(
        AgentConfig::default()
            .restart(Restart::Always)
            .backoff(1.secs()..=1.secs()),
    );
    h.start().await.unwrap();
    h.advance(10.secs()).await;
    assert!(runs.load(Ordering::SeqCst) >= 5);
    assert!(
        h.runs()
            .await
            .iter()
            .all(|r| matches!(r.outcome, RunOutcome::Completed | RunOutcome::Running))
    );
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn panics_are_recorded_and_restarted() {
    let (a, runs) = agent(Mode::Panic);
    let mut h = Harness::new(a).config(AgentConfig::default().backoff(1.secs()..=1.secs()));
    h.start().await.unwrap();
    h.advance(3.secs()).await;
    assert!(runs.load(Ordering::SeqCst) >= 2);
    let records = h.runs().await;
    assert_eq!(records[0].outcome, RunOutcome::Panicked);
    assert_eq!(
        records[0].error.as_deref(),
        Some("panicked: agent exploded")
    );
    assert!(h.restarts() >= 1);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn max_restarts_in_a_window_fails_the_agent() {
    let (a, runs) = agent(Mode::FailAfter(1));
    let mut h = Harness::new(a).config(
        AgentConfig::default()
            .backoff(1.secs()..=1.secs())
            .max_restarts(3, 1.mins()),
    );
    h.start().await.unwrap();
    h.advance(30.secs()).await;
    assert_eq!(h.state(), AgentState::Failed);
    // The first run plus three restarts.
    assert_eq!(runs.load(Ordering::SeqCst), 4);
    assert!(h.status().last_error.unwrap().contains("restarted 3 times"));
    // Starting it by hand works again.
    h.agents().start("worker").await.unwrap();
    h.advance(100.millis()).await;
    assert_eq!(h.state(), AgentState::Running);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn restarts_spread_beyond_the_window_keep_going() {
    // A run of 40 s, then a failure: at most 2 restarts per 60 s are never exceeded.
    let (a, runs) = agent(Mode::FailAfter(40));
    let mut h = Harness::new(a).config(
        AgentConfig::default()
            .backoff(1.secs()..=1.secs())
            .max_restarts(2, 1.mins()),
    );
    h.start().await.unwrap();
    h.advance(5.mins()).await;
    assert_ne!(h.state(), AgentState::Failed);
    assert!(runs.load(Ordering::SeqCst) >= 6);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn heartbeat_stall_restarts_with_outcome_stalled() {
    let (a, runs) = agent(Mode::Silent);
    let mut h = Harness::new(a).config(
        AgentConfig::default()
            .heartbeat_timeout(10.secs())
            .backoff(1.secs()..=1.secs()),
    );
    h.start().await.unwrap();
    h.advance(5.secs()).await;
    assert_eq!(h.status().health, Health::Healthy);
    h.advance(10.secs()).await;
    let records = h.runs().await;
    assert_eq!(records[0].outcome, RunOutcome::Stalled);
    assert_eq!(records[0].error.as_deref(), Some("no heartbeat for 10s"));
    assert!(
        runs.load(Ordering::SeqCst) >= 2,
        "restarted after the stall"
    );
    assert!(h.transitions().contains(&AgentState::BackingOff));
    h.shutdown().await;

    // A ticking agent never stalls.
    let (a, _) = agent(Mode::Loop);
    let mut h = Harness::new(a).config(AgentConfig::default().heartbeat_timeout(3.secs()));
    h.start().await.unwrap();
    h.advance(1.mins()).await;
    assert_eq!(h.state(), AgentState::Running);
    assert_eq!(h.status().health, Health::Healthy);
    assert!(h.status().last_heartbeat_ms.is_some());
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn pause_and_resume() {
    let (a, runs) = agent(Mode::Loop);
    let mut h = Harness::new(a);
    h.start().await.unwrap();
    h.advance(1.secs()).await;
    let paused = h.agents().pause("worker").await.unwrap();
    assert_eq!(paused.state, AgentState::Paused);
    assert!(matches!(
        h.agents().start("worker").await,
        Err(Error::Paused { .. })
    ));
    h.advance(1.mins()).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "no restart while paused");
    h.agents().resume("worker").await.unwrap();
    h.advance(1.secs()).await;
    assert_eq!(h.state(), AgentState::Running);
    assert!(matches!(
        h.agents().resume("worker").await,
        Err(Error::NotPaused { .. })
    ));
    assert_eq!(outcomes(&h.runs().await)[0], RunOutcome::Stopped);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn commands_while_backing_off() {
    let config = AgentConfig::default()
        .backoff(1.mins()..=1.mins())
        .restart(Restart::Always);
    // stop cancels the pending restart
    let (a, runs) = agent(Mode::CompleteAfter(1));
    let mut h = Harness::new(a).config(config.clone()).seed(1);
    h.start().await.unwrap();
    h.advance(1100.millis()).await;
    while h.state() != AgentState::BackingOff || h.next_backoff() == Some(Duration::ZERO) {
        // A zero jittered delay restarts at once; wait for a real backoff.
        h.advance(1100.millis()).await;
    }
    let before = runs.load(Ordering::SeqCst);
    assert_eq!(h.stop().await.unwrap().state, AgentState::Stopped);
    h.advance(5.mins()).await;
    assert_eq!(runs.load(Ordering::SeqCst), before);

    // start during backoff runs at once; pause during backoff pauses
    h.agents().start("worker").await.unwrap();
    h.advance(1100.millis()).await;
    while h.state() != AgentState::BackingOff || h.next_backoff() == Some(Duration::ZERO) {
        h.advance(1100.millis()).await;
    }
    let before = runs.load(Ordering::SeqCst);
    h.agents().start("worker").await.unwrap();
    h.advance(10.millis()).await;
    assert_eq!(runs.load(Ordering::SeqCst), before + 1);
    h.advance(1100.millis()).await;
    while h.state() != AgentState::BackingOff || h.next_backoff() == Some(Duration::ZERO) {
        h.advance(1100.millis()).await;
    }
    assert_eq!(
        h.agents().pause("worker").await.unwrap().state,
        AgentState::Paused
    );
    h.advance(5.mins()).await;
    assert_eq!(h.state(), AgentState::Paused);

    // restart from paused starts a run; restart while running restarts it
    h.agents().restart("worker").await.unwrap();
    h.advance(10.millis()).await;
    assert_eq!(h.state(), AgentState::Running);
    let restarts = h.restarts();
    h.agents().restart("worker").await.unwrap();
    h.advance(10.millis()).await;
    assert_eq!(h.state(), AgentState::Running);
    assert_eq!(h.restarts(), restarts + 1);
    h.shutdown().await;
    let records = h.runs().await;
    assert!(records.iter().all(|r| r.outcome != RunOutcome::Running));
}

#[tokio::test(start_paused = true)]
async fn stubborn_agent_is_killed_on_stop_and_on_shutdown_within_the_budget() {
    let (a, _) = agent(Mode::Stubborn);
    let mut h = Harness::new(a).config(AgentConfig::default().shutdown_timeout(3.secs()));
    h.start().await.unwrap();
    h.advance(1.secs()).await;
    let started = tokio::time::Instant::now();
    h.stop().await.unwrap();
    assert_eq!(started.elapsed(), 3.secs());
    h.agents().start("worker").await.unwrap();
    h.advance(1.secs()).await;

    // Shutdown: the agent's 3 s timeout fits in the budget.
    let started = tokio::time::Instant::now();
    h.shutdown().await;
    assert_eq!(started.elapsed(), 3.secs());
    let records = h.runs().await;
    assert_eq!(outcomes(&records), [RunOutcome::Killed, RunOutcome::Killed]);
    assert_eq!(records[1].error.as_deref(), Some("did not stop within 3s"));

    // A long agent timeout is capped by the app's budget (minus the recording reserve).
    let (a, _) = agent(Mode::Stubborn);
    let mut h = Harness::new(a)
        .config(AgentConfig::default().shutdown_timeout(1.hours()))
        .shutdown_budget(10.secs());
    h.start().await.unwrap();
    h.advance(1.secs()).await;
    let started = tokio::time::Instant::now();
    h.shutdown().await;
    assert_eq!(started.elapsed(), 8.secs());
    assert_eq!(outcomes(&h.runs().await), [RunOutcome::Killed]);
}

#[tokio::test(start_paused = true)]
async fn shutdown_while_busy_records_exactly_one_outcome_per_run() {
    let mut w = Watchfire::new();
    for i in 0..5_u64 {
        w.run(&format!("busy{i}"), move |ctx| async move {
            // Each takes a while to wind down after cancellation.
            ctx.cancelled().await;
            tokio::time::sleep(Duration::from_millis(100 * i)).await;
            Ok(())
        });
    }
    w.run("ignorer", |_ctx| async {
        std::future::pending::<()>().await;
        Ok(())
    })
    .shutdown_timeout(2.secs());
    w.run("flaky", |ctx| async move {
        ctx.sleep(300.millis()).await;
        Err(AgentError::msg("flake"))
    })
    .backoff(100.millis()..=100.millis());
    let mut h = Harness::from_watchfire(w);
    h.start().await.unwrap();
    h.advance(5.secs()).await;
    h.shutdown().await;
    for name in h.agents().names() {
        let runs = h.runs_of(&name).await;
        assert!(!runs.is_empty(), "{name} ran");
        let mut ids: Vec<u64> = runs.iter().map(|r| r.run_id).collect();
        ids.dedup();
        assert_eq!(ids.len(), runs.len(), "{name}: one record per run");
        assert!(
            runs.iter()
                .all(|r| r.outcome != RunOutcome::Running && r.ended_at_ms.is_some()),
            "{name}: {runs:?}"
        );
        assert_eq!(h.state_of(&name), AgentState::Stopped);
    }
    assert_eq!(h.runs_of("ignorer").await[0].outcome, RunOutcome::Killed);
    assert_eq!(h.runs_of("busy4").await[0].outcome, RunOutcome::Stopped);
}

#[tokio::test(start_paused = true)]
async fn pools_groups_and_the_global_limit() {
    let running = Arc::new(AtomicU32::new(0));
    let peak = Arc::new(AtomicU32::new(0));
    let make = |running: Arc<AtomicU32>, peak: Arc<AtomicU32>| {
        move |_i: usize| {
            let running = Arc::clone(&running);
            let peak = Arc::clone(&peak);
            agent_fn("x", move |ctx: AgentCtx| {
                let running = Arc::clone(&running);
                let peak = Arc::clone(&peak);
                async move {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    ctx.sleep(1.secs()).await;
                    running.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                }
            })
        }
    };
    let mut w = Watchfire::new();
    w.pool(5, "fetcher", make(Arc::clone(&running), Arc::clone(&peak)))
        .group("scrapers")
        .restart(Restart::Always)
        .backoff(1.millis()..=1.millis());
    w.group("scrapers").limit(2);
    let mut h = Harness::from_watchfire(w);
    h.start().await.unwrap();
    assert_eq!(
        h.agents().names(),
        [
            "fetcher#0",
            "fetcher#1",
            "fetcher#2",
            "fetcher#3",
            "fetcher#4"
        ]
    );
    h.advance(10.secs()).await;
    assert_eq!(peak.load(Ordering::SeqCst), 2, "the group limit holds");
    // Mid-run: two run, the others wait in Starting.
    h.advance(500.millis()).await;
    let states: Vec<AgentState> = h.agents().list().iter().map(|s| s.state).collect();
    let running = states.iter().filter(|s| **s == AgentState::Running).count();
    let starting = states
        .iter()
        .filter(|s| **s == AgentState::Starting)
        .count();
    assert_eq!((running, starting), (2, 3), "{states:?}");
    // Every member gets turns.
    for i in 0..5 {
        assert!(!h.runs_of(&format!("fetcher#{i}")).await.is_empty());
    }
    // Stop a waiting one: it has no run to end and stops at once.
    let waiting = h
        .agents()
        .list()
        .into_iter()
        .find(|s| s.state == AgentState::Starting)
        .unwrap();
    let runs_before = h.runs_of(&waiting.name).await.len();
    assert_eq!(
        h.agents().stop(&waiting.name).await.unwrap().state,
        AgentState::Stopped
    );
    assert_eq!(h.runs_of(&waiting.name).await.len(), runs_before);
    h.shutdown().await;

    // The global limit.
    let running = Arc::new(AtomicU32::new(0));
    let peak = Arc::new(AtomicU32::new(0));
    let mut w = Watchfire::new();
    w.pool(4, "a", make(Arc::clone(&running), Arc::clone(&peak)))
        .restart(Restart::Always)
        .backoff(1.millis()..=1.millis());
    let mut h = Harness::from_watchfire(w).max_concurrent(3);
    h.start().await.unwrap();
    h.advance(10.secs()).await;
    assert_eq!(peak.load(Ordering::SeqCst), 3);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn children_are_supervised_and_stopped_with_the_parent_run() {
    let child_runs = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&child_runs);
    let mut w = Watchfire::new();
    w.run("parent", move |ctx| {
        let counter = Arc::clone(&counter);
        async move {
            ctx.spawn_child(
                "helper",
                agent_fn("helper", move |c: AgentCtx| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async move {
                        c.cancelled().await;
                        Ok(())
                    }
                }),
            )
            .await?;
            ctx.cancelled().await;
            Ok(())
        }
    });
    let mut h = Harness::from_watchfire(w);
    h.start().await.unwrap();
    h.advance(1.secs()).await;
    assert_eq!(h.state_of("parent.helper"), AgentState::Running);
    assert_eq!(child_runs.load(Ordering::SeqCst), 1);
    h.stop().await.unwrap();
    h.advance(1.secs()).await;
    assert!(
        !h.agents().names().contains(&"parent.helper".to_owned()),
        "the child is removed with the parent run"
    );
    assert_eq!(
        outcomes(&h.runs_of("parent.helper").await),
        [RunOutcome::Stopped]
    );
    // A new parent run starts a new child.
    h.agents().start("parent").await.unwrap();
    h.advance(1.secs()).await;
    assert_eq!(h.state_of("parent.helper"), AgentState::Running);
    assert_eq!(child_runs.load(Ordering::SeqCst), 2);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn runtime_add_and_remove() {
    let (a, _) = agent(Mode::Loop);
    let mut h = Harness::new(a);
    h.start().await.unwrap();
    let added = h
        .agents()
        .add(agent_fn("extra", |ctx: AgentCtx| async move {
            ctx.cancelled().await;
            Ok(())
        }))
        .await
        .unwrap();
    assert_eq!(added.name, "extra");
    assert!(matches!(
        h.agents()
            .add(agent_fn("extra", |_ctx: AgentCtx| async { Ok(()) }))
            .await,
        Err(Error::Duplicate { .. })
    ));
    assert!(matches!(
        h.agents()
            .add(agent_fn("Bad!", |_ctx: AgentCtx| async { Ok(()) }))
            .await,
        Err(Error::InvalidName { .. })
    ));
    h.advance(1.secs()).await;
    assert_eq!(h.state_of("extra"), AgentState::Running);
    h.agents().remove("extra").await.unwrap();
    assert!(matches!(
        h.agents().status("extra"),
        Err(Error::UnknownAgent { .. })
    ));
    assert_eq!(outcomes(&h.runs_of("extra").await), [RunOutcome::Stopped]);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn checkpoints_survive_restarts_and_counters_land_in_run_records() {
    let mut w = Watchfire::new();
    w.run("crawler", |ctx| async move {
        let page: u32 = ctx.checkpoint_get().await?.unwrap_or(0);
        ctx.counter("pages").add(2);
        ctx.counter("pages").inc();
        ctx.log().info(format!("resuming at page {page}"));
        ctx.checkpoint(&(page + 1)).await?;
        ctx.sleep(1.secs()).await;
        Err(AgentError::msg("crash"))
    })
    .backoff(1.secs()..=1.secs());
    let mut h = Harness::from_watchfire(w);
    h.start().await.unwrap();
    h.advance(6.secs()).await;
    let runs = h.runs().await;
    assert!(runs.len() >= 3);
    assert_eq!(runs[0].counters.get("pages"), Some(&3));
    let logs = h.logs();
    assert_eq!(logs[0].message, "resuming at page 0");
    assert_eq!(logs[1].message, "resuming at page 1");
    assert_eq!(logs[2].message, "resuming at page 2");
    assert_eq!(logs[2].level, "info");
    assert_eq!(logs[2].run_id, 3);
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn emitted_events_reach_on_event_agents() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let mut w = Watchfire::new();
    w.on_event("post.created", "notify", move |_ctx, event| {
        let sink = Arc::clone(&sink);
        async move {
            let id: i64 = event.payload_as::<serde_json::Value>()?["id"]
                .as_i64()
                .unwrap_or_default();
            sink.lock().unwrap().push((event.source.clone(), id));
            Ok(())
        }
    });
    w.run("poster", |ctx| async move {
        ctx.sleep(1.secs()).await;
        ctx.emit("post.created", serde_json::json!({"id": 7}))?;
        ctx.emit("post.deleted", serde_json::json!({"id": 8}))?;
        ctx.cancelled().await;
        Ok(())
    });
    let mut h = Harness::from_watchfire(w);
    h.start().await.unwrap();
    h.advance(2.secs()).await;
    h.agents()
        .emit("post.created", serde_json::json!({"id": 9}))
        .unwrap();
    h.advance(1.secs()).await;
    assert_eq!(
        *seen.lock().unwrap(),
        [("poster".to_owned(), 7), ("app".to_owned(), 9)]
    );
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn every_quick_form_ticks_and_http_goes_through_the_fake() {
    use smeltery_watchfire::http::{FakeResponse, FakeTransport, Method};
    let fake = FakeTransport::new();
    fake.on(
        Method::GET,
        "https://api.example.com/price",
        FakeResponse::status(503),
    )
    .on(
        Method::GET,
        "https://api.example.com/price",
        FakeResponse::json(&serde_json::json!({"price": 42})),
    );
    let mut w = Watchfire::new();
    w.every(10.secs(), "poller", |ctx| async move {
        let res = ctx.http().get("https://api.example.com/price").await?;
        let body: serde_json::Value = res.json().await?;
        ctx.counter("price")
            .add(body["price"].as_i64().unwrap_or(0));
        Ok(())
    });
    w.rate_limit("api.example.com", 1.per_minute());
    let mut h = Harness::from_watchfire(w).http(fake.clone());
    h.start().await.unwrap();
    h.advance(125.secs()).await;
    let requests = fake.requests();
    // 503 then 200 on the first tick (the retry waits for the 1/minute bucket); the later
    // ticks also wait for the bucket: one request per minute.
    assert_eq!(requests.len(), 3);
    // The bucket refills in floating point; allow its rounding.
    for pair in requests.windows(2) {
        let gap = pair[1].at - pair[0].at;
        assert!(
            gap >= 60.secs() && gap <= 60.secs() + 10.millis(),
            "{gap:?}"
        );
    }
    assert!(
        requests[0]
            .headers
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("(smeltery-watchfire)")
    );
    h.shutdown().await;
    assert_eq!(h.runs().await[0].counters.get("price"), Some(&84));
}
