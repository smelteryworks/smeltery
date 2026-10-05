//! Jobs and the queue: memory driver on paused time, database driver (SQLite) on real time.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smeltery_core::config::Settings;
use smeltery_core::db::Db;
use smeltery_core::db::migration::Schema;
use smeltery_core::db::prelude::ConnectionTrait as _;
use smeltery_core::{App, AppBuilder};
use smeltery_watchfire::prelude::*;
use smeltery_watchfire::testing::{Harness, JobHarness};
use smeltery_watchfire::{Queue, RunRecord};

static RAN: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

#[derive(Serialize, Deserialize)]
struct Hello {
    who: String,
}

impl Job for Hello {
    const NAME: &'static str = "hello";

    async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
        ctx.log().info(format!("hello {}", self.who));
        ctx.counter("greeted").inc();
        RAN.lock().unwrap().push((self.who.clone(), ctx.attempt()));
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Flaky {
    fail_times: u32,
    tag: String,
}

impl Job for Flaky {
    const NAME: &'static str = "flaky";

    fn max_attempts(&self) -> u32 {
        3
    }

    fn backoff(&self, attempt: u32) -> Duration {
        Duration::from_secs(10 * u64::from(attempt))
    }

    async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
        RAN.lock().unwrap().push((self.tag.clone(), ctx.attempt()));
        if ctx.attempt() <= self.fail_times {
            return Err(AgentError::msg(format!("attempt {} failed", ctx.attempt())));
        }
        Ok(())
    }
}

fn ran(tag: &str) -> Vec<u32> {
    RAN.lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == tag)
        .map(|(_, a)| *a)
        .collect()
}

fn register(w: &mut Watchfire) {
    w.job::<Hello>();
    w.job::<Flaky>();
}

fn harness() -> Harness {
    let mut w = Watchfire::new();
    register(&mut w);
    Harness::from_watchfire(w).workers(2)
}

async fn worker_runs(h: &Harness) -> Vec<RunRecord> {
    let mut runs = h.runs_of("queue#0").await;
    runs.extend(h.runs_of("queue#1").await);
    runs.retain(|r| r.job.is_some());
    runs
}

#[tokio::test(start_paused = true)]
async fn dispatch_and_delayed_dispatch() {
    let mut h = harness();
    h.start().await.unwrap();
    assert_eq!(h.agents().names(), ["queue#0", "queue#1"]);
    let app = h.app_handle().clone();
    Hello { who: "ada".into() }.dispatch(&app).await.unwrap();
    Hello { who: "bob".into() }
        .dispatch_later(&app, 30.secs())
        .await
        .unwrap();
    h.advance(1.secs()).await;
    assert_eq!(ran("ada"), [1]);
    assert!(ran("bob").is_empty(), "not yet due");
    h.advance(31.secs()).await;
    assert_eq!(ran("bob"), [1]);
    let runs = worker_runs(&h).await;
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|r| r.job.as_deref() == Some("hello")
        && r.outcome == RunOutcome::Completed
        && r.counters.get("greeted") == Some(&1)));
    let stats = h.agents().queue().unwrap().stats().await.unwrap();
    assert_eq!((stats.pending, stats.reserved, stats.dead), (0, 0, 0));
    h.shutdown().await;
}

/// Each job is a run of its worker, so the worker's run count covers the job runs: it equals the highest run id in
/// the worker's history (it counted only the worker's own run before).
#[tokio::test(start_paused = true)]
async fn job_runs_count_in_the_workers_run_count() {
    let mut h = harness();
    h.start().await.unwrap();
    let app = h.app_handle().clone();
    for who in ["c1", "c2", "c3"] {
        Hello { who: who.into() }.dispatch(&app).await.unwrap();
    }
    h.advance(2.secs()).await;
    assert_eq!(worker_runs(&h).await.len(), 3);
    for worker in ["queue#0", "queue#1"] {
        let highest = h
            .runs_of(worker)
            .await
            .iter()
            .map(|r| r.run_id)
            .max()
            .unwrap();
        assert_eq!(h.agents().status(worker).unwrap().runs, highest, "{worker}");
    }
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn failures_retry_with_backoff_then_dead_letter() {
    let mut h = harness();
    h.start().await.unwrap();
    let app = h.app_handle().clone();
    Flaky {
        fail_times: 1,
        tag: "once".into(),
    }
    .dispatch(&app)
    .await
    .unwrap();
    Flaky {
        fail_times: 99,
        tag: "always".into(),
    }
    .dispatch(&app)
    .await
    .unwrap();
    h.advance(1.secs()).await;
    assert_eq!(ran("once"), [1]);
    assert_eq!(ran("always"), [1]);
    // Retry after 10 s (attempt 1 failed).
    h.advance(8.secs()).await;
    assert_eq!(ran("once"), [1]);
    h.advance(3.secs()).await;
    assert_eq!(ran("once"), [1, 2]);
    assert_eq!(ran("always"), [1, 2]);
    // Then 20 s more for the third and last attempt.
    h.advance(21.secs()).await;
    assert_eq!(ran("always"), [1, 2, 3]);
    h.advance(5.mins()).await;
    assert_eq!(ran("always"), [1, 2, 3], "no fourth attempt");
    let queue = h.agents().queue().unwrap();
    let dead = queue.dead_letters(10).await.unwrap();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].job, "flaky");
    assert_eq!(dead[0].error, "attempt 3 failed");
    assert_eq!(dead[0].attempts, 3);
    let outcomes: Vec<RunOutcome> = worker_runs(&h).await.iter().map(|r| r.outcome).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == RunOutcome::Failed)
            .count(),
        4
    );
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn unknown_and_undecodable_jobs_are_dead_lettered() {
    let mut h = harness();
    h.start().await.unwrap();
    let queue = h.app_handle().service::<Queue>().unwrap();
    queue.push_raw("ghost", "{}", 0.secs()).await.unwrap();
    queue.push_raw("hello", "not json", 0.secs()).await.unwrap();
    h.advance(1.secs()).await;
    let dead = queue.dead_letters(10).await.unwrap();
    let errors: Vec<&str> = dead.iter().map(|d| d.error.as_str()).collect();
    assert_eq!(errors.len(), 2);
    assert!(errors.contains(&"unknown job `ghost`"));
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("cannot decode job `hello`"))
    );
    h.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn slow_jobs_time_out_and_shutdown_releases_the_running_job() {
    #[derive(Serialize, Deserialize)]
    struct Slow;
    impl Job for Slow {
        const NAME: &'static str = "slow";
        fn max_attempts(&self) -> u32 {
            1
        }
        async fn handle(&self, _ctx: JobCtx) -> Result<(), AgentError> {
            // Ignores cancellation on purpose.
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        }
    }
    let mut w = Watchfire::new();
    w.job::<Slow>();
    let mut h = Harness::from_watchfire(w)
        .workers(1)
        .job_timeout(5.secs())
        .shutdown_budget(3.secs());
    h.start().await.unwrap();
    let app = h.app_handle().clone();
    Slow.dispatch(&app).await.unwrap();
    h.advance(6.secs()).await;
    let queue = h.agents().queue().unwrap().clone();
    let dead = queue.dead_letters(10).await.unwrap();
    assert_eq!(dead[0].error, "timed out after 5s");

    // A job running at shutdown is given back to the queue, its attempt not counted (the
    // budget's grace, 2.4 s, ends before the job's own timeout).
    Slow.dispatch(&app).await.unwrap();
    h.advance(1.secs()).await;
    assert_eq!(queue.stats().await.unwrap().reserved, 1);
    let started = tokio::time::Instant::now();
    h.shutdown().await;
    assert!(started.elapsed() <= 3.secs());
    let stats = queue.stats().await.unwrap();
    assert_eq!((stats.pending, stats.reserved), (1, 0));
    let runs = h.runs_of("queue#0").await;
    let last = runs.iter().rev().find(|r| r.job.is_some()).unwrap();
    assert_eq!(last.outcome, RunOutcome::Killed);
    assert!(runs.iter().all(|r| r.outcome != RunOutcome::Running));
}

#[tokio::test]
async fn job_harness_runs_handle_directly() {
    let h = JobHarness::new().await;
    h.run(&Hello {
        who: "direct".into(),
    })
    .await
    .unwrap();
    assert_eq!(ran("direct"), [1]);
    assert_eq!(h.counters().get("greeted"), Some(&1));
    let err = h
        .run_attempt(
            &Flaky {
                fail_times: 2,
                tag: "jh".into(),
            },
            2,
        )
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "attempt 2 failed");
}

#[tokio::test]
async fn dispatch_without_watchfire_is_a_clear_error() {
    let app = AppBuilder::new(Settings::from_env())
        .build()
        .await
        .unwrap()
        .app;
    let err = Hello { who: "x".into() }.dispatch(&app).await.unwrap_err();
    assert!(err.to_string().contains("Watchfire is not set up"), "{err}");
}

/// An app on a temporary SQLite file (several pooled connections) with the Watchfire tables.
async fn sqlite_app(dir: &tempfile::TempDir) -> (App, Db) {
    let path = dir.path().join("watchfire.sqlite");
    let mut settings = Settings::from_env();
    settings.database_url = format!("sqlite://{}", path.display());
    settings.db_pool_max = 5;
    settings.shutdown_timeout = Duration::from_secs(5);
    let app = AppBuilder::new(settings).build().await.unwrap().app;
    let db = app.db().unwrap();
    smeltery_watchfire::migrations::up(&Schema::new(&db))
        .await
        .unwrap();
    (app, db)
}

static COUNTS: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
static COUNT_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static COUNT_OVERLAPPED: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Deserialize)]
struct Count {
    n: u32,
}

impl Job for Count {
    const NAME: &'static str = "count";

    async fn handle(&self, _ctx: JobCtx) -> Result<(), AgentError> {
        // Hold the job until a second one runs at the same time (bounded by 3 s, longer than the 1 s queue poll),
        // so the test really has two workers reserving concurrently instead of hoping the scheduler interleaves
        // them: on a slow 2-core runner one worker used to drain all 60 instant jobs alone.
        let running = COUNT_IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
        if running >= 2 {
            COUNT_OVERLAPPED.store(true, Ordering::SeqCst);
        }
        for _ in 0..300 {
            if COUNT_OVERLAPPED.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        COUNT_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        *COUNTS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .entry(self.n)
            .or_insert(0) += 1;
        tokio::task::yield_now().await;
        Ok(())
    }
}

async fn wait_until(mut done: impl FnMut() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not met in 10 s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn database_queue_with_concurrent_workers_never_double_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (app, db) = sqlite_app(&dir).await;
    let mut w = Watchfire::new();
    w.job::<Count>();
    let mut h = Harness::from_watchfire(w).app(app.clone()).workers(4);
    h.start().await.unwrap();
    assert_eq!(h.agents().queue().unwrap().driver(), "database");
    for n in 0..60 {
        Count { n }.dispatch(&app).await.unwrap();
    }
    wait_until(|| {
        COUNTS
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.len() == 60)
    })
    .await;
    // Let any double run show up.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let counts = COUNTS.lock().unwrap().clone().unwrap();
    assert!(counts.values().all(|c| *c == 1), "{counts:?}");
    let stats = h.agents().queue().unwrap().stats().await.unwrap();
    assert_eq!((stats.pending, stats.reserved), (0, 0));
    // Several workers took part.
    let mut busy = 0;
    for i in 0..4 {
        if h.runs_of(&format!("queue#{i}"))
            .await
            .iter()
            .any(|r| r.job.is_some())
        {
            busy += 1;
        }
    }
    assert!(
        COUNT_OVERLAPPED.load(Ordering::SeqCst),
        "no two jobs ever ran at once"
    );
    assert!(busy >= 2, "only {busy} worker(s) ran jobs");
    h.shutdown().await;
    db.close().await.unwrap();
}

static HOLD_RELEASED: AtomicBool = AtomicBool::new(false);

/// A job that runs (without heartbeats) until the test releases it, at most 10 s.
#[derive(Serialize, Deserialize)]
struct Hold;

impl Job for Hold {
    const NAME: &'static str = "hold";

    async fn handle(&self, _ctx: JobCtx) -> Result<(), AgentError> {
        for _ in 0..1000 {
            if HOLD_RELEASED.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }
}

async fn stored_runs(db: &Db, agent: &str) -> i64 {
    let rows = db
        .conn()
        .query_all_raw(smeltery_core::db::prelude::sea_orm::Statement::from_string(
            smeltery_core::db::prelude::sea_orm::DbBackend::Sqlite,
            format!("SELECT runs FROM watchfire_agents WHERE name = '{agent}'"),
        ))
        .await
        .unwrap();
    rows[0].try_get::<i64>("", "runs").unwrap()
}

/// The shared table, which dashboards of other processes read (and their change check hashes), counts a worker's
/// job run as soon as it starts, not at the worker's next heartbeat (D-252).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shared_agent_row_counts_a_job_run_when_it_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (app, db) = sqlite_app(&dir).await;
    let mut w = Watchfire::new();
    w.job::<Hold>();
    let mut h = Harness::from_watchfire(w).app(app.clone()).workers(1);
    h.start().await.unwrap();
    // The worker's own run is recorded first.
    tokio::time::sleep(Duration::from_millis(500)).await;
    Hold.dispatch(&app).await.unwrap();
    // While the job runs (the worker sends no heartbeat meanwhile), the row already counts it.
    let mut job_run = None;
    for _ in 0..200 {
        job_run = h
            .runs_of("queue#0")
            .await
            .iter()
            .find(|r| r.job.as_deref() == Some("hold"))
            .map(|r| r.run_id);
        if job_run.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let job_run = i64::try_from(job_run.expect("the job started")).unwrap();
    let mut stored = 0;
    for _ in 0..40 {
        stored = stored_runs(&db, "queue#0").await;
        if stored == job_run {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let still_running = !HOLD_RELEASED.load(Ordering::SeqCst);
    HOLD_RELEASED.store(true, Ordering::SeqCst);
    assert!(still_running);
    assert_eq!(stored, job_run);
    h.shutdown().await;
    db.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_reservations_are_released() {
    let dir = tempfile::tempdir().unwrap();
    let (app, db) = sqlite_app(&dir).await;
    // A job a dead worker reserved long ago.
    db.execute(
        "INSERT INTO watchfire_jobs (job, payload, attempts, available_at, reserved_at, created_at) \
         VALUES ('hello', '{\"who\":\"stale\"}', 1, 0, 1000, 0)",
    )
    .await
    .unwrap();
    let mut h = harness().app(app).job_timeout(Duration::from_secs(1));
    h.start().await.unwrap();
    wait_until(|| !ran("stale").is_empty()).await;
    assert_eq!(ran("stale"), [2], "the crashed attempt counts");
    h.shutdown().await;
    db.close().await.unwrap();
}

/// S4-07: a job whose attempts are used up (its last outcome was never stored, e.g. dead-lettering failed) goes to
/// the dead letters without running again, instead of repeating its side effects forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_past_its_attempts_is_dead_lettered_without_running() {
    let dir = tempfile::tempdir().unwrap();
    let (app, db) = sqlite_app(&dir).await;
    // `Flaky` allows 3 attempts; 3 were made (the next reservation is the 4th).
    db.execute(
        "INSERT INTO watchfire_jobs (job, payload, attempts, available_at, reserved_at, created_at)          VALUES ('flaky', '{\"fail_times\":9,\"tag\":\"exhausted\"}', 3, 0, NULL, 0)",
    )
    .await
    .unwrap();
    let mut h = harness().app(app);
    h.start().await.unwrap();
    let queue = h.agents().queue().unwrap();
    let mut dead = Vec::new();
    for _ in 0..200 {
        dead = queue.dead_letters(10).await.unwrap();
        if !dead.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].attempts, 4);
    assert!(
        dead[0].error.contains("without running again"),
        "{}",
        dead[0].error
    );
    // One line, as the dashboard shows it (the text once carried a line break and source indentation).
    assert!(
        dead[0].error.contains("(its worker stopped, or"),
        "{:?}",
        dead[0].error
    );
    assert!(ran("exhausted").is_empty(), "the job ran again");
    let stats = queue.stats().await.unwrap();
    assert_eq!((stats.pending, stats.reserved, stats.dead), (0, 0, 1));
    h.shutdown().await;
    db.close().await.unwrap();
}

/// The dead letters of `queue`, waiting until there are `n`.
async fn dead_letters_when(queue: &Queue, n: usize) -> Vec<smeltery_watchfire::DeadLetter> {
    let mut dead = Vec::new();
    for _ in 0..200 {
        dead = queue.dead_letters(10).await.unwrap();
        if dead.len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    dead
}

/// Sweep W7-03: a stored job whose payload is over `WATCHFIRE_MAX_PAYLOAD` (1 MiB by default) is dead-lettered
/// without being read or run; the dead letter keeps the stored payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_payload_is_dead_lettered_without_running() {
    let dir = tempfile::tempdir().unwrap();
    let (app, db) = sqlite_app(&dir).await;
    let payload = format!(
        "{{\"fail_times\":0,\"tag\":\"oversized{}\"}}",
        "x".repeat(1024 * 1024)
    );
    db.execute_with(
        "INSERT INTO watchfire_jobs (job, payload, attempts, available_at, reserved_at, created_at) \
         VALUES ('flaky', ?, 0, 0, NULL, 0)",
        [payload.clone().into()],
    )
    .await
    .unwrap();
    let mut h = harness().app(app);
    h.start().await.unwrap();
    let queue = h.agents().queue().unwrap();
    let dead = dead_letters_when(queue, 1).await;
    assert_eq!(dead.len(), 1);
    assert!(
        dead[0].error.contains("WATCHFIRE_MAX_PAYLOAD"),
        "{}",
        dead[0].error
    );
    assert_eq!(
        dead[0].payload, payload,
        "the dead letter keeps the payload"
    );
    assert!(
        RAN.lock()
            .unwrap()
            .iter()
            .all(|(tag, _)| !tag.starts_with("oversized")),
        "the job ran"
    );
    h.shutdown().await;
    db.close().await.unwrap();
}

/// Sweep W7-04: a payload that does not decode is dead-lettered with an error that names the problem, never a
/// value of the payload (the error also goes to alerts).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decode_errors_never_quote_the_payload() {
    let dir = tempfile::tempdir().unwrap();
    let (app, db) = sqlite_app(&dir).await;
    db.execute(
        "INSERT INTO watchfire_jobs (job, payload, attempts, available_at, reserved_at, created_at) \
         VALUES ('flaky', '{\"fail_times\":\"secret-4711\",\"tag\":\"t\"}', 0, 0, NULL, 0)",
    )
    .await
    .unwrap();
    let mut h = harness().app(app);
    h.start().await.unwrap();
    let queue = h.agents().queue().unwrap();
    let dead = dead_letters_when(queue, 1).await;
    assert_eq!(dead.len(), 1);
    assert!(
        dead[0].error.contains("cannot decode job `flaky`"),
        "{}",
        dead[0].error
    );
    assert!(!dead[0].error.contains("secret-4711"), "{}", dead[0].error);
    h.shutdown().await;
    db.close().await.unwrap();
}

/// Long errors (an upstream body, say) are stored cut to 8 KiB.
#[tokio::test(start_paused = true)]
async fn long_job_errors_are_stored_truncated() {
    #[derive(Serialize, Deserialize)]
    struct Loud;
    impl Job for Loud {
        const NAME: &'static str = "loud";
        fn max_attempts(&self) -> u32 {
            1
        }
        async fn handle(&self, _ctx: JobCtx) -> Result<(), AgentError> {
            Err(AgentError::msg("e".repeat(70 * 1024)))
        }
    }
    let mut w = Watchfire::new();
    w.job::<Loud>();
    let mut h = Harness::from_watchfire(w).workers(1);
    h.start().await.unwrap();
    Loud.dispatch(h.app_handle()).await.unwrap();
    h.advance(2.secs()).await;
    let dead = h.agents().queue().unwrap().dead_letters(1).await.unwrap();
    assert!(
        dead[0].error.len() <= 8 * 1024 + 64,
        "{}",
        dead[0].error.len()
    );
    assert!(dead[0].error.contains("truncated"));
    let runs = h.runs_of("queue#0").await;
    let run = runs
        .iter()
        .find(|r| r.job.as_deref() == Some("loud"))
        .unwrap();
    assert!(run.error.as_ref().unwrap().len() <= 8 * 1024 + 64);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_runs_are_marked_and_checkpoints_persist_in_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let (app, db) = sqlite_app(&dir).await;
    // The previous process died during run 5 of `crawler`.
    db.execute(
        "INSERT INTO watchfire_runs (agent, run_id, started_at, outcome) VALUES ('crawler', 5, 1000, 'running')",
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO watchfire_checkpoints (agent, data, updated_at) VALUES ('crawler', '41', 1000)",
    )
    .await
    .unwrap();
    let seen = Arc::new(AtomicU32::new(0));
    let sink = Arc::clone(&seen);
    let mut w = Watchfire::new();
    w.run("crawler", move |ctx| {
        let sink = Arc::clone(&sink);
        async move {
            let page: u32 = ctx.checkpoint_get().await?.unwrap_or(0);
            sink.store(page, Ordering::SeqCst);
            ctx.checkpoint(&(page + 1)).await?;
            if page < 43 {
                return Err(AgentError::msg("restart me"));
            }
            ctx.cancelled().await;
            Ok(())
        }
    })
    .backoff(Duration::from_millis(1)..=Duration::from_millis(5));
    let mut h = Harness::from_watchfire(w).app(app);
    h.start().await.unwrap();
    wait_until(|| seen.load(Ordering::SeqCst) == 43).await;
    let runs = h.runs().await;
    assert_eq!(runs[0].run_id, 5);
    assert_eq!(runs[0].outcome, RunOutcome::Interrupted);
    assert_eq!(
        runs[1].run_id, 6,
        "run ids continue after the interrupted run"
    );
    assert_eq!(runs[1].outcome, RunOutcome::Failed);
    h.shutdown().await;
    let runs = h.runs().await;
    assert_eq!(runs.last().unwrap().outcome, RunOutcome::Stopped);
    // The registry row and checkpoint are in the database.
    let rows = db
        .conn()
        .query_all_raw(smeltery_core::db::prelude::sea_orm::Statement::from_string(
            smeltery_core::db::prelude::sea_orm::DbBackend::Sqlite,
            "SELECT state, runs FROM watchfire_agents WHERE name = 'crawler'",
        ))
        .await
        .unwrap();
    let state: String = rows[0].try_get("", "state").unwrap();
    assert_eq!(state, "stopped");
    let rows = db
        .conn()
        .query_all_raw(smeltery_core::db::prelude::sea_orm::Statement::from_string(
            smeltery_core::db::prelude::sea_orm::DbBackend::Sqlite,
            "SELECT data FROM watchfire_checkpoints WHERE agent = 'crawler'",
        ))
        .await
        .unwrap();
    let data: String = rows[0].try_get("", "data").unwrap();
    assert_eq!(data, "44");
    db.close().await.unwrap();
}

/// A service the `UsesService` job reads from the app.
struct Greeting(&'static str);

#[derive(Serialize, Deserialize)]
struct UsesService;

impl Job for UsesService {
    const NAME: &'static str = "uses_service";

    async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
        let greeting = ctx
            .service::<Greeting>()
            .ok_or_else(|| AgentError::msg("no greeting service"))?;
        ctx.counter(greeting.0).inc();
        ctx.dispatch(Hello { who: "next".into() }).await?;
        Ok(())
    }
}

#[tokio::test]
async fn job_harness_for_app_runs_inside_the_given_app() {
    let mut settings = Settings::from_env();
    settings.env = "testing".to_owned();
    settings.database_url = String::new();
    let app = AppBuilder::new(settings).build().await.unwrap().app;
    app.insert_service(Greeting("hello"));
    let h = JobHarness::for_app(app.clone()).await;
    h.run(&UsesService).await.unwrap();
    assert_eq!(h.counters().get("hello"), Some(&1));
    assert_eq!(
        h.queue_stats().await.pending,
        1,
        "the dispatched job waits in the queue"
    );
    assert!(
        app.service::<Queue>().is_some(),
        "a memory queue was installed"
    );
    // Without the service the same job fails: the minimal app of `JobHarness::new` has none.
    assert!(JobHarness::new().await.run(&UsesService).await.is_err());
}
