//! What each run receives: [`AgentCtx`].

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use smeltery_core::App;
use smeltery_core::db::Db;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, watch};
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, Event};
use crate::error::{AgentError, Error};
use crate::http::Http;
use crate::queue::{Job, JobId};
use crate::runtime::Shared;
use crate::status::{AgentStatus, LogLine};

/// The last heartbeat: a monotonic instant for stall checks, wall-clock ms for display.
pub(crate) type Beat = Option<(Instant, i64)>;

/// Lines kept per agent.
pub(crate) const LOG_LINES: usize = 200;

/// An agent's ring buffer of recent log lines.
#[derive(Debug, Default)]
pub(crate) struct LogRing {
    lines: Mutex<VecDeque<LogLine>>,
}

impl LogRing {
    pub(crate) fn push(&self, line: LogLine) {
        let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() >= LOG_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub(crate) fn lines(&self) -> Vec<LogLine> {
        self.lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}

/// What a run gets from the runtime: identity, cancellation, heartbeat, the app, HTTP,
/// checkpoints, events, counters and logs. Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct AgentCtx {
    inner: Arc<CtxInner>,
}

struct CtxInner {
    app: App,
    name: Arc<str>,
    run_id: u64,
    token: CancellationToken,
    beat: Arc<watch::Sender<Beat>>,
    limiter: Arc<Semaphore>,
    shared: Arc<Shared>,
    counters: Arc<Mutex<BTreeMap<String, i64>>>,
    logs: Arc<LogRing>,
    http: Http,
    span: tracing::Span,
    runs: Arc<AtomicU64>,
    /// The agent's status (supervised runs), so runs recorded inside this run count in `AgentStatus::runs`.
    status: Option<Arc<watch::Sender<AgentStatus>>>,
}

impl std::fmt::Debug for AgentCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCtx")
            .field("name", &self.inner.name)
            .field("run_id", &self.inner.run_id)
            .finish_non_exhaustive()
    }
}

/// The parts the runner hands to a new context.
pub(crate) struct CtxParts {
    pub(crate) app: App,
    pub(crate) name: Arc<str>,
    pub(crate) run_id: u64,
    pub(crate) token: CancellationToken,
    pub(crate) beat: Arc<watch::Sender<Beat>>,
    pub(crate) limiter: Arc<Semaphore>,
    pub(crate) shared: Arc<Shared>,
    pub(crate) counters: Arc<Mutex<BTreeMap<String, i64>>>,
    pub(crate) logs: Arc<LogRing>,
    pub(crate) runs: Arc<AtomicU64>,
    pub(crate) status: Option<Arc<watch::Sender<AgentStatus>>>,
}

impl AgentCtx {
    pub(crate) fn new(parts: CtxParts) -> Self {
        let span = tracing::info_span!("agent", agent = %parts.name, run = parts.run_id);
        let http = parts.shared.http.with_token(parts.token.clone());
        Self {
            inner: Arc::new(CtxInner {
                app: parts.app,
                name: parts.name,
                run_id: parts.run_id,
                token: parts.token,
                beat: parts.beat,
                limiter: parts.limiter,
                shared: parts.shared,
                counters: parts.counters,
                logs: parts.logs,
                http,
                span,
                runs: parts.runs,
                status: parts.status,
            }),
        }
    }

    /// A context outside the supervisor (one-off scheduled calls, job tests).
    pub(crate) fn detached(
        app: App,
        shared: Arc<Shared>,
        name: &str,
        token: CancellationToken,
    ) -> Self {
        Self::new(CtxParts {
            app,
            name: Arc::from(name),
            run_id: 1,
            token,
            beat: Arc::new(watch::channel(None).0),
            limiter: Arc::new(Semaphore::new(1)),
            shared,
            counters: Arc::default(),
            logs: Arc::default(),
            runs: Arc::new(AtomicU64::new(1)),
            status: None,
        })
    }

    /// The same context with another cancellation token (a scheduled call guarded by its run lease).
    pub(crate) fn with_token(&self, token: CancellationToken) -> Self {
        let inner = &self.inner;
        Self::new(CtxParts {
            app: inner.app.clone(),
            name: Arc::clone(&inner.name),
            run_id: inner.run_id,
            token,
            beat: Arc::clone(&inner.beat),
            limiter: Arc::clone(&inner.limiter),
            shared: Arc::clone(&inner.shared),
            counters: Arc::clone(&inner.counters),
            logs: Arc::clone(&inner.logs),
            runs: Arc::clone(&inner.runs),
            status: inner.status.clone(),
        })
    }

    /// The agent name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// This run's id, counting from 1 per agent.
    pub fn run_id(&self) -> u64 {
        self.inner.run_id
    }

    /// The run's cancellation token, e.g. for `select!` or a spawned task.
    pub fn token(&self) -> &CancellationToken {
        &self.inner.token
    }

    /// Completes when the run is asked to stop.
    pub async fn cancelled(&self) {
        self.inner.token.cancelled().await;
    }

    /// Whether the run was asked to stop.
    pub fn is_cancelled(&self) -> bool {
        self.inner.token.is_cancelled()
    }

    /// Record that the agent is alive. Cheap; call it as often as you like. Needed when the
    /// agent has a heartbeat timeout and does not use [`AgentCtx::interval`].
    pub fn heartbeat(&self) {
        let now = self.inner.shared.clock.now_ms();
        self.inner.beat.send_replace(Some((Instant::now(), now)));
    }

    /// Sleep for `duration`. Returns `false` when the run was cancelled first.
    pub async fn sleep(&self, duration: Duration) -> bool {
        tokio::select! {
            biased;
            () = self.inner.token.cancelled() => false,
            () = tokio::time::sleep(duration) => true,
        }
    }

    /// A [`Ticker`] firing every `period` (the first tick at once). A zero period is 1 ms.
    pub fn interval(&self, period: Duration) -> Ticker {
        let mut interval = tokio::time::interval(period.max(Duration::from_millis(1)));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Ticker {
            interval,
            ctx: self.clone(),
        }
    }

    /// Wait for one of the run's concurrency permits (`AgentConfig::concurrency`, default 1),
    /// shared by every clone of this context. `None` when the run is cancelled first.
    pub async fn acquire(&self) -> Option<Permit> {
        tokio::select! {
            biased;
            () = self.inner.token.cancelled() => None,
            permit = Arc::clone(&self.inner.limiter).acquire_owned() => {
                permit.ok().map(|p| Permit { _permit: p })
            }
        }
    }

    /// The polite HTTP client: timeouts, retries, per-host rate limits, cancelled with the run.
    pub fn http(&self) -> &Http {
        &self.inner.http
    }

    /// Wait for a token of `host`'s rate limit (see `Watchfire::rate_limit`), for requests made
    /// without [`AgentCtx::http`]. Returns `false` when the run was cancelled first.
    pub async fn rate_limited(&self, host: &str) -> bool {
        tokio::select! {
            biased;
            () = self.inner.token.cancelled() => false,
            () = self.inner.shared.http.limits().wait(host) => true,
        }
    }

    /// The app.
    pub fn app(&self) -> &App {
        &self.inner.app
    }

    /// The app's database.
    ///
    /// # Errors
    /// No database is configured.
    pub fn db(&self) -> smeltery_core::Result<Db> {
        self.inner.app.db()
    }

    /// The app's cache on its default store (`CACHE_STORE`), e.g. for a lock that keeps two
    /// processes from running the same work: `ctx.cache().lock("import", ttl)`.
    pub fn cache(&self) -> smeltery_core::cache::Cache {
        self.inner.app.cache()
    }

    /// A service registered on the app.
    pub fn service<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.inner.app.service::<T>()
    }

    /// Save `state` as this agent's checkpoint (JSON in `watchfire_checkpoints`, or in memory
    /// without a database). It survives restarts; read it back with
    /// [`AgentCtx::checkpoint_get`].
    ///
    /// # Errors
    /// `state` does not serialize, or the store fails (or times out).
    pub async fn checkpoint<T: Serialize + ?Sized>(&self, state: &T) -> Result<(), AgentError> {
        let data = serde_json::to_string(state)?;
        let shared = &self.inner.shared;
        let now = shared.clock.now_ms();
        shared
            .timed(
                "save_checkpoint",
                shared.store.save_checkpoint(&self.inner.name, &data, now),
            )
            .await?;
        Ok(())
    }

    /// The last checkpoint saved by this agent, if any.
    ///
    /// # Errors
    /// The store fails (or times out), or the saved JSON does not deserialize into `T`.
    pub async fn checkpoint_get<T: DeserializeOwned>(&self) -> Result<Option<T>, AgentError> {
        let shared = &self.inner.shared;
        let data = shared
            .timed(
                "load_checkpoint",
                shared.store.load_checkpoint(&self.inner.name),
            )
            .await?;
        match data {
            Some(data) => Ok(Some(serde_json::from_str(&data)?)),
            None => Ok(None),
        }
    }

    /// Send an event to every running `Watchfire::on_event` agent listening for `event`.
    /// Never blocks: a listener that falls more than 1024 events behind skips the oldest.
    ///
    /// # Errors
    /// `payload` does not serialize.
    pub fn emit(&self, event: &str, payload: impl Serialize) -> Result<(), AgentError> {
        let payload = serde_json::to_value(payload)?;
        let _ = self.inner.shared.bus.send(Event {
            name: event.to_owned(),
            source: self.inner.name.to_string(),
            payload,
        });
        Ok(())
    }

    pub(crate) fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.inner.shared.bus.subscribe()
    }

    /// A counter saved into this run's record (`watchfire_runs.counters`).
    pub fn counter(&self, name: &str) -> Counter {
        Counter {
            name: name.to_owned(),
            counters: Arc::clone(&self.inner.counters),
        }
    }

    /// The counters so far.
    pub fn counters(&self) -> BTreeMap<String, i64> {
        self.inner
            .counters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Log through `tracing` (with `agent` and `run` fields) and into the agent's ring buffer
    /// of the last 200 lines.
    pub fn log(&self) -> AgentLog<'_> {
        AgentLog { ctx: self }
    }

    /// The run's `tracing` span (fields `agent` and `run`). The run future is already
    /// instrumented with it; use it for tasks you spawn.
    pub fn span(&self) -> &tracing::Span {
        &self.inner.span
    }

    /// Start `agent` as a child of this run, named `<this agent>.<name>`. It is supervised like
    /// any agent (its own restart policy and backoff from `agent.config()`), shows in the agent
    /// list, and is stopped and removed when this run ends.
    ///
    /// # Errors
    /// An invalid or duplicate name, or Watchfire is shutting down.
    pub async fn spawn_child(&self, name: &str, agent: impl Agent) -> Result<AgentStatus, Error> {
        crate::config::validate_name(name)?;
        let full = format!("{}.{name}", self.inner.name);
        let config = agent.config();
        self.inner
            .shared
            .spawn_agent(
                full.clone(),
                config,
                Box::new(agent),
                Some(self.inner.token.clone()),
                false,
            )
            .await?;
        self.inner.shared.status(&full)
    }

    /// Queue `job` for the workers.
    ///
    /// # Errors
    /// The job does not serialize, there is no queue, or the queue fails.
    pub async fn dispatch<J: Job>(&self, job: J) -> Result<JobId, AgentError> {
        Ok(crate::queue::push(&self.inner.app, &job, Duration::ZERO).await?)
    }

    /// Queue `job` to run after `delay`.
    ///
    /// # Errors
    /// See [`AgentCtx::dispatch`].
    pub async fn dispatch_later<J: Job>(
        &self,
        job: J,
        delay: Duration,
    ) -> Result<JobId, AgentError> {
        Ok(crate::queue::push(&self.inner.app, &job, delay).await?)
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.inner.shared
    }

    /// A new run id of this agent, for runs recorded inside this run (jobs, scheduled calls).
    pub(crate) fn next_run_id(&self) -> u64 {
        let id = self
            .inner
            .runs
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        // The run count covers these runs too (a queue worker's jobs), as the run ids do.
        if let Some(status) = &self.inner.status {
            let now = self.inner.shared.clock.now_ms();
            status.send_modify(|s| {
                s.runs = s.runs.max(id);
                s.updated_at_ms = now;
            });
            // No subscribers is fine.
            let _ = self.inner.shared.events.send(status.borrow().clone());
        }
        id
    }

    /// Write the agent's status to the store (the shared `watchfire_agents` row other processes' dashboards read),
    /// after [`AgentCtx::next_run_id`] raised its run count. Only while this run lasts: a cancelled run (stopped,
    /// or a singleton whose lease was lost) writes nothing, like the runner's own writes. Store failures are logged.
    pub(crate) async fn persist_status(&self) {
        let Some(status) = &self.inner.status else {
            return;
        };
        if self.inner.token.is_cancelled() {
            return;
        }
        let snapshot = status.borrow().clone();
        self.inner.shared.persist_agent(&snapshot).await;
    }

    pub(crate) fn reset_counters(&self) -> BTreeMap<String, i64> {
        std::mem::take(
            &mut *self
                .inner
                .counters
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }

    fn write_log(&self, level: &'static str, message: &str) {
        let _entered = self.inner.span.enter();
        match level {
            "debug" => tracing::debug!("{message}"),
            "warn" => tracing::warn!("{message}"),
            "error" => tracing::error!("{message}"),
            _ => tracing::info!("{message}"),
        }
        self.inner.logs.push(LogLine {
            at_ms: self.inner.shared.clock.now_ms(),
            level,
            run_id: self.inner.run_id,
            message: message.to_owned(),
        });
    }
}

/// [`AgentCtx::log`]: one method per level.
#[derive(Debug)]
pub struct AgentLog<'a> {
    ctx: &'a AgentCtx,
}

impl AgentLog<'_> {
    /// Debug level.
    pub fn debug(&self, message: impl AsRef<str>) {
        self.ctx.write_log("debug", message.as_ref());
    }
    /// Info level.
    pub fn info(&self, message: impl AsRef<str>) {
        self.ctx.write_log("info", message.as_ref());
    }
    /// Warn level.
    pub fn warn(&self, message: impl AsRef<str>) {
        self.ctx.write_log("warn", message.as_ref());
    }
    /// Error level.
    pub fn error(&self, message: impl AsRef<str>) {
        self.ctx.write_log("error", message.as_ref());
    }
}

/// A named counter of the current run, from [`AgentCtx::counter`].
#[derive(Clone, Debug)]
pub struct Counter {
    name: String,
    counters: Arc<Mutex<BTreeMap<String, i64>>>,
}

impl Counter {
    /// Add one.
    pub fn inc(&self) {
        self.add(1);
    }

    /// Add `n` (may be negative).
    pub fn add(&self, n: i64) {
        let mut counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let value = counters.entry(self.name.clone()).or_insert(0);
        *value = value.saturating_add(n);
    }

    /// The current value.
    pub fn get(&self) -> i64 {
        self.counters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.name)
            .copied()
            .unwrap_or(0)
    }
}

/// A concurrency permit from [`AgentCtx::acquire`]; released on drop.
#[derive(Debug)]
pub struct Permit {
    _permit: OwnedSemaphorePermit,
}

/// A cancellation-aware interval that heartbeats on every tick.
///
/// ```
/// # use smeltery_watchfire::prelude::*;
/// # async fn demo(ctx: AgentCtx) {
/// let mut ticker = ctx.interval(30.secs());
/// while ticker.tick().await {
///     // one unit of work
/// }
/// // cancelled: clean up and return
/// # }
/// ```
#[derive(Debug)]
pub struct Ticker {
    interval: Interval,
    ctx: AgentCtx,
}

impl Ticker {
    /// Wait for the next tick and heartbeat. `false` once the run is cancelled. Missed ticks
    /// are delayed, not bursted.
    pub async fn tick(&mut self) -> bool {
        tokio::select! {
            biased;
            () = self.ctx.inner.token.cancelled() => false,
            _ = self.interval.tick() => {
                self.ctx.heartbeat();
                true
            }
        }
    }
}
