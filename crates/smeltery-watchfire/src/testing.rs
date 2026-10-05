//! Test helpers: [`Harness`] runs agents under the real supervisor on paused Tokio time with
//! an in-memory store and a fake HTTP transport; [`JobHarness`] runs a job's `handle`.
//!
//! ```
//! use smeltery_watchfire::prelude::*;
//! use smeltery_watchfire::testing::Harness;
//!
//! # #[tokio::main(flavor = "current_thread", start_paused = true)]
//! # async fn main() {
//! // In a #[tokio::test(start_paused = true)] function:
//! let mut h = Harness::new(agent_fn("flaky", |_ctx| async { Err(AgentError::msg("boom")) }))
//!     .config(AgentConfig::default().backoff(1.secs()..=8.secs()));
//! h.start().await.unwrap();
//! h.advance(30.secs()).await;
//! assert!(h.restarts() >= 3);
//! assert_eq!(h.runs().await[0].outcome, RunOutcome::Failed);
//! h.shutdown().await;
//! # }
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use smeltery_core::config::Settings;
use smeltery_core::{App, AppBuilder};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::app::{LaunchParts, WatchfireSettings, build_shared, launch_with};
use crate::config::AgentConfig;
use crate::ctx::AgentCtx;
use crate::error::{AgentError, Error};
use crate::http::FakeTransport;
use crate::policy::Jitter;
use crate::queue::{Job, JobCtx, Queue, QueueStats};
use crate::registry::Watchfire;
use crate::runtime::Agents;
use crate::status::{AgentState, AgentStatus, LogLine, RunRecord};
use crate::store::{MemoryStore, Store};
use crate::time::Clock;

/// A minimal app for tests: no routes, no database, `APP_ENV=testing`, a memory queue.
async fn test_app(shutdown_timeout: Duration) -> Result<App, Error> {
    let mut settings = Settings::from_env();
    settings.env = "testing".to_owned();
    settings.database_url = String::new();
    settings.shutdown_timeout = shutdown_timeout;
    let app = AppBuilder::new(settings)
        .build()
        .await
        .map_err(|e| Error::Config(e.to_string()))?
        .app;
    Ok(app)
}

/// Agents under test. Use it in `#[tokio::test(start_paused = true)]`: [`Harness::advance`]
/// moves the paused clock, so backoffs, heartbeats and schedules take no real time.
pub struct Harness {
    watchfire: Option<Watchfire>,
    main: String,
    config: Option<AgentConfig>,
    transport: FakeTransport,
    app: Option<App>,
    settings: WatchfireSettings,
    seed: Option<u64>,
    shutdown_timeout: Duration,
    store: Option<Arc<dyn Store>>,
    agents: Option<Agents>,
    events: Mutex<Option<broadcast::Receiver<AgentStatus>>>,
    transitions: Mutex<Vec<(String, AgentState)>>,
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Harness")
            .field("main", &self.main)
            .finish_non_exhaustive()
    }
}

impl Harness {
    /// A harness for one agent (use [`agent_fn`](crate::agent_fn) for a closure).
    pub fn new(agent: impl Agent) -> Self {
        let mut w = Watchfire::new();
        let name = agent.name();
        w.agent(agent);
        let mut h = Self::from_watchfire(w);
        h.main = name;
        h
    }

    /// A harness for a whole registration (pools, groups, jobs, the schedule). The first
    /// registered agent is the one [`Harness::state`] and friends look at.
    pub fn from_watchfire(watchfire: Watchfire) -> Self {
        let main = watchfire
            .agent_names()
            .first()
            .map(|s| (*s).to_owned())
            .unwrap_or_default();
        let mut settings = WatchfireSettings::from_env();
        settings.max_concurrent = 0;
        settings.queue_driver = String::new();
        settings.alert_webhook = None;
        settings.api_addr = None;
        Self {
            watchfire: Some(watchfire),
            main,
            config: None,
            transport: FakeTransport::new(),
            app: None,
            settings,
            seed: None,
            shutdown_timeout: Duration::from_secs(30),
            store: None,
            agents: None,
            events: Mutex::new(None),
            transitions: Mutex::new(Vec::new()),
        }
    }

    /// Replace the main agent's config.
    pub fn config(mut self, config: AgentConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Answer `ctx.http()` requests from this fake (default: an empty one, every request
    /// fails).
    pub fn http(mut self, transport: FakeTransport) -> Self {
        self.transport = transport;
        self
    }

    /// Run inside this app (e.g. one with a database). Without it a minimal app is built.
    /// A database app gets the database store and queue when the Watchfire tables exist.
    pub fn app(mut self, app: App) -> Self {
        self.app = Some(app);
        self
    }

    /// Seed the backoff jitter for repeatable delays.
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// The app's shutdown budget (default 30 s).
    pub fn shutdown_budget(mut self, budget: Duration) -> Self {
        self.shutdown_timeout = budget;
        self
    }

    /// The global concurrency limit (`WATCHFIRE_MAX_CONCURRENT`; 0 = none).
    pub fn max_concurrent(mut self, n: usize) -> Self {
        self.settings.max_concurrent = n;
        self
    }

    /// The number of queue workers.
    pub fn workers(mut self, n: usize) -> Self {
        self.settings.workers = n;
        self
    }

    /// POST alerts as JSON to `url` (through the fake transport).
    pub fn alert_webhook(mut self, url: &str) -> Self {
        self.settings.alert_webhook = Some(url.to_owned());
        self
    }

    /// The job timeout.
    pub fn job_timeout(mut self, timeout: Duration) -> Self {
        self.settings.job_timeout = timeout;
        self
    }

    /// Launch everything.
    ///
    /// # Errors
    /// The registration is invalid or a store fails.
    pub async fn start(&mut self) -> Result<(), Error> {
        let mut watchfire = self
            .watchfire
            .take()
            .ok_or(Error::Config("the harness was already started".to_owned()))?;
        if let Some(config) = self.config.take()
            && let Some(reg) = watchfire.agents.first_mut()
        {
            reg.config = config;
        }
        let app = match self.app.clone() {
            Some(app) => app,
            None => test_app(self.shutdown_timeout).await?,
        };
        let has_db_tables = match app.db() {
            Ok(db) => crate::store::DbStore::ready(&db).await,
            Err(_) => false,
        };
        let store: Arc<dyn Store> = match (has_db_tables, app.db()) {
            (true, Ok(db)) => Arc::new(crate::store::DbStore::open(db).await),
            _ => Arc::new(MemoryStore::default()),
        };
        if app.service::<Queue>().is_none() {
            let queue = match (has_db_tables, app.db()) {
                (true, Ok(db)) => Queue::database(db, Clock::new(), self.settings.store_timeout),
                _ => Queue::memory(Clock::new(), self.settings.store_timeout),
            }
            .with_max_payload(self.settings.max_payload);
            app.insert_service(queue);
        }
        self.store = Some(Arc::clone(&store));
        let parts = LaunchParts {
            transport: Arc::new(self.transport.clone()),
            store,
            jitter: self.seed.map_or_else(Jitter::from_os, Jitter::seeded),
            health_interval: Duration::from_secs(1),
            coord: None,
        };
        // Subscribe before anything runs, so no transition is missed.
        let mut events = None;
        let agents = launch_with(&app, watchfire, &self.settings, parts, |a| {
            events = Some(a.subscribe());
        })
        .await?;
        *self.events.lock().unwrap_or_else(|e| e.into_inner()) = events;
        app.insert_service(agents.clone());
        self.app = Some(app);
        self.agents = Some(agents);
        tokio::task::yield_now().await;
        Ok(())
    }

    /// Let `duration` of (paused) time pass while the agents run.
    pub async fn advance(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    /// Let tasks run without moving the clock.
    pub async fn settle(&self) {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    /// The handle (after [`Harness::start`]).
    ///
    /// # Panics
    /// Before `start`.
    #[allow(clippy::expect_used)]
    pub fn agents(&self) -> &Agents {
        self.agents.as_ref().expect("call Harness::start first")
    }

    /// The app.
    ///
    /// # Panics
    /// Before `start`.
    #[allow(clippy::expect_used)]
    pub fn app_handle(&self) -> &App {
        self.app.as_ref().expect("call Harness::start first")
    }

    /// The main agent's status.
    ///
    /// # Panics
    /// Before `start`, or when the agent was removed.
    #[allow(clippy::expect_used)]
    pub fn status(&self) -> AgentStatus {
        self.agents().status(&self.main).expect("the agent exists")
    }

    /// The main agent's state.
    pub fn state(&self) -> AgentState {
        self.status().state
    }

    /// Another agent's state.
    ///
    /// # Panics
    /// Unknown agent.
    #[allow(clippy::expect_used)]
    pub fn state_of(&self, name: &str) -> AgentState {
        self.agents().status(name).expect("the agent exists").state
    }

    /// Every state the main agent went through, in order (consecutive repeats collapsed).
    pub fn transitions(&self) -> Vec<AgentState> {
        self.transitions_of(&self.main.clone())
    }

    /// Every state `name` went through.
    pub fn transitions_of(&self, name: &str) -> Vec<AgentState> {
        let mut all = self.transitions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rx) = self
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            loop {
                match rx.try_recv() {
                    Ok(status) => all.push((status.name.clone(), status.state)),
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        }
        let mut states: Vec<AgentState> = Vec::new();
        for (agent, state) in all.iter() {
            if agent == name && states.last() != Some(state) {
                states.push(*state);
            }
        }
        states
    }

    /// The main agent's restart count.
    pub fn restarts(&self) -> u64 {
        self.status().restarts
    }

    /// The delay of the pending automatic restart, if one is pending.
    pub fn next_backoff(&self) -> Option<Duration> {
        self.status()
            .backoff_ms
            .map(|ms| Duration::from_millis(u64::try_from(ms).unwrap_or(0)))
    }

    /// The main agent's runs, oldest first.
    pub async fn runs(&self) -> Vec<RunRecord> {
        self.runs_of(&self.main.clone()).await
    }

    /// `name`'s runs, oldest first.
    pub async fn runs_of(&self, name: &str) -> Vec<RunRecord> {
        let Some(store) = &self.store else {
            return Vec::new();
        };
        let mut runs = store
            .recent_runs(Some(name), 10_000)
            .await
            .unwrap_or_default();
        runs.reverse();
        runs
    }

    /// The main agent's log lines.
    pub fn logs(&self) -> Vec<LogLine> {
        self.agents().logs(&self.main).unwrap_or_default()
    }

    /// Stop the main agent.
    ///
    /// # Errors
    /// See [`Agents::stop`].
    pub async fn stop(&self) -> Result<AgentStatus, Error> {
        self.agents().stop(&self.main).await
    }

    /// Shut everything down (as the app does on Ctrl-C).
    pub async fn shutdown(&self) {
        if let Some(agents) = &self.agents {
            agents.shutdown().await;
        }
    }
}

/// Run a job's `handle` directly, with a real [`JobCtx`] over a minimal app (memory queue,
/// fake HTTP).
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use smeltery_watchfire::prelude::*;
/// use smeltery_watchfire::testing::JobHarness;
///
/// #[derive(Serialize, Deserialize)]
/// struct Count { n: i64 }
///
/// impl Job for Count {
///     const NAME: &'static str = "count";
///     async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
///         ctx.counter("seen").add(self.n);
///         ctx.dispatch(Count { n: self.n - 1 }).await?;
///         Ok(())
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let h = JobHarness::new().await;
/// h.run(&Count { n: 3 }).await.unwrap();
/// assert_eq!(h.counters().get("seen"), Some(&3));
/// assert_eq!(h.queue_stats().await.pending, 1);
/// # }
/// ```
pub struct JobHarness {
    ctx: AgentCtx,
    queue: Queue,
}

impl std::fmt::Debug for JobHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHarness").finish_non_exhaustive()
    }
}

impl JobHarness {
    /// A harness with an empty fake HTTP transport.
    ///
    /// # Panics
    /// When the minimal app cannot be built.
    pub async fn new() -> Self {
        Self::with_http(FakeTransport::new()).await
    }

    /// A harness answering `ctx.http()` from `transport`.
    ///
    /// # Panics
    /// When the minimal app cannot be built.
    #[allow(clippy::panic)]
    pub async fn with_http(transport: FakeTransport) -> Self {
        let app = match test_app(Duration::from_secs(5)).await {
            Ok(app) => app,
            Err(e) => panic!("cannot build the test app: {e}"),
        };
        Self::build(app, transport)
    }

    /// A harness running jobs inside `app` (e.g. a `TestApp`'s app, with its database and services), with an
    /// empty fake HTTP transport. Jobs the job dispatches go to the app's queue, or to a memory queue when the
    /// app has none.
    ///
    /// ```no_run
    /// # use smeltery_watchfire::testing::JobHarness;
    /// # async fn demo(app: smeltery_core::App) {
    /// let h = JobHarness::for_app(app).await;
    /// # }
    /// ```
    pub async fn for_app(app: App) -> Self {
        Self::build(app, FakeTransport::new())
    }

    fn build(app: App, transport: FakeTransport) -> Self {
        let settings = WatchfireSettings::from_env();
        let queue = match app.service::<Queue>() {
            Some(queue) => (*queue).clone(),
            None => {
                let queue = Queue::memory(Clock::new(), settings.store_timeout)
                    .with_max_payload(settings.max_payload);
                app.insert_service(queue.clone());
                queue
            }
        };
        let parts = LaunchParts {
            transport: Arc::new(transport),
            store: Arc::new(MemoryStore::default()),
            jitter: Jitter::seeded(1),
            health_interval: Duration::from_secs(1),
            coord: None,
        };
        let shared = build_shared(
            &app,
            &mut Watchfire::new(),
            &settings,
            parts,
            CancellationToken::new(),
        );
        let ctx = AgentCtx::detached(app.clone(), shared, "queue#0", CancellationToken::new());
        Self { ctx, queue }
    }

    /// Run `job.handle` once (attempt 1).
    ///
    /// # Errors
    /// What `handle` returns.
    pub async fn run<J: Job>(&self, job: &J) -> Result<(), AgentError> {
        self.run_attempt(job, 1).await
    }

    /// Run `job.handle` as attempt number `attempt`.
    ///
    /// # Errors
    /// What `handle` returns.
    pub async fn run_attempt<J: Job>(&self, job: &J, attempt: u32) -> Result<(), AgentError> {
        job.handle(JobCtx::new(self.ctx.clone(), J::NAME, 1, attempt))
            .await
    }

    /// The counters the job set.
    pub fn counters(&self) -> std::collections::BTreeMap<String, i64> {
        self.ctx.counters()
    }

    /// The app.
    pub fn app(&self) -> &App {
        self.ctx.app()
    }

    /// The queue's counts (jobs the job dispatched are pending).
    pub async fn queue_stats(&self) -> QueueStats {
        self.queue.stats().await.unwrap_or_default()
    }
}
