//! Agents that run in other processes: what the shared tables say about them, and commands for them through the
//! `watchfire_commands` table.
//!
//! A web process (`serve --no-agents`) or a process where an agent is in `standby` reads the agent's row in
//! `watchfire_agents`, its runs in `watchfire_runs` and the holder of its lease from the lock store. A command for
//! such an agent becomes a row in `watchfire_commands`; every process that coordinates polls the table once a second
//! for the agents it holds, takes a row with a compare-and-set update, carries the command out through its own
//! runner and writes the outcome. The requester waits up to [`COMMAND_WAIT`] for it; a row nobody finished within
//! [`COMMAND_TTL`] lapses (and says so).

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use smeltery_core::App;

use crate::config::AgentConfig;
use crate::coord::Coordinator;
use crate::error::Error;
use crate::policy::Restart;
use crate::registry::Watchfire;
use crate::runtime::{Agents, Shared, agent_lock};
use crate::schedule::{ScheduleInfo, Timing};
use crate::status::{AgentState, AgentStatus, RunRecord};
use crate::store::{AgentRowData, DbStore, Store};
use crate::time::system_ms;

/// How long a request waits for the process holding the agent to carry a command out.
pub(crate) const COMMAND_WAIT: Duration = Duration::from_secs(5);
/// A command nobody took within this lapses.
pub(crate) const COMMAND_TTL: Duration = Duration::from_secs(60);
/// A command taken but not finished within `COMMAND_TTL` + this is closed as "outcome unknown" (longer than any
/// agent's stop).
pub(crate) const TAKEN_GRACE: Duration = Duration::from_secs(600);
/// How often a coordinating process looks for commands for the agents it holds.
pub(crate) const POLL: Duration = Duration::from_secs(1);
/// Commands that may wait per agent.
const MAX_WAITING: u64 = 20;
/// Finished commands are deleted after a day.
const KEEP_DONE_MS: i64 = 86_400_000;
/// Finished `logs` answers (an agent's log lines) are deleted after a minute when their requester did not delete them.
const KEEP_LOGS_MS: i64 = 60_000;
/// Lease-holder lookups one page render runs at once.
const LOOKUPS: usize = 8;
/// Agents whose commands one process carries out at once.
const COMMAND_AGENTS: usize = 8;

/// The actions a command may name.
pub(crate) const ACTIONS: [&str; 5] = ["start", "stop", "pause", "resume", "restart"];

/// An agent as the app registered it (for a process that does not run Watchfire).
#[derive(Clone, Debug)]
pub(crate) struct RegisteredAgent {
    pub(crate) name: String,
    pub(crate) restart: Restart,
    pub(crate) group: Option<String>,
    pub(crate) per_process: bool,
}

/// A scheduled task as the app registered it.
#[derive(Clone, Debug)]
pub(crate) struct RegisteredTask {
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) timing: Option<Timing>,
    pub(crate) per_process: bool,
}

/// What the app registered (a service, also in processes that do not run Watchfire).
#[derive(Clone, Debug, Default)]
pub(crate) struct Registered {
    pub(crate) agents: Vec<RegisteredAgent>,
    pub(crate) schedule: Vec<RegisteredTask>,
}

impl Registered {
    pub(crate) fn of(w: &Watchfire) -> Self {
        Self {
            agents: w
                .agents
                .iter()
                .map(|r| RegisteredAgent {
                    name: r.name.clone(),
                    restart: r.config.restart,
                    group: r.config.group.clone(),
                    per_process: r.config.per_process,
                })
                .collect(),
            schedule: w
                .schedule
                .iter()
                .map(|e| RegisteredTask {
                    name: e.name.clone(),
                    kind: e.kind(),
                    timing: e.timing.clone(),
                    per_process: e.per_process,
                })
                .collect(),
        }
    }

    /// The schedule with the next run after `now` (on the ticks coordinated processes use when `aligned`).
    pub(crate) fn schedule_infos(&self, now: i64, aligned: bool) -> Vec<ScheduleInfo> {
        self.schedule
            .iter()
            .map(|t| ScheduleInfo {
                name: t.name.clone(),
                kind: t.kind,
                expression: t.timing.as_ref().map(Timing::describe).unwrap_or_default(),
                next_run_ms: t.timing.as_ref().and_then(|timing| {
                    if aligned && !t.per_process {
                        timing.first_aligned(now)
                    } else {
                        timing.first_after(now)
                    }
                }),
            })
            .collect()
    }
}

/// The shared tables (with the several-process schema) and a view of the lock store.
#[derive(Clone, Debug)]
pub(crate) struct Remote {
    pub(crate) store: DbStore,
    /// Who holds which lease; `None` when processes do not coordinate (then nobody carries commands out).
    pub(crate) locks: Option<Arc<Coordinator>>,
}

/// The app's [`Remote`], found on first use (after the migrations; the answer is kept once the tables exist).
#[derive(Default)]
pub(crate) struct RemoteCell {
    found: tokio::sync::OnceCell<Remote>,
}

/// The app's shared tables and lock view, when its database has the several-process schema.
pub(crate) async fn remote_of(app: &App) -> Option<Remote> {
    let cell = app.service::<RemoteCell>()?;
    if let Some(found) = cell.found.get() {
        return Some(found.clone());
    }
    let db = app.db().ok()?;
    if !DbStore::has_processes(&db).await {
        return None;
    }
    let settings = crate::web::settings(app);
    let remote = Remote {
        store: DbStore::open(db).await,
        locks: crate::app::lock_view(app, &settings),
    };
    Some(cell.found.get_or_init(|| async { remote }).await.clone())
}

/// Use `remote` for this app (tests: a fake lock view).
#[cfg(test)]
pub(crate) fn set_remote(app: &App, remote: Remote) {
    let cell = RemoteCell::default();
    let _ = cell.found.set(remote);
    app.insert_service(cell);
}

/// What a command came to.
#[derive(Debug)]
pub(crate) enum Control {
    /// Not one of [`ACTIONS`].
    UnknownAction,
    /// Watchfire does not run here, and no other process can be reached.
    NotRunning,
    /// Carried out by this process.
    Local(Result<AgentStatus, Error>),
    /// Carried out by the process holding the agent: its status after, or its refusal (HTTP status, message).
    Remote(Result<AgentStatus, (u16, String)>),
    /// Taken by this process, which had not recorded the outcome within [`COMMAND_WAIT`].
    Taken(String),
    /// Queued; no process took it within [`COMMAND_WAIT`].
    Queued,
    /// Not queued: too many commands wait for the agent.
    QueueFull(String),
    /// The commands table could not be used.
    StoreError(String),
}

impl Control {
    /// The line the dashboard shows.
    pub(crate) fn notice(&self, name: &str, action: &str) -> String {
        match self {
            Self::UnknownAction => format!("Unknown action `{action}`."),
            Self::NotRunning => "Watchfire is not running in this process.".to_owned(),
            Self::Local(Ok(status)) => {
                format!("{name}: {}.", status.state.as_str().replace('_', " "))
            }
            Self::Local(Err(e)) => format!("{e}."),
            Self::Remote(Ok(status)) => format!(
                "{name}: {} (in {}).",
                status.state.as_str().replace('_', " "),
                status.held_by.as_deref().unwrap_or("another process")
            ),
            Self::Remote(Err((_, e))) => format!("{e}."),
            Self::Taken(process) => format!(
                "{name}: {action} is being carried out by {process} (no outcome within {}s).",
                COMMAND_WAIT.as_secs()
            ),
            Self::Queued => format!(
                "{name}: {action} requested; no process holding it took it within {}s. It is carried out when a \
                 process holding the agent takes it, or lapses after {}s.",
                COMMAND_WAIT.as_secs(),
                COMMAND_TTL.as_secs()
            ),
            Self::QueueFull(why) => format!("{name}: {action} not queued: {why}."),
            Self::StoreError(why) => format!("{name}: {action}: the commands table failed: {why}."),
        }
    }
}

impl Remote {
    /// Every agent row, by name (empty when the database fails; logged).
    pub(crate) async fn rows(&self) -> HashMap<String, AgentRowData> {
        match self.store.agent_rows().await {
            Ok(rows) => rows.into_iter().map(|r| (r.name.clone(), r)).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the agents table");
                HashMap::new()
            }
        }
    }

    /// The process holding agent `name`'s lease.
    pub(crate) async fn holder(&self, name: &str) -> Option<String> {
        let locks = self.locks.as_ref()?;
        locks.holder(&agent_lock(name)).await.ok().flatten()
    }

    /// The runs in the shared history.
    pub(crate) async fn runs(
        &self,
        agent: Option<&str>,
        limit: u32,
    ) -> Result<Vec<RunRecord>, Error> {
        Ok(self.store.recent_runs(agent, limit).await?)
    }

    /// The agent's status from its row and its lease holder.
    pub(crate) async fn status(
        &self,
        name: &str,
        registered: Option<&RegisteredAgent>,
        row: Option<&AgentRowData>,
    ) -> AgentStatus {
        let holder = self.holder(name).await;
        status_from(name, registered, row, holder)
    }

    /// A short summary of the agent rows, to tell whether anything changed.
    pub(crate) async fn fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Ok(rows) = self.store.agent_rows().await {
            for row in rows {
                (
                    row.name,
                    row.state,
                    row.updated_at_ms,
                    row.restarts,
                    row.runs,
                )
                    .hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// Ask the process holding `name` to carry `action` out, and wait up to [`COMMAND_WAIT`] for it.
    pub(crate) async fn command(
        &self,
        name: &str,
        action: &str,
        registered: Option<&RegisteredAgent>,
    ) -> Control {
        match self.ask(name, action).await {
            Asked::Done(_, outcome) if outcome.ok => {
                let rows = self.rows().await;
                Control::Remote(Ok(self.status(name, registered, rows.get(name)).await))
            }
            Asked::Done(_, outcome) => {
                Control::Remote(Err((outcome.code.unwrap_or(409), outcome.result)))
            }
            Asked::Taken(process) => Control::Taken(process),
            Asked::Queued => Control::Queued,
            Asked::QueueFull(why) => Control::QueueFull(why),
            Asked::StoreError(why) => Control::StoreError(why),
            Asked::NotRunning => Control::NotRunning,
        }
    }

    /// The last log lines of agent `name` from the process holding it (JSON, as `Agents::logs` serializes them), or
    /// the HTTP status and message to answer instead.
    pub(crate) async fn logs(&self, name: &str) -> Result<serde_json::Value, (u16, String)> {
        let no_answer = |what: &str| {
            format!(
                "agent `{name}` runs in another process, and {what} within {}s; its log lines are kept there",
                COMMAND_WAIT.as_secs()
            )
        };
        let asked = self.ask(name, LOGS).await;
        // Log lines may carry what should not sit in the database (and its backups): the row goes once read; one
        // nobody read goes with the lapse after a minute.
        if let Asked::Done(id, _) = &asked
            && let Err(e) = self.store.command_delete(*id, LOGS).await
        {
            tracing::warn!(error = %e, "cannot delete a read `logs` answer; it lapses within a minute");
        }
        match asked {
            Asked::Done(_, outcome) if outcome.ok => {
                serde_json::from_str(&outcome.result).map_err(|e| {
                    (
                        502,
                        format!("the holder's log lines could not be read: {e}"),
                    )
                })
            }
            Asked::Done(_, outcome) => Err((outcome.code.unwrap_or(409), outcome.result)),
            Asked::Taken(process) => Err((504, no_answer(&format!("{process} did not answer")))),
            Asked::Queued => Err((504, no_answer("no process holding it answered"))),
            Asked::QueueFull(why) => Err((429, why)),
            Asked::StoreError(why) => Err((503, format!("the commands table failed: {why}"))),
            Asked::NotRunning => Err((503, "Watchfire is not running in this process.".to_owned())),
        }
    }

    /// Queue `action` for `name` and wait up to [`COMMAND_WAIT`] for its outcome.
    async fn ask(&self, name: &str, action: &str) -> Asked {
        if self.locks.is_none() {
            return Asked::NotRunning;
        }
        // The database's clock stamps and lapses every command, whatever the processes' clocks say.
        let now = match self.store.db_now().await {
            Ok(now) => now,
            Err(e) => return Asked::StoreError(e.to_string()),
        };
        // Old rows lapse here too, so the table stays bounded also while no Watchfire process polls it.
        if let Err(e) = lapse(&self.store, now).await {
            tracing::warn!(error = %e, "cannot tidy the commands table");
        }
        let id = match self
            .store
            .command_request(name, action, now, MAX_WAITING)
            .await
        {
            Ok(Ok(id)) => id,
            Ok(Err(why)) => return Asked::QueueFull(why),
            Err(e) => return Asked::StoreError(e.to_string()),
        };
        let deadline = tokio::time::Instant::now() + COMMAND_WAIT;
        loop {
            let state = match self.store.command_state(id).await {
                Ok(state) => state,
                Err(e) => return Asked::StoreError(e.to_string()),
            };
            if let Some(outcome) = state.outcome {
                return Asked::Done(id, outcome);
            }
            if tokio::time::Instant::now() >= deadline {
                return match state.taken_by {
                    Some(process) => Asked::Taken(process),
                    None => Asked::Queued,
                };
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// What a queued command came to, as the requester sees it.
enum Asked {
    /// The row's id and its outcome.
    Done(i64, crate::store::CommandOutcome),
    Taken(String),
    Queued,
    QueueFull(String),
    StoreError(String),
    NotRunning,
}

/// The command that reads an agent's log lines in the process holding it (not a state change, so not in [`ACTIONS`]).
pub(crate) const LOGS: &str = "logs";

/// The most JSON a `logs` answer may hold (the `result` column is `TEXT`: 64 KB on MySQL); older lines go first.
const LOGS_MAX_BYTES: usize = 60_000;

/// `lines` as JSON, dropping the oldest until it fits in `max` bytes.
pub(crate) fn bounded_logs(lines: &[crate::status::LogLine], max: usize) -> String {
    let mut from = 0;
    loop {
        let json = serde_json::to_string(lines.get(from..).unwrap_or_default())
            .unwrap_or_else(|_| "[]".to_owned());
        if json.len() <= max || from >= lines.len() {
            return json;
        }
        // Drop about the share that is too much (at least one line).
        let over = json.len() - max;
        let per_line = json.len() / (lines.len() - from).max(1);
        from += (over / per_line.max(1)).max(1);
        from = from.min(lines.len());
    }
}

/// The log lines of agent `name` as this process can see them: its own, or those of the process holding it.
/// `None` when Watchfire neither runs here nor can be reached through the shared tables.
pub(crate) async fn logs_view(
    app: &App,
    name: &str,
) -> Option<Result<serde_json::Value, (u16, String)>> {
    let local = app.service::<Agents>();
    if let Some(local) = &local
        && let Ok(status) = local.status(name)
        && status.state != AgentState::Standby
    {
        return Some(
            local
                .logs(name)
                .map(|lines| serde_json::to_value(lines).unwrap_or_default())
                .map_err(|e| (crate::web::api::status_of(&e).as_u16(), e.to_string())),
        );
    }
    let remote = remote_of(app).await.filter(|r| r.locks.is_some());
    let registered = app.service::<Registered>();
    let reg = registered
        .as_ref()
        .and_then(|r| r.agents.iter().find(|a| a.name == name).cloned());
    let elsewhere = match &local {
        Some(local) => local
            .status(name)
            .is_ok_and(|s| s.state == AgentState::Standby),
        None => reg.as_ref().is_some_and(|r| !r.per_process),
    };
    match (remote, &local) {
        (Some(remote), _) if elsewhere => Some(remote.logs(name).await),
        (_, Some(local)) => Some(
            local
                .logs(name)
                .map(|lines| serde_json::to_value(lines).unwrap_or_default())
                .map_err(|e| (crate::web::api::status_of(&e).as_u16(), e.to_string())),
        ),
        (Some(_), None) if reg.is_none() && registered.is_some() => Some(Err((
            404,
            Error::UnknownAgent {
                name: name.to_owned(),
            }
            .to_string(),
        ))),
        _ => None,
    }
}

/// Lapse old commands and forget finished ones, by the database's clock `now`.
async fn lapse(store: &DbStore, now: i64) -> Result<(), crate::StoreError> {
    let ttl = crate::time::duration_ms(COMMAND_TTL);
    let grace = crate::time::duration_ms(TAKEN_GRACE);
    store
        .commands_lapse(
            now - ttl,
            now - ttl - grace,
            now - KEEP_DONE_MS,
            (LOGS, now - KEEP_LOGS_MS),
            now,
        )
        .await
}

/// An agent's status from its stored row (`None`: never recorded) and the holder of its lease.
pub(crate) fn status_from(
    name: &str,
    registered: Option<&RegisteredAgent>,
    row: Option<&AgentRowData>,
    holder: Option<String>,
) -> AgentStatus {
    let restart = registered.map_or_else(|| AgentConfig::default().restart_policy(), |r| r.restart);
    let group = registered.and_then(|r| r.group.clone());
    let mut status = AgentStatus::new(name, restart, group, row.map_or(0, |r| r.updated_at_ms));
    if let Some(row) = row {
        status.state = AgentState::parse(&row.state).unwrap_or(AgentState::Stopped);
        status.restarts = row.restarts;
        status.runs = row.runs;
        status.started_at_ms = row.started_at_ms;
        status.last_heartbeat_ms = row.last_heartbeat_ms;
        status.last_error.clone_from(&row.last_error);
    }
    status.held_by = holder;
    status
}

/// Every agent as this process sees it: its own (with the shared row for those in `standby`), or, when Watchfire
/// does not run here, the registered singleton agents from the shared tables. `None` when there is neither.
pub(crate) async fn agent_view(app: &App) -> Option<Vec<AgentStatus>> {
    let remote = remote_of(app).await;
    if let Some(local) = app.service::<Agents>() {
        let mut list = local.list();
        if let Some(remote) = remote
            && list.iter().any(|s| s.state == AgentState::Standby)
        {
            let rows = remote.rows().await;
            for status in list.iter_mut().filter(|s| s.state == AgentState::Standby) {
                if let Some(row) = rows.get(&status.name) {
                    let holder = status.held_by.take();
                    *status = status_from(&status.name, None, Some(row), holder);
                }
            }
        }
        return Some(list);
    }
    let remote = remote?;
    let registered = app
        .service::<Registered>()
        .map(|r| (*r).clone())
        .unwrap_or_default();
    let rows = remote.rows().await;
    // One lock-store lookup per agent, at most LOOKUPS at once: a pool would otherwise wait for each in turn, and a
    // large one would otherwise send every query at once at a small connection pool.
    let lookups: Vec<_> = registered
        .agents
        .iter()
        .filter(|r| !r.per_process)
        .map(|reg| remote.status(&reg.name, Some(reg), rows.get(&reg.name)))
        .collect();
    let mut list: Vec<AgentStatus> = futures_util::stream::iter(lookups)
        .buffer_unordered(LOOKUPS)
        .collect()
        .await;
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Some(list)
}

/// The runs of an agent (or of all) as this process can see them.
pub(crate) async fn runs_view(
    app: &App,
    agent: Option<&str>,
    limit: u32,
) -> Option<Result<Vec<RunRecord>, Error>> {
    if let Some(local) = app.service::<Agents>() {
        return Some(local.runs(agent, limit).await);
    }
    let remote = remote_of(app).await?;
    Some(remote.runs(agent, limit).await)
}

/// The schedule as this process can see it.
pub(crate) async fn schedule_view(app: &App) -> Option<Vec<ScheduleInfo>> {
    if let Some(local) = app.service::<Agents>() {
        return Some(local.schedule());
    }
    let remote = remote_of(app).await?;
    let registered = app.service::<Registered>()?;
    Some(registered.schedule_infos(system_ms(), remote.locks.is_some()))
}

/// Carry `action` out for agent `name`: here when this process runs it, else through the process holding it.
pub(crate) async fn control(app: &App, name: &str, action: &str) -> Control {
    if !ACTIONS.contains(&action) {
        return Control::UnknownAction;
    }
    let local = app.service::<Agents>();
    let found = remote_of(app).await;
    let remote = found.clone().filter(|r| r.locks.is_some());
    if let Some(local) = &local
        && let Ok(status) = local.status(name)
        && status.state != AgentState::Standby
    {
        return Control::Local(run_local(local, name, action).await);
    }
    let registered = app.service::<Registered>();
    let reg = registered
        .as_ref()
        .and_then(|r| r.agents.iter().find(|a| a.name == name).cloned());
    let remote_agent = match &local {
        Some(local) => local
            .status(name)
            .is_ok_and(|s| s.state == AgentState::Standby),
        None => reg.as_ref().is_some_and(|r| !r.per_process),
    };
    if let Some(remote) = remote
        && remote_agent
    {
        return remote.command(name, action, reg.as_ref()).await;
    }
    match local {
        Some(local) => Control::Local(run_local(&local, name, action).await),
        None if reg.is_none() && registered.is_some() && found.is_some() => {
            Control::Local(Err(Error::UnknownAgent {
                name: name.to_owned(),
            }))
        }
        None => Control::NotRunning,
    }
}

async fn run_local(agents: &Agents, name: &str, action: &str) -> Result<AgentStatus, Error> {
    match action {
        "start" => agents.start(name).await,
        "stop" => agents.stop(name).await,
        "pause" => agents.pause(name).await,
        "resume" => agents.resume(name).await,
        _ => agents.restart(name).await,
    }
}

/// Carry out the commands for the agents this process holds, once a second, until shutdown. Owned by Watchfire's
/// task set. The commands of one agent run one after another, in order; those of different agents run side by side
/// (at most [`COMMAND_AGENTS`] agents at once), so one slow stop does not hold the others back. Each command is taken
/// with a compare-and-set update before it runs, so it runs at most once among the processes, and its outcome is
/// recorded as soon as it is known: the poller's own database calls run alongside, never in front of it. At shutdown
/// the commands taken here and not finished are closed at once, as cut short by the shutdown.
pub(crate) async fn poll_commands(shared: Arc<Shared>, store: DbStore) {
    let agents = Agents::new(Arc::clone(&shared));
    let shutdown = shared.shutdown.clone();
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut polls = 0_u32;
    // The commands taken here whose outcome is not recorded yet.
    let open: std::sync::Mutex<std::collections::BTreeSet<i64>> = std::sync::Mutex::default();
    // The agents whose commands are being carried out, and that work: polled here, inside this task (dropped with
    // it at shutdown).
    let mut busy: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut working = futures_util::stream::FuturesUnordered::new();
    'poll: loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            Some(agent) = working.next(), if !working.is_empty() => {
                busy.remove(&agent);
                continue;
            }
            _ = tick.tick() => {}
        }
        polls = polls.wrapping_add(1);
        let held = shared.held();
        if held.is_empty() && polls % 30 != 1 {
            continue;
        }
        let free: Vec<String> = held.into_iter().filter(|a| !busy.contains(a)).collect();
        let wanted = (!free.is_empty() && busy.len() < COMMAND_AGENTS).then_some(free);
        // The poll's own database calls, raced against the commands under way so their outcomes are not held back.
        let poll = poll_once(&shared, &store, polls % 30 == 1, wanted);
        tokio::pin!(poll);
        let found = loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break 'poll,
                Some(agent) = working.next(), if !working.is_empty() => {
                    busy.remove(&agent);
                }
                found = &mut poll => break found,
            }
        };
        let Some((now, pending)) = found else {
            continue;
        };
        // Per agent, oldest first.
        let mut batches: Vec<(String, Vec<(i64, String)>)> = Vec::new();
        for (id, agent, action) in pending {
            match batches.iter_mut().find(|(name, _)| *name == agent) {
                Some((_, commands)) => commands.push((id, action)),
                None => batches.push((agent, vec![(id, action)])),
            }
        }
        for (agent, commands) in batches {
            if busy.len() >= COMMAND_AGENTS {
                break;
            }
            busy.insert(agent.clone());
            working.push(carry_out(
                &shared, &store, &agents, &open, agent, commands, now,
            ));
        }
    }
    // A command whose outcome is being written as the shutdown starts gets a moment to record its true outcome.
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while working.next().await.is_some() {}
    })
    .await;
    drop(working);
    let unfinished: Vec<i64> = std::mem::take(&mut *open.lock().unwrap_or_else(|e| e.into_inner()))
        .into_iter()
        .collect();
    // Otherwise they would close only after the grace, as "outcome never recorded".
    close_unfinished(&store, &shared.process, &unfinished).await;
}

/// The most the shutdown close of unfinished commands may take: it runs inside the app's shutdown budget.
const CLOSE_DEADLINE: Duration = Duration::from_secs(2);

/// Close the commands among `ids` that `owner` took and did not finish, in one statement within
/// [`CLOSE_DEADLINE`] (a database that does not answer only loses that). `true` when the statement ran.
pub(crate) async fn close_unfinished(store: &DbStore, owner: &str, ids: &[i64]) -> bool {
    if ids.is_empty() {
        return true;
    }
    let close = async {
        let now = tokio::time::timeout(Duration::from_millis(500), store.db_now())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_else(system_ms);
        store.commands_close(ids, owner, 503, SHUT_DOWN, now).await
    };
    match tokio::time::timeout(CLOSE_DEADLINE, close).await {
        Ok(Ok(_)) => true,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "cannot record the outcome of commands cut short by the shutdown");
            false
        }
        Err(_) => {
            tracing::warn!(
                "the database did not answer within {CLOSE_DEADLINE:?}; commands cut short by the shutdown lapse later"
            );
            false
        }
    }
}

/// How long a shutting-down poller lets the commands under way finish (their outcome writes, mostly).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// The outcome of a command this process took but had not finished when it shut down.
pub(crate) const SHUT_DOWN: &str =
    "the process carrying it out shut down before it finished; the outcome is unknown";

/// One poll's database work: the database clock, the clean-up every 30th poll, and the commands waiting for
/// `agents` (none asked when `None`). `None` when the database fails.
async fn poll_once(
    shared: &Shared,
    store: &DbStore,
    tidy: bool,
    agents: Option<Vec<String>>,
) -> Option<(i64, Vec<(i64, String, String)>)> {
    let now = match shared.timed("db_now", store.db_now()).await {
        Ok(now) => now,
        Err(e) => {
            tracing::debug!(error = %e, "cannot read the database clock");
            return None;
        }
    };
    if tidy && let Err(e) = shared.timed("commands_lapse", lapse(store, now)).await {
        tracing::debug!(error = %e, "cannot tidy the commands table");
    }
    let agents = agents?;
    let ttl = crate::time::duration_ms(COMMAND_TTL);
    match shared
        .timed(
            "commands_pending",
            store.commands_pending(&agents, now - ttl),
        )
        .await
    {
        Ok(pending) => Some((now, pending)),
        Err(e) => {
            tracing::debug!(error = %e, "cannot read the commands table");
            None
        }
    }
}

/// Take and carry out `commands` (id, action) for `agent`, in order, recording each outcome as it is known; returns
/// the agent's name. `open` holds the ids taken and not finished.
async fn carry_out(
    shared: &Shared,
    store: &DbStore,
    agents: &Agents,
    open: &std::sync::Mutex<std::collections::BTreeSet<i64>>,
    agent: String,
    commands: Vec<(i64, String)>,
    now: i64,
) -> String {
    for (id, action) in commands {
        if shared.shutdown.is_cancelled() {
            break;
        }
        // Open before the take: a take still in flight when the shutdown cuts it off is closed with the rest (the
        // close only touches rows this process took).
        open.lock().unwrap_or_else(|e| e.into_inner()).insert(id);
        match shared
            .timed("command_take", store.command_take(id, &shared.process))
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                open.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                continue;
            }
            Err(e) => {
                open.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                tracing::debug!(error = %e, "cannot take a command");
                continue;
            }
        }
        let (ok, code, result) = if action == LOGS {
            match agents.logs(&agent) {
                Ok(lines) => (true, None, bounded_logs(&lines, LOGS_MAX_BYTES)),
                Err(e) => (
                    false,
                    Some(crate::web::api::status_of(&e).as_u16()),
                    e.to_string(),
                ),
            }
        } else if ACTIONS.contains(&action.as_str()) {
            match run_local(agents, &agent, &action).await {
                Ok(status) => (true, None, status.state.as_str().to_owned()),
                Err(e) => (
                    false,
                    Some(crate::web::api::status_of(&e).as_u16()),
                    e.to_string(),
                ),
            }
        } else {
            (false, Some(404), format!("unknown action `{action}`"))
        };
        tracing::info!(agent = %agent, action = %action, ok, "command from another process");
        let done = shared.timed("db_now", store.db_now()).await.unwrap_or(now);
        if let Err(e) = shared
            .timed(
                "command_finish",
                store.command_finish(id, ok, code, &result, done),
            )
            .await
        {
            tracing::warn!(error = %e, "cannot record a command's outcome");
        }
        open.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    }
    agent
}
