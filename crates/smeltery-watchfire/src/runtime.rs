//! The supervisor: owns agents, runs them, restarts them, reports their status.

use std::any::Any;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use futures_util::FutureExt as _;
use smeltery_core::{App, WeakApp};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, info, warn};

use crate::agent::{Agent, DynAgent, Event};
use crate::config::AgentConfig;
use crate::coord::{Coordinator, Lease};
use crate::ctx::{AgentCtx, Beat, CtxParts, LogRing};
use crate::error::{Error, StoreError};
use crate::http::Http;
use crate::policy::{Jitter, Restart};
use crate::queue::{JobRegistry, Queue};
use crate::status::{AgentState, AgentStatus, Health, LogLine, RunOutcome, RunRecord};
use crate::store::Store;
use crate::time::{Clock, duration_ms};

/// Status updates buffered per subscriber; a slower one skips ahead.
const STATUS_CAPACITY: usize = 256;
/// Events buffered per `on_event` listener.
const EVENT_CAPACITY: usize = 1024;
/// Commands queued per agent before callers wait.
const COMMAND_CAPACITY: usize = 16;

/// Everything the runners, contexts and the [`Agents`] handle share.
pub(crate) struct Shared {
    /// Weak: `Shared` lives inside the app (the `Agents` service), so a strong `App` here would keep
    /// the app alive forever. Runners hold a strong `App` while they run.
    pub(crate) app: WeakApp,
    pub(crate) app_name: String,
    pub(crate) store: Arc<dyn Store>,
    pub(crate) store_timeout: Duration,
    pub(crate) clock: Clock,
    pub(crate) jitter: Jitter,
    pub(crate) http: Http,
    pub(crate) bus: broadcast::Sender<Event>,
    pub(crate) events: broadcast::Sender<AgentStatus>,
    pub(crate) queue: Option<Queue>,
    pub(crate) jobs: JobRegistry,
    pub(crate) job_timeout: Duration,
    pub(crate) worker_shutdown_timeout: Duration,
    pub(crate) health_interval: Duration,
    pub(crate) groups: HashMap<String, Arc<Semaphore>>,
    pub(crate) global: Option<Arc<Semaphore>>,
    pub(crate) shutdown: CancellationToken,
    pub(crate) budget: Duration,
    shutdown_started: OnceLock<Instant>,
    agents: RwLock<BTreeMap<String, Entry>>,
    tasks: Mutex<Option<JoinSet<()>>>,
    stopped: CancellationToken,
    alerts: OnceLock<mpsc::Sender<crate::alert::Alert>>,
    pub(crate) schedule: OnceLock<Arc<Vec<crate::schedule::Entry>>>,
    /// The shared lock store, when processes coordinate (singleton agents, schedule claims).
    pub(crate) coord: Option<Arc<Coordinator>>,
    /// This process's name in run records (the coordinator's owner, or a fresh one).
    pub(crate) process: String,
    /// The singleton agents this process holds the lease of.
    held: Mutex<std::collections::BTreeSet<String>>,
    /// The runs of this process still running, as last persisted (to put them back after a false sweep).
    live_runs: Mutex<BTreeMap<(String, u64), RunRecord>>,
    /// Final run records whose write failed (an outage), oldest first: written again by the process keeper.
    unwritten: Mutex<VecDeque<RunRecord>>,
}

/// Final run records kept for a retry at most; beyond, the oldest is dropped (logged).
const MAX_UNWRITTEN: usize = 1_000;

/// Queue `run` (replacing an earlier record of the same run) and drop the oldest past `max`; the dropped record.
fn push_bounded(queue: &mut VecDeque<RunRecord>, run: RunRecord, max: usize) -> Option<RunRecord> {
    queue.retain(|r| r.agent != run.agent || r.run_id != run.run_id);
    queue.push_back(run);
    if queue.len() > max {
        queue.pop_front()
    } else {
        None
    }
}

/// The settings a [`Shared`] is built from.
pub(crate) struct SharedParts {
    pub(crate) app: App,
    pub(crate) store: Arc<dyn Store>,
    pub(crate) store_timeout: Duration,
    pub(crate) clock: Clock,
    pub(crate) jitter: Jitter,
    pub(crate) http: Http,
    pub(crate) queue: Option<Queue>,
    pub(crate) jobs: JobRegistry,
    pub(crate) job_timeout: Duration,
    pub(crate) health_interval: Duration,
    pub(crate) groups: HashMap<String, usize>,
    pub(crate) max_concurrent: usize,
    pub(crate) shutdown: CancellationToken,
    pub(crate) budget: Duration,
    pub(crate) coord: Option<Arc<Coordinator>>,
}

#[derive(Clone)]
struct Entry {
    commands: mpsc::Sender<Request>,
    status: watch::Receiver<AgentStatus>,
    logs: Arc<LogRing>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Start,
    Stop,
    Pause,
    Resume,
    Restart,
    Remove,
}

type Reply = oneshot::Sender<Result<AgentStatus, Error>>;

struct Request {
    command: Command,
    reply: Reply,
}

impl Shared {
    pub(crate) fn new(parts: SharedParts) -> Arc<Self> {
        let groups = parts
            .groups
            .into_iter()
            .map(|(name, limit)| (name, Arc::new(Semaphore::new(limit.max(1)))))
            .collect();
        let global =
            (parts.max_concurrent > 0).then(|| Arc::new(Semaphore::new(parts.max_concurrent)));
        Arc::new(Self {
            app: parts.app.downgrade(),
            app_name: parts.app.settings().name.clone(),
            store: parts.store,
            store_timeout: parts.store_timeout,
            clock: parts.clock,
            jitter: parts.jitter,
            http: parts.http,
            bus: broadcast::channel(EVENT_CAPACITY).0,
            events: broadcast::channel(STATUS_CAPACITY).0,
            queue: parts.queue,
            jobs: parts.jobs,
            job_timeout: parts.job_timeout,
            worker_shutdown_timeout: Duration::from_secs(10),
            health_interval: parts.health_interval,
            groups,
            global,
            shutdown: parts.shutdown,
            budget: parts.budget,
            shutdown_started: OnceLock::new(),
            agents: RwLock::new(BTreeMap::new()),
            tasks: Mutex::new(Some(JoinSet::new())),
            stopped: CancellationToken::new(),
            alerts: OnceLock::new(),
            schedule: OnceLock::new(),
            process: parts
                .coord
                .as_ref()
                .map_or_else(crate::coord::local_process_name, |c| c.owner().to_owned()),
            coord: parts.coord,
            held: Mutex::new(std::collections::BTreeSet::new()),
            live_runs: Mutex::new(BTreeMap::new()),
            unwritten: Mutex::new(VecDeque::new()),
        })
    }

    fn started_shutdown(&self) -> Instant {
        *self.shutdown_started.get_or_init(Instant::now)
    }

    /// How long a cancelled run may take to return. During shutdown the app's budget caps it,
    /// keeping a slice of the budget (a fifth, at most 2 s) to record the outcomes.
    pub(crate) fn grace(&self, timeout: Duration) -> Duration {
        if !self.shutdown.is_cancelled() {
            return timeout;
        }
        let reserve = (self.budget / 5).min(Duration::from_secs(2));
        let deadline = self.started_shutdown() + self.budget.saturating_sub(reserve);
        timeout.min(deadline.saturating_duration_since(Instant::now()))
    }

    /// A store call with its timeout (during shutdown: at most what is left of the budget).
    pub(crate) async fn timed<T>(
        &self,
        op: &'static str,
        fut: impl Future<Output = Result<T, StoreError>>,
    ) -> Result<T, StoreError> {
        let mut limit = self.store_timeout;
        if self.shutdown.is_cancelled() {
            let end = self.started_shutdown() + self.budget;
            limit = limit
                .min(end.saturating_duration_since(Instant::now()))
                .max(Duration::from_millis(10));
        }
        tokio::time::timeout(limit, fut)
            .await
            .map_err(|_| StoreError::new(op, format!("timed out after {limit:?}")))?
    }

    /// Put this process's running runs back to `running` where another process's sweep marked them interrupted
    /// while its process lease had lapsed, then write the final records that failed meanwhile. The restore is
    /// conditional (only rows the sweep marked), so a run that ended in between keeps its outcome.
    pub(crate) async fn reassert_runs(&self) {
        let live: Vec<RunRecord> = self
            .live_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        let mut restored = 0_usize;
        for run in &live {
            match self.timed("restore_run", self.store.restore_run(run)).await {
                Ok(true) => restored += 1,
                Ok(false) => {}
                Err(e) => {
                    warn!(error = %e, agent = %run.agent, run_id = run.run_id, "could not restore a run record");
                }
            }
        }
        if restored > 0 {
            info!(
                runs = restored,
                "this process's running runs are recorded as running again"
            );
        }
        self.flush_unwritten().await;
    }

    fn forget_unwritten(&self, agent: &str, run_id: u64) {
        self.unwritten
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|r| r.agent != agent || r.run_id != run_id);
    }

    /// Write the final run records whose write failed before (each is kept until a write succeeds).
    pub(crate) async fn flush_unwritten(&self) {
        let waiting: Vec<RunRecord> = self
            .unwritten
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        for run in waiting {
            if self
                .timed("upsert_run", self.store.upsert_run(&run))
                .await
                .is_ok()
            {
                self.forget_unwritten(&run.agent, run.run_id);
            } else {
                // The store still fails: the rest waits for the next pass.
                break;
            }
        }
    }

    /// The singleton agents this process holds now.
    pub(crate) fn held(&self) -> Vec<String> {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    fn set_held(&self, name: &str, held: bool) {
        let mut set = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if held {
            set.insert(name.to_owned());
        } else {
            set.remove(name);
        }
    }

    /// Persist a run record (stamped with this process); a failing store is logged, never fatal.
    pub(crate) async fn persist_run(&self, run: &RunRecord) {
        let stamped;
        let too_long = run
            .error
            .as_ref()
            .is_some_and(|e| e.len() > crate::queue::MAX_STORED_ERROR);
        let run = if run.process.is_empty() || too_long {
            let mut copy = run.clone();
            if copy.process.is_empty() {
                copy.process.clone_from(&self.process);
            }
            copy.error = copy.error.map(|e| crate::queue::stored_error(&e));
            stamped = copy;
            &stamped
        } else {
            run
        };
        {
            let mut live = self.live_runs.lock().unwrap_or_else(|e| e.into_inner());
            let key = (run.agent.clone(), run.run_id);
            if run.outcome == RunOutcome::Running {
                live.insert(key, run.clone());
            } else {
                live.remove(&key);
            }
        }
        match self.timed("upsert_run", self.store.upsert_run(run)).await {
            Ok(()) => self.forget_unwritten(&run.agent, run.run_id),
            Err(e) => {
                warn!(error = %e, agent = %run.agent, run_id = run.run_id, "could not persist run record");
                // A final record is written again later; a lost one would leave the run `running` for good.
                if run.outcome != RunOutcome::Running {
                    let mut unwritten = self.unwritten.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(dropped) = push_bounded(&mut unwritten, run.clone(), MAX_UNWRITTEN)
                    {
                        warn!(
                            agent = %dropped.agent,
                            run_id = dropped.run_id,
                            outcome = dropped.outcome.as_str(),
                            "too many run records wait for a retry; the oldest was dropped"
                        );
                    }
                }
            }
        }
    }

    pub(crate) async fn persist_agent(&self, status: &AgentStatus) {
        let shortened;
        let status = if status
            .last_error
            .as_ref()
            .is_some_and(|e| e.len() > crate::queue::MAX_STORED_ERROR)
        {
            let mut copy = status.clone();
            copy.last_error = copy.last_error.map(|e| crate::queue::stored_error(&e));
            shortened = copy;
            &shortened
        } else {
            status
        };
        if let Err(e) = self
            .timed("upsert_agent", self.store.upsert_agent(status))
            .await
        {
            warn!(error = %e, agent = %status.name, "could not persist agent status");
        }
    }

    /// Report something that needs a human: an error-level `tracing` event with target
    /// `smeltery_watchfire::alert`, and the alert hooks and webhook when they are set up. Never
    /// waits: a full delivery queue drops the alert (logged).
    pub(crate) fn alert(
        &self,
        kind: crate::alert::AlertKind,
        agent: &str,
        job: Option<&str>,
        message: &str,
    ) {
        tracing::error!(target: "smeltery_watchfire::alert", kind = kind.as_str(), agent, job, "{message}");
        let Some(tx) = self.alerts.get() else { return };
        let alert = crate::alert::Alert {
            kind,
            app: self.app_name.clone(),
            agent: agent.to_owned(),
            job: job.map(str::to_owned),
            message: crate::queue::stored_error(message),
            at_ms: self.clock.now_ms(),
        };
        if tx.try_send(alert).is_err() {
            warn!("the alert queue is full; an alert was dropped");
        }
    }

    /// Start delivering alerts to `hooks` and `webhook` (once).
    pub(crate) fn start_alerts(
        self: &Arc<Self>,
        hooks: Vec<crate::alert::AlertHook>,
        webhook: Option<String>,
    ) {
        if hooks.is_empty() && webhook.is_none() {
            return;
        }
        let (tx, rx) = mpsc::channel(crate::alert::ALERT_CAPACITY);
        if self.alerts.set(tx).is_err() {
            return;
        }
        self.spawn_task(crate::alert::deliver(Arc::clone(self), rx, hooks, webhook));
    }

    /// Run a task owned by the runtime (joined on shutdown). It must end once the shutdown
    /// token is cancelled (or, for a lease keeper, once its lease is released or dropped). `false` when the
    /// runtime has been joined already (the task is dropped).
    pub(crate) fn spawn_task(&self, task: impl Future<Output = ()> + Send + 'static) -> bool {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        match tasks.as_mut() {
            Some(set) => {
                set.spawn(task);
                true
            }
            None => false,
        }
    }

    pub(crate) fn status(&self, name: &str) -> Result<AgentStatus, Error> {
        Ok(self.entry(name)?.status.borrow().clone())
    }

    fn entry(&self, name: &str) -> Result<Entry, Error> {
        self.agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
            .ok_or_else(|| Error::UnknownAgent {
                name: name.to_owned(),
            })
    }

    /// Register an agent and spawn its runner. `parent` makes it a child: it exits (and is
    /// removed) when that token is cancelled.
    pub(crate) async fn spawn_agent(
        self: &Arc<Self>,
        name: String,
        config: AgentConfig,
        agent: Box<dyn DynAgent>,
        parent: Option<CancellationToken>,
        framework: bool,
    ) -> Result<(), Error> {
        if self.shutdown.is_cancelled() {
            return Err(Error::ShuttingDown);
        }
        let Some(app) = self.app.upgrade() else {
            return Err(Error::ShuttingDown);
        };
        if self.entry(&name).is_ok() {
            return Err(Error::Duplicate { name });
        }
        if let Some(group) = &config.group
            && !self.groups.contains_key(group)
        {
            crate::config::validate_name(group)?;
        }
        // A singleton runs in one process at a time: its history is loaded (and its interrupted runs marked) only
        // once this process holds its lease, never while another process may be running it.
        let singleton =
            self.coord.is_some() && !framework && parent.is_none() && !config.per_process;
        if singleton
            && let Some(coord) = &self.coord
            && let Err(needed) = coord.cutoff(config.shutdown_timeout)
        {
            return Err(Error::Config(format!(
                "agent `{name}` (shutdown timeout {:?}): {}",
                config.shutdown_timeout,
                coord.too_short(config.shutdown_timeout, needed)
            )));
        }
        let status = if singleton {
            AgentStatus::new(
                &name,
                config.restart,
                config.group.clone(),
                self.clock.now_ms(),
            )
        } else {
            // With coordination, another live process may run an agent of the same name right now: its runs
            // are marked by the process sweep once that process is gone, never here.
            let mark = self.coord.is_none();
            self.restore(&name, &config, mark).await
        };
        let runs = Arc::new(AtomicU64::new(status.runs));
        let (status_tx, status_rx) = watch::channel(status);
        let status_tx = Arc::new(status_tx);
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CAPACITY);
        let logs = Arc::new(LogRing::default());
        {
            let mut agents = self.agents.write().unwrap_or_else(|e| e.into_inner());
            if agents.contains_key(&name) {
                return Err(Error::Duplicate { name });
            }
            agents.insert(
                name.clone(),
                Entry {
                    commands: cmd_tx,
                    status: status_rx,
                    logs: Arc::clone(&logs),
                },
            );
        }
        let is_child = parent.is_some();
        let exit = parent.map_or_else(|| self.shutdown.clone(), |p| p.child_token());
        let runner = Runner {
            app,
            shared: Arc::clone(self),
            name: Arc::from(name.as_str()),
            limiter: Arc::new(Semaphore::new(config.concurrency)),
            config,
            status: status_tx,
            commands: cmd_rx,
            exit,
            remove_on_exit: is_child,
            framework,
            logs,
            runs,
            attempt: 0,
            window: VecDeque::new(),
            singleton,
            lease: None,
        };
        let span = tracing::info_span!("watchfire", agent = %name);
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        match tasks.as_mut() {
            Some(set) => {
                // Forget runners that already ended (removed agents, children).
                while set.try_join_next().is_some() {}
                set.spawn(runner.run(agent).instrument(span));
                Ok(())
            }
            None => {
                drop(tasks);
                self.agents
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&name);
                Err(Error::ShuttingDown)
            }
        }
    }

    /// The initial status from the stored registry row; with `mark`, runs the store still lists as running (the
    /// process died during them) are marked interrupted. Store failures are logged.
    async fn restore(&self, name: &str, config: &AgentConfig, mark: bool) -> AgentStatus {
        let mut status = AgentStatus::new(
            name,
            config.restart,
            config.group.clone(),
            self.clock.now_ms(),
        );
        match self.timed("load_agent", self.store.load_agent(name)).await {
            Ok(Some(stored)) => {
                status.restarts = stored.restarts;
                status.runs = stored.runs;
            }
            Ok(None) => {}
            Err(e) => warn!(error = %e, agent = name, "could not load the agent's history"),
        }
        let now = self.clock.now_ms();
        if mark {
            match self
                .timed("mark_interrupted", self.store.mark_interrupted(name, now))
                .await
            {
                Ok(ids) => {
                    if let Some(max) = ids.iter().max() {
                        status.runs = status.runs.max(*max);
                    }
                }
                Err(e) => warn!(error = %e, agent = name, "could not mark interrupted runs"),
            }
        }
        self.persist_agent(&status).await;
        status
    }

    /// Cancel everything and wait for every runner to finish (within the budget).
    pub(crate) async fn shutdown_and_wait(&self) {
        self.shutdown.cancel();
        self.started_shutdown();
        let set = self.tasks.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(mut set) = set else {
            // Another caller is joining; wait for it.
            self.stopped.cancelled().await;
            return;
        };
        info!("Watchfire shutting down");
        while let Some(joined) = set.join_next().await {
            if let Err(e) = joined {
                warn!(error = %e, "an agent runner ended abnormally");
            }
        }
        self.stopped.cancel();
        info!("Watchfire stopped");
    }
}

/// The handle to the running agents: a service on the app once Watchfire runs
/// (`app.service::<Agents>()`), and an extractor in handlers. Cheap to clone.
///
/// ```
/// # async fn demo(agents: smeltery_watchfire::Agents) -> Result<(), smeltery_watchfire::Error> {
/// agents.pause("scraper").await?;
/// agents.resume("scraper").await?;
/// for status in agents.list() {
///     println!("{} {}", status.name, status.state.as_str());
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Agents {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Agents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agents")
            .field("agents", &self.names())
            .finish_non_exhaustive()
    }
}

impl Agents {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// The agent names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.shared
            .agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// Every agent's status, sorted by name.
    pub fn list(&self) -> Vec<AgentStatus> {
        self.shared
            .agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|e| e.status.borrow().clone())
            .collect()
    }

    /// One agent's status.
    ///
    /// # Errors
    /// [`Error::UnknownAgent`].
    pub fn status(&self, name: &str) -> Result<AgentStatus, Error> {
        self.shared.status(name)
    }

    /// The agent's last log lines (at most 200), oldest first.
    ///
    /// # Errors
    /// [`Error::UnknownAgent`].
    pub fn logs(&self, name: &str) -> Result<Vec<LogLine>, Error> {
        Ok(self.shared.entry(name)?.logs.lines())
    }

    /// The latest `limit` runs (of one agent, or of all), newest first.
    ///
    /// # Errors
    /// The store fails.
    pub async fn runs(&self, agent: Option<&str>, limit: u32) -> Result<Vec<RunRecord>, Error> {
        Ok(self
            .shared
            .timed("recent_runs", self.shared.store.recent_runs(agent, limit))
            .await?)
    }

    /// Start a stopped, completed or failed agent, or a backing-off one now. Returns once
    /// the agent is starting.
    ///
    /// # Errors
    /// Unknown, already running, paused, or shutting down.
    pub async fn start(&self, name: &str) -> Result<AgentStatus, Error> {
        self.send(name, Command::Start).await
    }

    /// Cancel the run and wait for it to stop (at most its shutdown timeout), or cancel a
    /// pending restart.
    ///
    /// # Errors
    /// Unknown, not running, or shutting down.
    pub async fn stop(&self, name: &str) -> Result<AgentStatus, Error> {
        self.send(name, Command::Stop).await
    }

    /// Cancel the run (like stop) and keep the agent from restarting until
    /// [`Agents::resume`].
    ///
    /// # Errors
    /// Unknown or shutting down.
    pub async fn pause(&self, name: &str) -> Result<AgentStatus, Error> {
        self.send(name, Command::Pause).await
    }

    /// Start a paused agent again.
    ///
    /// # Errors
    /// Unknown, not paused, or shutting down.
    pub async fn resume(&self, name: &str) -> Result<AgentStatus, Error> {
        self.send(name, Command::Resume).await
    }

    /// Stop the run if there is one, then start a new one (resetting the backoff).
    ///
    /// # Errors
    /// Unknown or shutting down.
    pub async fn restart(&self, name: &str) -> Result<AgentStatus, Error> {
        self.send(name, Command::Restart).await
    }

    /// Add an agent while Watchfire runs (it starts when its config says autostart).
    ///
    /// # Errors
    /// An invalid or duplicate name, or shutting down.
    pub async fn add(&self, agent: impl Agent) -> Result<AgentStatus, Error> {
        let config = agent.config();
        self.add_with(agent, config).await
    }

    /// [`Agents::add`] with an explicit config.
    ///
    /// # Errors
    /// See [`Agents::add`].
    pub async fn add_with(
        &self,
        agent: impl Agent,
        config: AgentConfig,
    ) -> Result<AgentStatus, Error> {
        let name = agent.name();
        crate::config::validate_name(&name)?;
        self.shared
            .spawn_agent(name.clone(), config, Box::new(agent), None, false)
            .await?;
        self.status(&name)
    }

    /// Stop the agent (if it runs) and remove it. Its history stays in the store.
    ///
    /// # Errors
    /// Unknown or shutting down.
    pub async fn remove(&self, name: &str) -> Result<AgentStatus, Error> {
        self.send(name, Command::Remove).await
    }

    /// Status snapshots, one per change of any agent. A receiver more than 256 updates behind
    /// gets `Lagged` and should re-read [`Agents::list`].
    pub fn subscribe(&self) -> broadcast::Receiver<AgentStatus> {
        self.shared.events.subscribe()
    }

    /// Events emitted with `ctx.emit`.
    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.shared.bus.subscribe()
    }

    /// Emit an event from outside an agent (source `app`).
    ///
    /// # Errors
    /// The payload does not serialize.
    pub fn emit(
        &self,
        event: &str,
        payload: impl serde::Serialize,
    ) -> Result<(), crate::AgentError> {
        let payload = serde_json::to_value(payload)?;
        let _ = self.shared.bus.send(Event {
            name: event.to_owned(),
            source: "app".to_owned(),
            payload,
        });
        Ok(())
    }

    /// The queue, when jobs are set up.
    pub fn queue(&self) -> Option<&Queue> {
        self.shared.queue.as_ref()
    }

    /// The scheduled tasks with their next run after now.
    pub fn schedule(&self) -> Vec<crate::schedule::ScheduleInfo> {
        self.shared.schedule.get().map_or_else(Vec::new, |entries| {
            crate::schedule::infos(entries, self.shared.clock.now_ms())
        })
    }

    /// The token that starts shutdown (a child of the app's).
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shared.shutdown.clone()
    }

    /// Run `task` as a task owned by Watchfire (joined at shutdown; it must end once the
    /// shutdown token is cancelled).
    pub(crate) fn spawn_owned(&self, task: impl Future<Output = ()> + Send + 'static) {
        self.shared.spawn_task(task);
    }

    /// Stop every agent and worker (each within its shutdown timeout, all within the app's
    /// budget), record every outcome and wait for it. Later commands fail with
    /// [`Error::ShuttingDown`].
    pub async fn shutdown(&self) {
        self.shared.shutdown_and_wait().await;
    }

    /// Wait for the shutdown token (the app's), then stop everything (see
    /// [`Agents::shutdown`]).
    pub async fn run_until_shutdown(&self) {
        self.shared.shutdown.cancelled().await;
        self.shared.shutdown_and_wait().await;
    }

    async fn send(&self, name: &str, command: Command) -> Result<AgentStatus, Error> {
        let entry = self.shared.entry(name)?;
        if self.shared.shutdown.is_cancelled() {
            return Err(Error::ShuttingDown);
        }
        let (reply, answer) = oneshot::channel();
        entry
            .commands
            .send(Request { command, reply })
            .await
            .map_err(|_| Error::ShuttingDown)?;
        answer.await.map_err(|_| Error::ShuttingDown)?
    }
}

impl axum_extract::FromRequestParts<App> for Agents {
    type Rejection = smeltery_core::Error;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> Result<Self, Self::Rejection> {
        app.service::<Agents>()
            .map(|a| (*a).clone())
            .ok_or_else(|| {
                smeltery_core::Error::internal("Watchfire is not running in this process")
            })
    }
}

/// Axum's extractor trait, through the core's re-export.
mod axum_extract {
    pub(crate) use smeltery_core::http::FromRequestParts;
}

/// What a runner does next.
enum Next {
    /// A singleton without its lease: wait for it.
    Standby,
    Idle,
    Paused,
    Run {
        replies: Vec<Reply>,
        restart: bool,
    },
    Backoff(Duration),
    Exit(Option<Reply>),
}

/// Why a run was cancelled.
enum Cancel {
    Stop(Reply),
    Pause(Reply),
    Restart(Reply),
    Remove(Reply),
    Exit,
    Stall,
    /// The singleton's lease was lost.
    Lost,
}

type Panic = Box<dyn Any + Send>;
type RunResult = Result<Result<(), crate::AgentError>, Panic>;

/// The task that owns one agent. Only it changes the agent's status.
struct Runner {
    /// Strong while the runner runs; dropped when it ends (at the latest at shutdown).
    app: App,
    shared: Arc<Shared>,
    name: Arc<str>,
    config: AgentConfig,
    status: Arc<watch::Sender<AgentStatus>>,
    commands: mpsc::Receiver<Request>,
    /// Cancelled on shutdown (or when the parent run of a child ends).
    exit: CancellationToken,
    remove_on_exit: bool,
    /// Framework agents (scheduler, queue workers) do not count against the global limit.
    framework: bool,
    limiter: Arc<Semaphore>,
    logs: Arc<LogRing>,
    runs: Arc<AtomicU64>,
    /// Consecutive automatic restarts, for the backoff.
    attempt: u32,
    /// When recent automatic restarts happened, for `max_restarts`.
    window: VecDeque<Instant>,
    /// Runs in one process at a time, holding `lease` (a shared lock store is configured).
    singleton: bool,
    lease: Option<Lease>,
}

impl Runner {
    async fn run(mut self, mut agent: Box<dyn DynAgent>) {
        let mut next = if self.singleton {
            Next::Standby
        } else {
            self.first()
        };
        let reply = loop {
            next = match next {
                Next::Standby => self.standby().await,
                Next::Idle => self.idle().await,
                Next::Paused => self.paused().await,
                Next::Run { replies, restart } => {
                    self.run_once(agent.as_mut(), replies, restart).await
                }
                Next::Backoff(delay) => self.backoff(delay).await,
                Next::Exit(reply) => break reply,
            };
        };
        // The run has ended: another process may take the agent over now.
        if let Some(lease) = self.lease.take() {
            self.shared.set_held(&self.name, false);
            lease.release().await;
        }
        if self.remove_on_exit {
            self.shared
                .agents
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&*self.name);
            info!("agent removed");
        }
        if let Some(reply) = reply {
            let _ = reply.send(Ok(self.snapshot()));
        }
        // Every request gets an answer, including the ones queued during shutdown.
        self.commands.close();
        while let Some(req) = self.commands.recv().await {
            let _ = req.reply.send(Err(Error::ShuttingDown));
        }
    }

    fn refuse(&self, reply: Reply, error: fn(String) -> Error) {
        let _ = reply.send(Err(error(self.name.to_string())));
    }

    /// What follows taking the agent on: a run (autostart) or idle.
    fn first(&self) -> Next {
        if self.config.autostart {
            Next::Run {
                replies: Vec::new(),
                restart: false,
            }
        } else {
            Next::Idle
        }
    }

    /// Cancelled when the singleton's lease is lost (never, without a lease).
    fn lost(&self) -> CancellationToken {
        self.lease
            .as_ref()
            .map_or_else(CancellationToken::new, |l| l.lost().clone())
    }

    /// The lease is gone: stop holding the agent here and wait for the lease again.
    fn demote(&mut self) -> Next {
        self.lease = None;
        self.shared.set_held(&self.name, false);
        self.attempt = 0;
        self.window.clear();
        warn!("lease lost; the agent waits in standby");
        Next::Standby
    }

    /// A singleton waits here until this process takes its lease (at once when it is free), answering commands
    /// with `Standby` meanwhile.
    async fn standby(&mut self) -> Next {
        let Some(coord) = self.shared.coord.clone() else {
            return self.first();
        };
        let mut warned = false;
        loop {
            if self.exit.is_cancelled() {
                return Next::Exit(None);
            }
            let shared = Arc::clone(&self.shared);
            let lock = agent_lock(&self.name);
            let mut holder = None;
            match coord
                .lease(&lock, self.config.shutdown_timeout, |task| {
                    shared.spawn_task(task)
                })
                .await
            {
                Ok(Some(lease)) => {
                    self.lease = Some(lease);
                    self.shared.set_held(&self.name, true);
                    self.take_over().await;
                    return self.first();
                }
                Ok(None) => holder = coord.holder(&lock).await.ok().flatten(),
                Err(e) => {
                    if !warned {
                        warn!(error = %e, store = coord.store(), "cannot reach the lock store; the agent waits in standby");
                        warned = true;
                    }
                }
            }
            if self.status.borrow().state != AgentState::Standby
                || self.status.borrow().held_by != holder
            {
                // Not persisted: the store row belongs to the process that runs the agent.
                self.update(|s| {
                    s.held_by = holder;
                    s.state = AgentState::Standby;
                    s.health = Health::Unknown;
                    s.backoff_ms = None;
                    s.next_restart_at_ms = None;
                });
                info!("standby: another process runs this agent");
            }
            let wait = tokio::time::sleep(coord.retry());
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    biased;
                    () = self.exit.cancelled() => return Next::Exit(None),
                    req = self.commands.recv() => match req {
                        None => return Next::Exit(None),
                        Some(Request { command, reply }) => match command {
                            Command::Remove => {
                                self.remove_on_exit = true;
                                return Next::Exit(Some(reply));
                            }
                            _ => self.refuse(reply, |name| Error::Standby { name }),
                        },
                    },
                    () = &mut wait => break,
                }
            }
        }
    }

    /// This process holds the lease now: load the agent's history and mark the runs the previous holder left
    /// running (it died) as interrupted.
    async fn take_over(&mut self) {
        let restored = self.shared.restore(&self.name, &self.config, true).await;
        self.runs.fetch_max(restored.runs, Ordering::SeqCst);
        let me = self.shared.process.clone();
        self.update(|s| {
            s.held_by = Some(me);
            s.state = AgentState::Stopped;
            s.restarts = restored.restarts;
            s.runs = s.runs.max(restored.runs);
        });
        info!("lease taken: the agent runs in this process");
    }

    async fn idle(&mut self) -> Next {
        let lost = self.lost();
        loop {
            tokio::select! {
                biased;
                () = self.exit.cancelled() => return Next::Exit(None),
                () = lost.cancelled() => return self.demote(),
                req = self.commands.recv() => match req {
                    None => return Next::Exit(None),
                    Some(Request { command, reply }) => match command {
                        Command::Start | Command::Restart => {
                            return Next::Run { replies: vec![reply], restart: false };
                        }
                        Command::Stop => self.refuse(reply, |name| Error::NotRunning { name }),
                        Command::Resume => self.refuse(reply, |name| Error::NotPaused { name }),
                        Command::Pause => {
                            self.settle(AgentState::Paused).await;
                            let _ = reply.send(Ok(self.snapshot()));
                            return Next::Paused;
                        }
                        Command::Remove => {
                            self.remove_on_exit = true;
                            return Next::Exit(Some(reply));
                        }
                    },
                },
            }
        }
    }

    async fn paused(&mut self) -> Next {
        let lost = self.lost();
        loop {
            tokio::select! {
                biased;
                () = self.exit.cancelled() => return Next::Exit(None),
                () = lost.cancelled() => return self.demote(),
                req = self.commands.recv() => match req {
                    None => return Next::Exit(None),
                    Some(Request { command, reply }) => match command {
                        Command::Start => self.refuse(reply, |name| Error::Paused { name }),
                        Command::Resume | Command::Restart => {
                            self.attempt = 0;
                            self.window.clear();
                            return Next::Run { replies: vec![reply], restart: false };
                        }
                        Command::Pause => {
                            let _ = reply.send(Ok(self.snapshot()));
                        }
                        Command::Stop => {
                            self.settle(AgentState::Stopped).await;
                            let _ = reply.send(Ok(self.snapshot()));
                            return Next::Idle;
                        }
                        Command::Remove => {
                            self.remove_on_exit = true;
                            return Next::Exit(Some(reply));
                        }
                    },
                },
            }
        }
    }

    async fn backoff(&mut self, delay: Duration) -> Next {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        let lost = self.lost();
        loop {
            tokio::select! {
                biased;
                () = self.exit.cancelled() => {
                    self.settle(AgentState::Stopped).await;
                    return Next::Exit(None);
                }
                () = lost.cancelled() => return self.demote(),
                req = self.commands.recv() => match req {
                    None => {
                        self.settle(AgentState::Stopped).await;
                        return Next::Exit(None);
                    }
                    Some(Request { command, reply }) => match command {
                        Command::Start => return Next::Run { replies: vec![reply], restart: true },
                        Command::Restart => {
                            self.attempt = 0;
                            return Next::Run { replies: vec![reply], restart: true };
                        }
                        Command::Resume => self.refuse(reply, |name| Error::NotPaused { name }),
                        Command::Stop => {
                            self.attempt = 0;
                            self.settle(AgentState::Stopped).await;
                            info!("pending restart cancelled by stop");
                            let _ = reply.send(Ok(self.snapshot()));
                            return Next::Idle;
                        }
                        Command::Pause => {
                            self.attempt = 0;
                            self.settle(AgentState::Paused).await;
                            let _ = reply.send(Ok(self.snapshot()));
                            return Next::Paused;
                        }
                        Command::Remove => {
                            self.settle(AgentState::Stopped).await;
                            self.remove_on_exit = true;
                            return Next::Exit(Some(reply));
                        }
                    },
                },
                () = &mut sleep => return Next::Run { replies: Vec::new(), restart: true },
            }
        }
    }

    /// Wait for the group and global permits (state `Starting`), answering commands.
    async fn permits(
        &mut self,
    ) -> Result<(Option<OwnedSemaphorePermit>, Option<OwnedSemaphorePermit>), Next> {
        let group = self
            .config
            .group
            .as_ref()
            .and_then(|g| self.shared.groups.get(g))
            .cloned();
        let global = if self.framework {
            None
        } else {
            self.shared.global.clone()
        };
        let acquire = async move {
            let group = match group {
                Some(sem) => Some(sem.acquire_owned().await.ok()?),
                None => None,
            };
            let global = match global {
                Some(sem) => Some(sem.acquire_owned().await.ok()?),
                None => None,
            };
            Some((group, global))
        };
        tokio::pin!(acquire);
        let lost = self.lost();
        loop {
            tokio::select! {
                biased;
                () = self.exit.cancelled() => {
                    self.settle(AgentState::Stopped).await;
                    return Err(Next::Exit(None));
                }
                () = lost.cancelled() => return Err(self.demote()),
                req = self.commands.recv() => match req {
                    None => {
                        self.settle(AgentState::Stopped).await;
                        return Err(Next::Exit(None));
                    }
                    Some(Request { command, reply }) => match command {
                        Command::Start => self.refuse(reply, |name| Error::AlreadyRunning { name }),
                        Command::Resume => self.refuse(reply, |name| Error::NotPaused { name }),
                        Command::Restart => {
                            let _ = reply.send(Ok(self.snapshot()));
                        }
                        Command::Stop => {
                            self.settle(AgentState::Stopped).await;
                            let _ = reply.send(Ok(self.snapshot()));
                            return Err(Next::Idle);
                        }
                        Command::Pause => {
                            self.settle(AgentState::Paused).await;
                            let _ = reply.send(Ok(self.snapshot()));
                            return Err(Next::Paused);
                        }
                        Command::Remove => {
                            self.settle(AgentState::Stopped).await;
                            self.remove_on_exit = true;
                            return Err(Next::Exit(Some(reply)));
                        }
                    },
                },
                permits = &mut acquire => match permits {
                    Some(permits) => return Ok(permits),
                    None => {
                        // A semaphore closed: only on teardown.
                        self.settle(AgentState::Stopped).await;
                        return Err(Next::Exit(None));
                    }
                },
            }
        }
    }

    async fn run_once(
        &mut self,
        agent: &mut dyn DynAgent,
        replies: Vec<Reply>,
        restart: bool,
    ) -> Next {
        self.update(|s| {
            s.state = AgentState::Starting;
            s.health = Health::Unknown;
            s.backoff_ms = None;
            s.next_restart_at_ms = None;
        });
        for reply in replies {
            let _ = reply.send(Ok(self.snapshot()));
        }
        let permits = match self.permits().await {
            Ok(permits) => permits,
            Err(next) => return next,
        };

        let run_id = self.runs.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        let token = self.exit.child_token();
        let beat = Arc::new(watch::channel::<Beat>(None).0);
        let ctx = AgentCtx::new(CtxParts {
            app: self.app.clone(),
            name: Arc::clone(&self.name),
            run_id,
            token: token.clone(),
            beat: Arc::clone(&beat),
            limiter: Arc::clone(&self.limiter),
            shared: Arc::clone(&self.shared),
            counters: Arc::default(),
            logs: Arc::clone(&self.logs),
            runs: Arc::clone(&self.runs),
            status: Some(Arc::clone(&self.status)),
        });
        let span = ctx.span().clone();
        let counters_ctx = ctx.clone();
        let started = Instant::now();
        let mut record = RunRecord::started(&self.name, run_id, None, self.shared.clock.now_ms());
        let monitored = self.config.heartbeat_timeout.is_some();
        self.update(|s| {
            s.state = AgentState::Running;
            s.health = if monitored {
                Health::Healthy
            } else {
                Health::Unknown
            };
            s.runs = s.runs.max(run_id);
            s.started_at_ms = Some(record.started_at_ms);
            s.last_heartbeat_ms = None;
            if restart {
                s.restarts = s.restarts.saturating_add(1);
            }
        });
        self.persist().await;
        self.shared.persist_run(&record).await;
        let lost = self.lost();
        if lost.is_cancelled() {
            // The lease went while the start was being recorded: the run never starts here.
            drop(permits);
            self.end_run(
                &mut record,
                RunOutcome::Stopped,
                Some("the lease was lost before the run started".to_owned()),
            )
            .await;
            return self.demote();
        }
        info!(run_id, restart, "run started");

        let mut fut =
            Box::pin(AssertUnwindSafe(agent.run_boxed(ctx).instrument(span)).catch_unwind());
        let check_every = match self.config.heartbeat_timeout {
            Some(limit) => {
                (limit / 4).clamp(Duration::from_millis(10), self.shared.health_interval)
            }
            None => self.shared.health_interval,
        };
        let mut health = tokio::time::interval_at(started + check_every, check_every);
        health.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // `returned`: the run ended on its own after the exit token was cancelled (its token is
        // a child, so it may notice first).
        let (cancel, returned): (Cancel, Option<RunResult>) = loop {
            let result = tokio::select! {
                biased;
                result = &mut fut => result,
                () = self.exit.cancelled() => break (Cancel::Exit, None),
                () = lost.cancelled() => break (Cancel::Lost, None),
                req = self.commands.recv() => match req {
                    None => break (Cancel::Exit, None),
                    Some(Request { command, reply }) => match command {
                        Command::Start => {
                            self.refuse(reply, |name| Error::AlreadyRunning { name });
                            continue;
                        }
                        Command::Resume => {
                            self.refuse(reply, |name| Error::NotPaused { name });
                            continue;
                        }
                        Command::Stop => break (Cancel::Stop(reply), None),
                        Command::Pause => break (Cancel::Pause(reply), None),
                        Command::Restart => break (Cancel::Restart(reply), None),
                        Command::Remove => break (Cancel::Remove(reply), None),
                    },
                },
                _ = health.tick() => {
                    let (stalled, changed) = self.check_health(&beat, started);
                    let mut ended = None;
                    if changed {
                        // The run is polled while the status is written: it may be waiting for a pooled database
                        // connection that the pool has already handed to it (first come, first served). Not
                        // polling it would leave that connection unused while this write waits for one, until
                        // the pool's acquire timeout; with a pool of one (in-memory SQLite) nothing else gets
                        // through meanwhile (D-390).
                        let mut lost_now = false;
                        {
                            let persist = self.persist();
                            tokio::pin!(persist);
                            loop {
                                tokio::select! {
                                    biased;
                                    // A slow store write must not delay the stop past the lease's cut-off.
                                    () = lost.cancelled() => {
                                        lost_now = true;
                                        break;
                                    }
                                    () = &mut persist => break,
                                    result = &mut fut, if ended.is_none() => ended = Some(result),
                                }
                            }
                        }
                        if lost_now {
                            break (Cancel::Lost, ended);
                        }
                    }
                    match ended {
                        Some(result) => result,
                        None if stalled => break (Cancel::Stall, None),
                        None => continue,
                    }
                }
            };
            // The run ended on its own.
            if self.exit.is_cancelled() {
                break (Cancel::Exit, Some(result));
            }
            drop(fut);
            token.cancel();
            drop(permits);
            record.counters = counters_ctx.counters();
            return self.finished(&mut record, started, result).await;
        };

        token.cancel();
        if matches!(cancel, Cancel::Lost) {
            // Stop persisting the agent's status: it belongs to the next holder now.
            self.lease = None;
            self.shared.set_held(&self.name, false);
            warn!(run_id, "lease lost; stopping the run");
        }
        let grace = match cancel {
            Cancel::Exit => self.shared.grace(self.config.shutdown_timeout),
            _ => self.config.shutdown_timeout,
        };
        // The run returned on its own just before the lease was lost (during a status write): its own outcome.
        let ended_itself = returned.is_some() && matches!(cancel, Cancel::Lost);
        let result = match returned {
            Some(result) => Ok(result),
            None => {
                self.update(|s| s.state = AgentState::Stopping);
                tokio::time::timeout(grace, &mut fut).await
            }
        };
        drop(fut);
        drop(permits);
        let (outcome, error) = if matches!(cancel, Cancel::Stall) {
            let limit = self.config.heartbeat_timeout.unwrap_or_default();
            (
                RunOutcome::Stalled,
                Some(format!("no heartbeat for {limit:?}")),
            )
        } else {
            match result {
                Ok(Ok(Ok(()))) if ended_itself => (RunOutcome::Completed, None),
                Ok(Ok(Err(e))) if ended_itself => (RunOutcome::Failed, Some(e.to_string())),
                Ok(Ok(Ok(()))) => (RunOutcome::Stopped, None),
                Ok(Ok(Err(e))) => (RunOutcome::Stopped, Some(e.to_string())),
                Ok(Err(panic)) => (RunOutcome::Panicked, Some(panic_message(&*panic))),
                Err(_) => (
                    RunOutcome::Killed,
                    Some(format!("did not stop within {grace:?}")),
                ),
            }
        };
        if outcome == RunOutcome::Killed {
            warn!(run_id, "run did not stop in time and was dropped");
        }
        record.counters = counters_ctx.counters();
        self.end_run(&mut record, outcome, error).await;

        match cancel {
            Cancel::Restart(reply) => {
                self.attempt = 0;
                Next::Run {
                    replies: vec![reply],
                    restart: true,
                }
            }
            Cancel::Stop(reply) => {
                self.attempt = 0;
                self.settle(AgentState::Stopped).await;
                info!(run_id, outcome = outcome.as_str(), "stopped");
                let _ = reply.send(Ok(self.snapshot()));
                Next::Idle
            }
            Cancel::Pause(reply) => {
                self.attempt = 0;
                self.settle(AgentState::Paused).await;
                info!(run_id, outcome = outcome.as_str(), "paused");
                let _ = reply.send(Ok(self.snapshot()));
                Next::Paused
            }
            Cancel::Remove(reply) => {
                self.settle(AgentState::Stopped).await;
                self.remove_on_exit = true;
                Next::Exit(Some(reply))
            }
            Cancel::Exit => {
                self.settle(AgentState::Stopped).await;
                info!(run_id, outcome = outcome.as_str(), "stopped for shutdown");
                Next::Exit(None)
            }
            Cancel::Lost => self.demote(),
            Cancel::Stall => {
                self.shared.alert(
                    crate::alert::AlertKind::Stalled,
                    &self.name,
                    None,
                    &format!("run {run_id} stalled (no heartbeat); restarting"),
                );
                self.after_end(outcome, started).await
            }
        }
    }

    /// The run returned on its own: record it and apply the restart policy.
    async fn finished(&mut self, run: &mut RunRecord, started: Instant, result: RunResult) -> Next {
        let (outcome, error) = match result {
            Ok(Ok(())) => (RunOutcome::Completed, None),
            Ok(Err(e)) => (RunOutcome::Failed, Some(e.to_string())),
            Err(panic) => (RunOutcome::Panicked, Some(panic_message(&*panic))),
        };
        match &error {
            Some(e) => {
                warn!(run_id = run.run_id, outcome = outcome.as_str(), error = %e, "run ended")
            }
            None => info!(run_id = run.run_id, outcome = outcome.as_str(), "run ended"),
        }
        self.end_run(run, outcome, error).await;
        self.after_end(outcome, started).await
    }

    /// Apply the restart policy, the restart limit and the backoff after a run ended.
    async fn after_end(&mut self, outcome: RunOutcome, started: Instant) -> Next {
        let restart = match self.config.restart {
            Restart::Never => false,
            Restart::OnFailure => outcome != RunOutcome::Completed,
            Restart::Always => true,
        };
        if !restart {
            if outcome == RunOutcome::Completed {
                self.settle(AgentState::Completed).await;
            } else {
                self.settle(AgentState::Failed).await;
                self.shared.alert(
                    crate::alert::AlertKind::Failed,
                    &self.name,
                    None,
                    &format!("agent failed ({})", outcome.as_str()),
                );
            }
            return Next::Idle;
        }
        let backoff = self.config.backoff.clone();
        if started.elapsed() >= backoff.reset_after() {
            self.attempt = 0;
        }
        if let Some((max, per)) = self.config.max_restarts {
            let now = Instant::now();
            while self
                .window
                .front()
                .is_some_and(|t| now.duration_since(*t) > per)
            {
                self.window.pop_front();
            }
            if self.window.len() >= usize::try_from(max).unwrap_or(usize::MAX) {
                let message = format!("restarted {max} times within {per:?}; giving up");
                self.update(|s| s.last_error = Some(message.clone()));
                self.settle(AgentState::Failed).await;
                self.shared
                    .alert(crate::alert::AlertKind::Failed, &self.name, None, &message);
                self.window.clear();
                self.attempt = 0;
                return Next::Idle;
            }
            self.window.push_back(now);
        }
        let delay = backoff.delay(self.attempt, &self.shared.jitter);
        self.attempt = self.attempt.saturating_add(1);
        let delay_ms = duration_ms(delay);
        let now_ms = self.shared.clock.now_ms();
        self.update(|s| {
            s.state = AgentState::BackingOff;
            s.health = Health::Unknown;
            s.backoff_ms = Some(delay_ms);
            s.next_restart_at_ms = Some(now_ms.saturating_add(delay_ms));
        });
        self.persist().await;
        info!(delay_ms, "restarting after backoff");
        Next::Backoff(delay)
    }

    async fn end_run(&mut self, run: &mut RunRecord, outcome: RunOutcome, error: Option<String>) {
        let error = error.map(|e| crate::queue::stored_error(&e));
        run.ended_at_ms = Some(self.shared.clock.now_ms());
        run.outcome = outcome;
        run.error = error.clone();
        if outcome.is_failure() {
            self.update(|s| s.last_error = error);
        }
        self.shared.persist_run(run).await;
    }

    /// Enter a resting state and persist it.
    async fn settle(&mut self, state: AgentState) {
        self.update(|s| {
            s.state = state;
            s.health = Health::Unknown;
            s.backoff_ms = None;
            s.next_restart_at_ms = None;
        });
        self.persist().await;
    }

    /// Update health from the heartbeat: whether the run stalled, and whether the status changed (to persist).
    fn check_health(&mut self, beat: &watch::Sender<Beat>, started: Instant) -> (bool, bool) {
        let last = *beat.borrow();
        let stalled = self.config.heartbeat_timeout.is_some_and(|limit| {
            let since = last.map_or(started, |(at, _)| at);
            since.elapsed() > limit
        });
        let health = match (self.config.heartbeat_timeout, stalled) {
            (None, _) => Health::Unknown,
            (Some(_), true) => Health::Stalled,
            (Some(_), false) => Health::Healthy,
        };
        let beat_ms = last.map(|(_, ms)| ms);
        let changed = {
            let s = self.status.borrow();
            s.health != health || s.last_heartbeat_ms != beat_ms
        };
        if changed {
            if stalled {
                warn!("heartbeat timed out; the run is stalled");
            }
            self.update(|s| {
                s.health = health;
                s.last_heartbeat_ms = beat_ms;
            });
        }
        (stalled, changed)
    }

    fn snapshot(&self) -> AgentStatus {
        self.status.borrow().clone()
    }

    /// Change the status and publish it.
    fn update(&self, f: impl FnOnce(&mut AgentStatus)) {
        let now = self.shared.clock.now_ms();
        self.status.send_modify(|s| {
            f(s);
            s.updated_at_ms = now;
        });
        // No subscribers is fine.
        let _ = self.shared.events.send(self.snapshot());
    }

    async fn persist(&self) {
        if self.singleton && self.lease.is_none() {
            return;
        }
        let status = self.snapshot();
        self.shared.persist_agent(&status).await;
    }
}

/// The lease name of a singleton agent.
pub(crate) fn agent_lock(name: &str) -> String {
    format!("watchfire:agent:{name}")
}

pub(crate) fn panic_message(panic: &(dyn Any + Send)) -> String {
    let msg = panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned());
    format!("panicked: {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_messages() {
        assert_eq!(panic_message(&"x"), "panicked: x");
        assert_eq!(panic_message(&String::from("y")), "panicked: y");
        assert_eq!(panic_message(&7_u8), "panicked: non-string panic payload");
    }

    #[test]
    fn the_retry_queue_drops_the_oldest_record() {
        let mut queue = VecDeque::new();
        let run = |agent: &str, id: u64| RunRecord::started(agent, id, None, 0);
        assert!(push_bounded(&mut queue, run("beta", 1), 2).is_none());
        assert!(push_bounded(&mut queue, run("alpha", 2), 2).is_none());
        // The oldest goes, not the smallest name.
        let dropped = push_bounded(&mut queue, run("gamma", 3), 2).unwrap();
        assert_eq!((dropped.agent.as_str(), dropped.run_id), ("beta", 1));
        // A newer record of a queued run replaces it and counts as new.
        assert!(push_bounded(&mut queue, run("alpha", 2), 2).is_none());
        let dropped = push_bounded(&mut queue, run("delta", 4), 2).unwrap();
        assert_eq!((dropped.agent.as_str(), dropped.run_id), ("gamma", 3));
        let left: Vec<(&str, u64)> = queue.iter().map(|r| (r.agent.as_str(), r.run_id)).collect();
        assert_eq!(left, [("alpha", 2), ("delta", 4)]);
    }
}
