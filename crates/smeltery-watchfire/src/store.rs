//! Persistence of the agent registry, run history and checkpoints: in memory, or in the app's
//! database (the `watchfire_*` tables, see [`crate::migrations`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use smeltery_core::BoxFuture;
use smeltery_core::db::Db;
use smeltery_core::db::prelude::sea_orm::sea_query::{Alias, Expr, ExprTrait, Order, Query};
use smeltery_core::db::prelude::sea_orm::{ConnectionTrait, QueryResult, Value};

use crate::error::StoreError;
use crate::status::{AgentStatus, RunOutcome, RunRecord};

pub(crate) const AGENTS: &str = "watchfire_agents";
pub(crate) const RUNS: &str = "watchfire_runs";
pub(crate) const CHECKPOINTS: &str = "watchfire_checkpoints";
/// Commands for agents that run in another process (see `crate::remote`).
pub(crate) const COMMANDS: &str = "watchfire_commands";

/// What survives a restart of the process about an agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StoredAgent {
    pub(crate) restarts: u64,
    pub(crate) runs: u64,
}

type StoreResult<T> = Result<T, StoreError>;

/// The persistence seam. Implemented by [`MemoryStore`] and [`DbStore`]; tests add failing
/// ones.
pub(crate) trait Store: Send + Sync + 'static {
    fn upsert_agent<'a>(&'a self, status: &'a AgentStatus) -> BoxFuture<'a, StoreResult<()>>;
    fn load_agent<'a>(&'a self, name: &'a str) -> BoxFuture<'a, StoreResult<Option<StoredAgent>>>;
    fn upsert_run<'a>(&'a self, run: &'a RunRecord) -> BoxFuture<'a, StoreResult<()>>;
    /// Newest first; all agents when `agent` is `None`.
    fn recent_runs<'a>(
        &'a self,
        agent: Option<&'a str>,
        limit: u32,
    ) -> BoxFuture<'a, StoreResult<Vec<RunRecord>>>;
    /// Mark the agent's runs still recorded as running `interrupted` (in any process); returns their ids.
    fn mark_interrupted<'a>(
        &'a self,
        agent: &'a str,
        at_ms: i64,
    ) -> BoxFuture<'a, StoreResult<Vec<u64>>>;
    /// The processes (other than `except`) with runs still recorded as running.
    fn running_processes<'a>(&'a self, except: &'a str) -> BoxFuture<'a, StoreResult<Vec<String>>>;
    /// Mark every run of `process` still recorded as running `interrupted` (that process is gone); returns how
    /// many.
    fn mark_process_interrupted<'a>(
        &'a self,
        process: &'a str,
        at_ms: i64,
    ) -> BoxFuture<'a, StoreResult<u64>>;
    /// Record `run` (of this process) as running again, but only where a sweep marked it interrupted: a run whose
    /// final outcome was written meanwhile keeps it. `true` when the row was put back.
    fn restore_run<'a>(&'a self, run: &'a RunRecord) -> BoxFuture<'a, StoreResult<bool>>;
    fn load_checkpoint<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, StoreResult<Option<String>>>;
    fn save_checkpoint<'a>(
        &'a self,
        agent: &'a str,
        data: &'a str,
        at_ms: i64,
    ) -> BoxFuture<'a, StoreResult<()>>;
}

/// An in-process store: nothing survives the process.
#[derive(Clone, Debug, Default)]
pub(crate) struct MemoryStore {
    inner: Arc<Mutex<MemoryInner>>,
}

#[derive(Debug, Default)]
struct MemoryInner {
    agents: BTreeMap<String, StoredAgent>,
    /// Insertion order is the run order across agents.
    runs: Vec<RunRecord>,
    index: HashMap<(String, String, u64), usize>,
    checkpoints: BTreeMap<String, String>,
}

/// Runs kept in memory per process; the oldest go first.
const MEMORY_RUNS: usize = 10_000;

impl MemoryStore {
    fn with<T>(&self, f: impl FnOnce(&mut MemoryInner) -> T) -> T {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }
}

impl Store for MemoryStore {
    fn upsert_agent<'a>(&'a self, status: &'a AgentStatus) -> BoxFuture<'a, StoreResult<()>> {
        self.with(|m| {
            m.agents.insert(
                status.name.clone(),
                StoredAgent {
                    restarts: status.restarts,
                    runs: status.runs,
                },
            )
        });
        Box::pin(async { Ok(()) })
    }

    fn load_agent<'a>(&'a self, name: &'a str) -> BoxFuture<'a, StoreResult<Option<StoredAgent>>> {
        let found = self.with(|m| m.agents.get(name).cloned());
        Box::pin(async { Ok(found) })
    }

    fn upsert_run<'a>(&'a self, run: &'a RunRecord) -> BoxFuture<'a, StoreResult<()>> {
        self.with(|m| {
            let key = (run.agent.clone(), run.process.clone(), run.run_id);
            if let Some(slot) = m.index.get(&key).and_then(|i| m.runs.get_mut(*i)) {
                *slot = run.clone();
                return;
            }
            if m.runs.len() >= MEMORY_RUNS {
                let drop = MEMORY_RUNS / 10;
                m.runs.drain(..drop);
                m.index = m
                    .runs
                    .iter()
                    .enumerate()
                    .map(|(i, r)| ((r.agent.clone(), r.process.clone(), r.run_id), i))
                    .collect();
            }
            m.index.insert(key, m.runs.len());
            m.runs.push(run.clone());
        });
        Box::pin(async { Ok(()) })
    }

    fn recent_runs<'a>(
        &'a self,
        agent: Option<&'a str>,
        limit: u32,
    ) -> BoxFuture<'a, StoreResult<Vec<RunRecord>>> {
        let runs = self.with(|m| {
            m.runs
                .iter()
                .rev()
                .filter(|r| agent.is_none_or(|a| r.agent == a))
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .cloned()
                .collect()
        });
        Box::pin(async { Ok(runs) })
    }

    fn mark_interrupted<'a>(
        &'a self,
        agent: &'a str,
        _at_ms: i64,
    ) -> BoxFuture<'a, StoreResult<Vec<u64>>> {
        let ids = self.with(|m| {
            let mut ids = Vec::new();
            for run in m.runs.iter_mut() {
                if run.agent == agent && run.outcome == RunOutcome::Running {
                    run.outcome = RunOutcome::Interrupted;
                    run.error = Some(INTERRUPTED.to_owned());
                    ids.push(run.run_id);
                }
            }
            ids
        });
        Box::pin(async { Ok(ids) })
    }

    fn running_processes<'a>(&'a self, except: &'a str) -> BoxFuture<'a, StoreResult<Vec<String>>> {
        let found = self.with(|m| {
            let mut found: Vec<String> = m
                .runs
                .iter()
                .filter(|r| {
                    r.outcome == RunOutcome::Running && !r.process.is_empty() && r.process != except
                })
                .map(|r| r.process.clone())
                .collect();
            found.sort();
            found.dedup();
            found
        });
        Box::pin(async { Ok(found) })
    }

    fn mark_process_interrupted<'a>(
        &'a self,
        process: &'a str,
        _at_ms: i64,
    ) -> BoxFuture<'a, StoreResult<u64>> {
        let count = self.with(|m| {
            let mut count = 0;
            for run in m.runs.iter_mut() {
                if run.process == process && run.outcome == RunOutcome::Running {
                    run.outcome = RunOutcome::Interrupted;
                    run.error = Some(INTERRUPTED.to_owned());
                    count += 1;
                }
            }
            count
        });
        Box::pin(async move { Ok(count) })
    }

    fn restore_run<'a>(&'a self, run: &'a RunRecord) -> BoxFuture<'a, StoreResult<bool>> {
        let restored = self.with(|m| {
            let key = (run.agent.clone(), run.process.clone(), run.run_id);
            match m.index.get(&key).and_then(|i| m.runs.get_mut(*i)) {
                Some(slot)
                    if slot.outcome == RunOutcome::Interrupted
                        && slot.error.as_deref() == Some(INTERRUPTED) =>
                {
                    *slot = run.clone();
                    slot.outcome = RunOutcome::Running;
                    slot.ended_at_ms = None;
                    slot.error = None;
                    true
                }
                _ => false,
            }
        });
        Box::pin(async move { Ok(restored) })
    }

    fn load_checkpoint<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, StoreResult<Option<String>>> {
        let found = self.with(|m| m.checkpoints.get(agent).cloned());
        Box::pin(async { Ok(found) })
    }

    fn save_checkpoint<'a>(
        &'a self,
        agent: &'a str,
        data: &'a str,
        _at_ms: i64,
    ) -> BoxFuture<'a, StoreResult<()>> {
        self.with(|m| m.checkpoints.insert(agent.to_owned(), data.to_owned()));
        Box::pin(async { Ok(()) })
    }
}

/// The error of a run whose process ended during it.
const INTERRUPTED: &str = "the process ended during the run";

/// The store over the app's database.
#[derive(Clone, Debug)]
pub(crate) struct DbStore {
    db: Db,
    /// The schema has the `process` column and the commands table (`migrations::up`, or
    /// `migrations::up_multi_process` for apps migrated before).
    processes: bool,
    /// Tests: `commands_pending` sleeps this long first (a slow database under the command poller).
    #[cfg(test)]
    pub(crate) pending_delay_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Tests: `command_finish` and `commands_close` sleep this long first.
    #[cfg(test)]
    pub(crate) finish_delay_ms: Arc<std::sync::atomic::AtomicU64>,
}

/// A command row: who took it, and its outcome once recorded.
#[derive(Clone, Debug, Default)]
pub(crate) struct CommandState {
    pub(crate) taken_by: Option<String>,
    pub(crate) outcome: Option<CommandOutcome>,
}

/// A recorded command outcome.
#[derive(Clone, Debug, Default)]
pub(crate) struct CommandOutcome {
    pub(crate) ok: bool,
    /// The HTTP status of a refusal (404 unknown agent, 409 wrong state, 503 shutting down, 504 lapsed untaken).
    pub(crate) code: Option<u16>,
    pub(crate) result: String,
}

/// An agent's row in `watchfire_agents`, as another process wrote it.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentRowData {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) restarts: u64,
    pub(crate) runs: u64,
    pub(crate) started_at_ms: Option<i64>,
    pub(crate) last_heartbeat_ms: Option<i64>,
    pub(crate) last_error: Option<String>,
    pub(crate) updated_at_ms: i64,
}

fn col(name: &str) -> Alias {
    Alias::new(name)
}

fn i64_of(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn u64_of(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

impl DbStore {
    /// The store, checking which version of the tables the database has.
    pub(crate) async fn open(db: Db) -> Self {
        let processes = Self::has_processes(&db).await;
        if !processes {
            tracing::warn!(
                "the Watchfire tables predate several-process support: add a migration that calls \
                 `smeltery::watchfire::migrations::up_multi_process` (run history is kept without process names, \
                 and commands cannot reach agents in other processes)"
            );
        }
        Self {
            db,
            processes,
            #[cfg(test)]
            pending_delay_ms: Arc::default(),
            #[cfg(test)]
            finish_delay_ms: Arc::default(),
        }
    }

    /// Whether the database has the several-process schema (the commands table).
    pub(crate) async fn has_processes(db: &Db) -> bool {
        smeltery_core::db::migration::Schema::new(db)
            .has_table(COMMANDS)
            .await
            .unwrap_or(false)
    }

    /// The commands table and the `process` column exist.
    #[cfg(test)]
    pub(crate) fn processes(&self) -> bool {
        self.processes
    }

    /// Queue a command for `agent` (`requested_at` = `now`): its id. At most `limit` commands wait per agent.
    pub(crate) async fn command_request(
        &self,
        agent: &str,
        action: &str,
        now: i64,
        limit: u64,
    ) -> StoreResult<Result<i64, String>> {
        let count = Query::select()
            .expr(Expr::cust("COUNT(*) AS n"))
            .from(col(COMMANDS))
            .and_where(Expr::col(col("agent")).eq(agent))
            .and_where(Expr::col(col("done_at")).is_null())
            .to_owned();
        let rows = self.query("command_request", &count).await?;
        let waiting = rows
            .first()
            .and_then(|r| r.try_get::<i64>("", "n").ok())
            .unwrap_or(0);
        if u64_of(waiting) >= limit {
            return Ok(Err(format!(
                "{waiting} commands for `{agent}` are waiting already"
            )));
        }
        let token = format!("{}-{now}", crate::policy::Jitter::from_os().next_u64());
        let mut insert = Query::insert();
        insert
            .into_table(col(COMMANDS))
            .columns([
                col("token"),
                col("agent"),
                col("action"),
                col("requested_at"),
            ])
            .values([
                Expr::val(token.clone()),
                Expr::val(agent),
                Expr::val(action),
                Expr::val(now),
            ])
            .map_err(|e| StoreError::new("command_request", e.to_string()))?;
        self.exec("command_request", &insert).await?;
        let select = Query::select()
            .column(col("id"))
            .from(col(COMMANDS))
            .and_where(Expr::col(col("token")).eq(token))
            .to_owned();
        let rows = self.query("command_request", &select).await?;
        rows.first()
            .and_then(|r| r.try_get::<i64>("", "id").ok())
            .map(Ok)
            .ok_or_else(|| StoreError::new("command_request", "the new command row is missing"))
    }

    /// The state of command `id`: who took it, and its outcome once recorded.
    pub(crate) async fn command_state(&self, id: i64) -> StoreResult<CommandState> {
        let select = Query::select()
            .columns([
                col("taken_by"),
                col("done_at"),
                col("ok"),
                col("code"),
                col("result"),
            ])
            .from(col(COMMANDS))
            .and_where(Expr::col(col("id")).eq(id))
            .to_owned();
        let rows = self.query("command_state", &select).await?;
        let Some(r) = rows.first() else {
            return Err(StoreError::new("command_state", "the command row is gone"));
        };
        let done: Option<i64> = r.try_get("", "done_at").ok().flatten();
        Ok(CommandState {
            taken_by: r.try_get("", "taken_by").ok().flatten(),
            outcome: done.map(|_| CommandOutcome {
                ok: r
                    .try_get::<Option<bool>>("", "ok")
                    .ok()
                    .flatten()
                    .unwrap_or(false),
                code: r
                    .try_get::<Option<i32>>("", "code")
                    .ok()
                    .flatten()
                    .and_then(|c| u16::try_from(c).ok()),
                result: r
                    .try_get::<Option<String>>("", "result")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
            }),
        })
    }

    /// The database's own clock, Unix milliseconds: every process compares command times against one clock.
    pub(crate) async fn db_now(&self) -> StoreResult<i64> {
        use smeltery_core::db::prelude::sea_orm::{DatabaseBackend, Statement};
        let conn = self.db.conn();
        let backend = conn.get_database_backend();
        let sql = match backend {
            DatabaseBackend::Postgres => {
                "SELECT CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT) AS n"
            }
            DatabaseBackend::MySql => {
                // UTC_TIMESTAMP is UTC whatever the session time zone (UNIX_TIMESTAMP(NOW()) converts a local time back
                // through it, ambiguous in a daylight-saving fall-back hour); the literal epoch is a plain DATETIME.
                "SELECT CAST(TIMESTAMPDIFF(MICROSECOND, '1970-01-01 00:00:00', UTC_TIMESTAMP(6)) DIV 1000 AS SIGNED) AS n"
            }
            _ => "SELECT CAST(ROUND((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER) AS n",
        };
        let row = conn
            .query_one_raw(Statement::from_string(backend, sql))
            .await
            .map_err(|e| StoreError::new("db_now", e))?
            .ok_or_else(|| StoreError::new("db_now", "no row"))?;
        row.try_get::<i64>("", "n")
            .map_err(|e| StoreError::new("db_now", e))
    }

    /// Commands waiting for `agents` (requested after `since`): `(id, agent, action)`, oldest first.
    pub(crate) async fn commands_pending(
        &self,
        agents: &[String],
        since: i64,
    ) -> StoreResult<Vec<(i64, String, String)>> {
        if agents.is_empty() {
            return Ok(Vec::new());
        }
        #[cfg(test)]
        {
            let delay = self
                .pending_delay_ms
                .load(std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        let select = Query::select()
            .columns([col("id"), col("agent"), col("action")])
            .from(col(COMMANDS))
            .and_where(Expr::col(col("agent")).is_in(agents.iter().cloned()))
            .and_where(Expr::col(col("done_at")).is_null())
            .and_where(Expr::col(col("taken_by")).is_null())
            .and_where(Expr::col(col("requested_at")).gt(since))
            .order_by(col("id"), Order::Asc)
            .limit(20)
            .to_owned();
        let rows = self.query("commands_pending", &select).await?;
        let err = |e| StoreError::new("commands_pending", e);
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("", "id").map_err(err)?,
                    r.try_get("", "agent").map_err(err)?,
                    r.try_get("", "action").map_err(err)?,
                ))
            })
            .collect()
    }

    /// Take command `id` for `owner`; `false` when another process took it first.
    pub(crate) async fn command_take(&self, id: i64, owner: &str) -> StoreResult<bool> {
        let update = Query::update()
            .table(col(COMMANDS))
            .values([(col("taken_by"), Expr::val(owner))])
            .and_where(Expr::col(col("id")).eq(id))
            .and_where(Expr::col(col("taken_by")).is_null())
            .and_where(Expr::col(col("done_at")).is_null())
            .to_owned();
        Ok(self.exec("command_take", &update).await? > 0)
    }

    /// Record command `id`'s outcome (`code`: the HTTP status of a refusal; unless it lapsed meanwhile).
    pub(crate) async fn command_finish(
        &self,
        id: i64,
        ok: bool,
        code: Option<u16>,
        result: &str,
        now: i64,
    ) -> StoreResult<()> {
        #[cfg(test)]
        {
            let delay = self
                .finish_delay_ms
                .load(std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        let update = Query::update()
            .table(col(COMMANDS))
            .values([
                (col("done_at"), Expr::val(now)),
                (col("ok"), Expr::val(ok)),
                (col("code"), Expr::val(code.map(i32::from))),
                (col("result"), Expr::val(result)),
            ])
            .and_where(Expr::col(col("id")).eq(id))
            .and_where(Expr::col(col("done_at")).is_null())
            .to_owned();
        self.exec("command_finish", &update).await.map(|_| ())
    }

    /// Close, in one statement, the commands among `ids` that `owner` took and that are not finished yet: their
    /// outcome is `code` / `result` (a process shutting down).
    pub(crate) async fn commands_close(
        &self,
        ids: &[i64],
        owner: &str,
        code: u16,
        result: &str,
        now: i64,
    ) -> StoreResult<u64> {
        #[cfg(test)]
        {
            let delay = self
                .finish_delay_ms
                .load(std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        if ids.is_empty() {
            return Ok(0);
        }
        let update = Query::update()
            .table(col(COMMANDS))
            .values([
                (col("done_at"), Expr::val(now)),
                (col("ok"), Expr::val(false)),
                (col("code"), Expr::val(i32::from(code))),
                (col("result"), Expr::val(result)),
            ])
            .and_where(Expr::col(col("id")).is_in(ids.iter().copied()))
            .and_where(Expr::col(col("taken_by")).eq(owner))
            .and_where(Expr::col(col("done_at")).is_null())
            .to_owned();
        self.exec("commands_close", &update).await
    }

    /// Delete command `id` if its action is `action` (a `logs` answer, once its requester has read it).
    pub(crate) async fn command_delete(&self, id: i64, action: &str) -> StoreResult<()> {
        let delete = Query::delete()
            .from_table(col(COMMANDS))
            .and_where(Expr::col(col("id")).eq(id))
            .and_where(Expr::col(col("action")).eq(action))
            .to_owned();
        self.exec("command_delete", &delete).await.map(|_| ())
    }

    /// How many command rows have `action` (tests).
    #[cfg(test)]
    pub(crate) async fn commands_with_action(&self, action: &str) -> i64 {
        let count = Query::select()
            .expr(Expr::cust("COUNT(*) AS n"))
            .from(col(COMMANDS))
            .and_where(Expr::col(col("action")).eq(action))
            .to_owned();
        self.query("commands_with_action", &count)
            .await
            .ok()
            .and_then(|rows| rows.first().and_then(|r| r.try_get::<i64>("", "n").ok()))
            .unwrap_or(-1)
    }

    /// Close the commands nobody took that were requested at or before `untaken_before`, and those taken but not
    /// finished requested at or before `taken_before` (their outcome is unknown: the taker may have carried them
    /// out); delete finished ones older than `forget_before`, and finished `forget_action` rows (log lines) older
    /// than `forget_action_before`.
    pub(crate) async fn commands_lapse(
        &self,
        untaken_before: i64,
        taken_before: i64,
        forget_before: i64,
        (forget_action, forget_action_before): (&str, i64),
        now: i64,
    ) -> StoreResult<()> {
        let untaken = Query::update()
            .table(col(COMMANDS))
            .values([
                (col("done_at"), Expr::val(now)),
                (col("ok"), Expr::val(false)),
                (col("code"), Expr::val(504)),
                (
                    col("result"),
                    Expr::val("lapsed: no process holding the agent took it in time"),
                ),
            ])
            .and_where(Expr::col(col("done_at")).is_null())
            .and_where(Expr::col(col("taken_by")).is_null())
            .and_where(Expr::col(col("requested_at")).lte(untaken_before))
            .to_owned();
        self.exec("commands_lapse", &untaken).await?;
        let taken = Query::update()
            .table(col(COMMANDS))
            .values([
                (col("done_at"), Expr::val(now)),
                (col("ok"), Expr::val(false)),
                (
                    col("result"),
                    Expr::val("taken, but its outcome was never recorded (the process may have carried it out)"),
                ),
            ])
            .and_where(Expr::col(col("done_at")).is_null())
            .and_where(Expr::col(col("taken_by")).is_not_null())
            .and_where(Expr::col(col("requested_at")).lte(taken_before))
            .to_owned();
        self.exec("commands_lapse", &taken).await?;
        let delete = Query::delete()
            .from_table(col(COMMANDS))
            .and_where(Expr::col(col("action")).eq(forget_action))
            .and_where(Expr::col(col("done_at")).lt(forget_action_before))
            .to_owned();
        self.exec("commands_lapse", &delete).await?;
        let delete = Query::delete()
            .from_table(col(COMMANDS))
            .and_where(Expr::col(col("done_at")).lt(forget_before))
            .to_owned();
        self.exec("commands_lapse", &delete).await.map(|_| ())
    }

    /// Every agent row, by name.
    pub(crate) async fn agent_rows(&self) -> StoreResult<Vec<AgentRowData>> {
        let select = Query::select()
            .columns([
                col("name"),
                col("state"),
                col("restarts"),
                col("runs"),
                col("started_at"),
                col("last_heartbeat_at"),
                col("last_error"),
                col("updated_at"),
            ])
            .from(col(AGENTS))
            .order_by(col("name"), Order::Asc)
            .to_owned();
        let rows = self.query("agent_rows", &select).await?;
        let err = |e| StoreError::new("agent_rows", e);
        rows.iter()
            .map(|row| {
                Ok(AgentRowData {
                    name: row.try_get("", "name").map_err(err)?,
                    state: row.try_get("", "state").map_err(err)?,
                    restarts: u64_of(row.try_get("", "restarts").map_err(err)?),
                    runs: u64_of(row.try_get("", "runs").map_err(err)?),
                    started_at_ms: row.try_get("", "started_at").map_err(err)?,
                    last_heartbeat_ms: row.try_get("", "last_heartbeat_at").map_err(err)?,
                    last_error: row.try_get("", "last_error").map_err(err)?,
                    updated_at_ms: row.try_get("", "updated_at").map_err(err)?,
                })
            })
            .collect()
    }

    async fn exec<S>(&self, op: &'static str, stmt: &S) -> StoreResult<u64>
    where
        S: smeltery_core::db::prelude::sea_orm::StatementBuilder,
    {
        let conn = self.db.conn();
        let stmt = conn.get_database_backend().build(stmt);
        conn.execute_raw(stmt)
            .await
            .map(|r| r.rows_affected())
            .map_err(|e| StoreError::new(op, e))
    }

    async fn query<S>(&self, op: &'static str, stmt: &S) -> StoreResult<Vec<QueryResult>>
    where
        S: smeltery_core::db::prelude::sea_orm::StatementBuilder,
    {
        let conn = self.db.conn();
        let stmt = conn.get_database_backend().build(stmt);
        conn.query_all_raw(stmt)
            .await
            .map_err(|e| StoreError::new(op, e))
    }

    /// Whether the runs table exists (the migration ran).
    pub(crate) async fn ready(db: &Db) -> bool {
        smeltery_core::db::migration::Schema::new(db)
            .has_table(RUNS)
            .await
            .unwrap_or(false)
    }

    fn run_from_row(row: &QueryResult) -> Result<RunRecord, StoreError> {
        let err = |e| StoreError::new("read_run", e);
        let counters: Option<String> = row.try_get("", "counters").map_err(err)?;
        let outcome: String = row.try_get("", "outcome").map_err(err)?;
        Ok(RunRecord {
            agent: row.try_get("", "agent").map_err(err)?,
            run_id: u64_of(row.try_get("", "run_id").map_err(err)?),
            job: row.try_get("", "job").map_err(err)?,
            started_at_ms: row.try_get("", "started_at").map_err(err)?,
            ended_at_ms: row.try_get("", "ended_at").map_err(err)?,
            outcome: RunOutcome::parse(&outcome).unwrap_or(RunOutcome::Interrupted),
            error: row.try_get("", "error").map_err(err)?,
            counters: counters
                .and_then(|c| serde_json::from_str(&c).ok())
                .unwrap_or_default(),
            process: row
                .try_get::<Option<String>>("", "process")
                .ok()
                .flatten()
                .unwrap_or_default(),
        })
    }
}

impl Store for DbStore {
    fn upsert_agent<'a>(&'a self, status: &'a AgentStatus) -> BoxFuture<'a, StoreResult<()>> {
        Box::pin(async move {
            let values: Vec<(Alias, Value)> = vec![
                (col("state"), status.state.as_str().into()),
                (col("restarts"), i64_of(status.restarts).into()),
                (col("runs"), i64_of(status.runs).into()),
                (col("started_at"), status.started_at_ms.into()),
                (col("last_heartbeat_at"), status.last_heartbeat_ms.into()),
                (col("last_error"), status.last_error.clone().into()),
                (col("updated_at"), status.updated_at_ms.into()),
            ];
            let update = Query::update()
                .table(col(AGENTS))
                .values(
                    values
                        .iter()
                        .map(|(c, v)| (c.clone(), Expr::val(v.clone()))),
                )
                .and_where(Expr::col(col("name")).eq(status.name.clone()))
                .to_owned();
            if self.exec("upsert_agent", &update).await? > 0 {
                return Ok(());
            }
            let mut insert = Query::insert();
            insert
                .into_table(col(AGENTS))
                .columns(std::iter::once(col("name")).chain(values.iter().map(|(c, _)| c.clone())))
                .values(
                    std::iter::once(Expr::val(status.name.clone()))
                        .chain(values.into_iter().map(|(_, v)| Expr::val(v))),
                )
                .map_err(|e| StoreError::new("upsert_agent", e.to_string()))?;
            self.exec("upsert_agent", &insert).await.map(|_| ())
        })
    }

    fn load_agent<'a>(&'a self, name: &'a str) -> BoxFuture<'a, StoreResult<Option<StoredAgent>>> {
        Box::pin(async move {
            let select = Query::select()
                .columns([col("restarts"), col("runs")])
                .from(col(AGENTS))
                .and_where(Expr::col(col("name")).eq(name))
                .to_owned();
            let rows = self.query("load_agent", &select).await?;
            let Some(row) = rows.first() else {
                return Ok(None);
            };
            let err = |e| StoreError::new("load_agent", e);
            Ok(Some(StoredAgent {
                restarts: u64_of(row.try_get("", "restarts").map_err(err)?),
                runs: u64_of(row.try_get("", "runs").map_err(err)?),
            }))
        })
    }

    fn upsert_run<'a>(&'a self, run: &'a RunRecord) -> BoxFuture<'a, StoreResult<()>> {
        Box::pin(async move {
            let counters = serde_json::to_string(&run.counters)
                .map_err(|e| StoreError::new("upsert_run", e))?;
            let values: Vec<(Alias, Value)> = vec![
                (col("job"), run.job.clone().into()),
                (col("started_at"), run.started_at_ms.into()),
                (col("ended_at"), run.ended_at_ms.into()),
                (col("outcome"), run.outcome.as_str().into()),
                (col("error"), run.error.clone().into()),
                (col("counters"), counters.into()),
            ];
            let mut update = Query::update();
            update
                .table(col(RUNS))
                .values(
                    values
                        .iter()
                        .map(|(c, v)| (c.clone(), Expr::val(v.clone()))),
                )
                .and_where(Expr::col(col("agent")).eq(run.agent.clone()))
                .and_where(Expr::col(col("run_id")).eq(i64_of(run.run_id)));
            // Run ids count per process for agents that run in every process: the process is part of the key.
            if self.processes {
                update.and_where(Expr::col(col("process")).eq(run.process.clone()));
            }
            if self.exec("upsert_run", &update).await? > 0 {
                return Ok(());
            }
            let mut keys = vec![
                (col("agent"), Value::from(run.agent.clone())),
                (col("run_id"), Value::from(i64_of(run.run_id))),
            ];
            if self.processes {
                keys.push((col("process"), Value::from(run.process.clone())));
            }
            let mut insert = Query::insert();
            insert
                .into_table(col(RUNS))
                .columns(
                    keys.iter()
                        .map(|(c, _)| c.clone())
                        .chain(values.iter().map(|(c, _)| c.clone())),
                )
                .values(
                    keys.into_iter()
                        .map(|(_, v)| Expr::val(v))
                        .chain(values.into_iter().map(|(_, v)| Expr::val(v))),
                )
                .map_err(|e| StoreError::new("upsert_run", e.to_string()))?;
            self.exec("upsert_run", &insert).await.map(|_| ())
        })
    }

    fn recent_runs<'a>(
        &'a self,
        agent: Option<&'a str>,
        limit: u32,
    ) -> BoxFuture<'a, StoreResult<Vec<RunRecord>>> {
        Box::pin(async move {
            let mut select = Query::select();
            select
                .columns([
                    col("agent"),
                    col("run_id"),
                    col("job"),
                    col("started_at"),
                    col("ended_at"),
                    col("outcome"),
                    col("error"),
                    col("counters"),
                ])
                .from(col(RUNS));
            if self.processes {
                select.column(col("process"));
            }
            select
                .order_by(col("started_at"), Order::Desc)
                .order_by(col("id"), Order::Desc)
                .limit(u64::from(limit));
            if let Some(agent) = agent {
                select.and_where(Expr::col(col("agent")).eq(agent));
            }
            let rows = self.query("recent_runs", &select).await?;
            rows.iter().map(Self::run_from_row).collect()
        })
    }

    fn mark_interrupted<'a>(
        &'a self,
        agent: &'a str,
        at_ms: i64,
    ) -> BoxFuture<'a, StoreResult<Vec<u64>>> {
        Box::pin(async move {
            let select = Query::select()
                .column(col("run_id"))
                .from(col(RUNS))
                .and_where(Expr::col(col("agent")).eq(agent))
                .and_where(Expr::col(col("outcome")).eq(RunOutcome::Running.as_str()))
                .to_owned();
            let rows = self.query("mark_interrupted", &select).await?;
            let ids = rows
                .iter()
                .filter_map(|r| r.try_get::<i64>("", "run_id").ok())
                .map(u64_of)
                .collect::<Vec<_>>();
            if !ids.is_empty() {
                let update = Query::update()
                    .table(col(RUNS))
                    .values([
                        (col("outcome"), Expr::val(RunOutcome::Interrupted.as_str())),
                        (col("error"), Expr::val(INTERRUPTED)),
                    ])
                    .and_where(Expr::col(col("agent")).eq(agent))
                    .and_where(Expr::col(col("outcome")).eq(RunOutcome::Running.as_str()))
                    .to_owned();
                self.exec("mark_interrupted", &update).await?;
                tracing::warn!(agent, runs = ?ids, at_ms, "marked runs interrupted");
            }
            Ok(ids)
        })
    }

    fn running_processes<'a>(&'a self, except: &'a str) -> BoxFuture<'a, StoreResult<Vec<String>>> {
        Box::pin(async move {
            if !self.processes {
                return Ok(Vec::new());
            }
            let select = Query::select()
                .distinct()
                .column(col("process"))
                .from(col(RUNS))
                .and_where(Expr::col(col("outcome")).eq(RunOutcome::Running.as_str()))
                .and_where(Expr::col(col("process")).ne(except))
                .and_where(Expr::col(col("process")).ne(""))
                .to_owned();
            let rows = self.query("running_processes", &select).await?;
            Ok(rows
                .iter()
                .filter_map(|r| r.try_get::<String>("", "process").ok())
                .collect())
        })
    }

    fn mark_process_interrupted<'a>(
        &'a self,
        process: &'a str,
        at_ms: i64,
    ) -> BoxFuture<'a, StoreResult<u64>> {
        Box::pin(async move {
            if !self.processes {
                return Ok(0);
            }
            let update = Query::update()
                .table(col(RUNS))
                .values([
                    (col("outcome"), Expr::val(RunOutcome::Interrupted.as_str())),
                    (col("error"), Expr::val(INTERRUPTED)),
                ])
                .and_where(Expr::col(col("process")).eq(process))
                .and_where(Expr::col(col("outcome")).eq(RunOutcome::Running.as_str()))
                .to_owned();
            let count = self.exec("mark_process_interrupted", &update).await?;
            if count > 0 {
                tracing::warn!(
                    process,
                    runs = count,
                    at_ms,
                    "marked the runs of an ended process interrupted"
                );
            }
            Ok(count)
        })
    }

    fn restore_run<'a>(&'a self, run: &'a RunRecord) -> BoxFuture<'a, StoreResult<bool>> {
        Box::pin(async move {
            let counters = serde_json::to_string(&run.counters)
                .map_err(|e| StoreError::new("restore_run", e))?;
            // One conditional statement: only a row a sweep marked (outcome and error both the sweep's) is put back;
            // a final outcome written in between is left alone.
            let mut update = Query::update();
            update
                .table(col(RUNS))
                .values([
                    (col("outcome"), Expr::val(RunOutcome::Running.as_str())),
                    (col("ended_at"), Expr::val(Option::<i64>::None)),
                    (col("error"), Expr::val(Option::<String>::None)),
                    (col("counters"), Expr::val(counters)),
                ])
                .and_where(Expr::col(col("agent")).eq(run.agent.clone()))
                .and_where(Expr::col(col("run_id")).eq(i64_of(run.run_id)))
                .and_where(Expr::col(col("outcome")).eq(RunOutcome::Interrupted.as_str()))
                .and_where(Expr::col(col("error")).eq(INTERRUPTED));
            if self.processes {
                update.and_where(Expr::col(col("process")).eq(run.process.clone()));
            }
            Ok(self.exec("restore_run", &update).await? > 0)
        })
    }

    fn load_checkpoint<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, StoreResult<Option<String>>> {
        Box::pin(async move {
            let select = Query::select()
                .column(col("data"))
                .from(col(CHECKPOINTS))
                .and_where(Expr::col(col("agent")).eq(agent))
                .to_owned();
            let rows = self.query("load_checkpoint", &select).await?;
            match rows.first() {
                Some(row) => row
                    .try_get("", "data")
                    .map(Some)
                    .map_err(|e| StoreError::new("load_checkpoint", e)),
                None => Ok(None),
            }
        })
    }

    fn save_checkpoint<'a>(
        &'a self,
        agent: &'a str,
        data: &'a str,
        at_ms: i64,
    ) -> BoxFuture<'a, StoreResult<()>> {
        Box::pin(async move {
            let update = Query::update()
                .table(col(CHECKPOINTS))
                .values([
                    (col("data"), Expr::val(data)),
                    (col("updated_at"), Expr::val(at_ms)),
                ])
                .and_where(Expr::col(col("agent")).eq(agent))
                .to_owned();
            if self.exec("save_checkpoint", &update).await? > 0 {
                return Ok(());
            }
            let mut insert = Query::insert();
            insert
                .into_table(col(CHECKPOINTS))
                .columns([col("agent"), col("data"), col("updated_at")])
                .values([Expr::val(agent), Expr::val(data), Expr::val(at_ms)])
                .map_err(|e| StoreError::new("save_checkpoint", e.to_string()))?;
            self.exec("save_checkpoint", &insert).await.map(|_| ())
        })
    }
}

#[cfg(test)]
pub(crate) mod failing {
    //! A store whose every operation fails, for unhappy-path tests.
    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct FailingStore;

    fn fail<'a, T: Send + 'a>(op: &'static str) -> BoxFuture<'a, StoreResult<T>> {
        Box::pin(async move { Err(StoreError::new(op, "fake failure")) })
    }

    impl Store for FailingStore {
        fn upsert_agent<'a>(&'a self, _: &'a AgentStatus) -> BoxFuture<'a, StoreResult<()>> {
            fail("upsert_agent")
        }
        fn load_agent<'a>(&'a self, _: &'a str) -> BoxFuture<'a, StoreResult<Option<StoredAgent>>> {
            fail("load_agent")
        }
        fn upsert_run<'a>(&'a self, _: &'a RunRecord) -> BoxFuture<'a, StoreResult<()>> {
            fail("upsert_run")
        }
        fn recent_runs<'a>(
            &'a self,
            _: Option<&'a str>,
            _: u32,
        ) -> BoxFuture<'a, StoreResult<Vec<RunRecord>>> {
            fail("recent_runs")
        }
        fn mark_interrupted<'a>(
            &'a self,
            _: &'a str,
            _: i64,
        ) -> BoxFuture<'a, StoreResult<Vec<u64>>> {
            fail("mark_interrupted")
        }
        fn running_processes<'a>(&'a self, _: &'a str) -> BoxFuture<'a, StoreResult<Vec<String>>> {
            fail("running_processes")
        }
        fn mark_process_interrupted<'a>(
            &'a self,
            _: &'a str,
            _: i64,
        ) -> BoxFuture<'a, StoreResult<u64>> {
            fail("mark_process_interrupted")
        }
        fn restore_run<'a>(&'a self, _: &'a RunRecord) -> BoxFuture<'a, StoreResult<bool>> {
            fail("restore_run")
        }
        fn load_checkpoint<'a>(&'a self, _: &'a str) -> BoxFuture<'a, StoreResult<Option<String>>> {
            fail("load_checkpoint")
        }
        fn save_checkpoint<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: i64,
        ) -> BoxFuture<'a, StoreResult<()>> {
            fail("save_checkpoint")
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::policy::Restart;

    pub(crate) async fn round_trip(store: &dyn Store) {
        let mut status = AgentStatus::new("a", Restart::Always, None, 0);
        status.restarts = 2;
        status.runs = 3;
        store.upsert_agent(&status).await.unwrap();
        status.runs = 4;
        store.upsert_agent(&status).await.unwrap();
        assert_eq!(
            store.load_agent("a").await.unwrap(),
            Some(StoredAgent {
                restarts: 2,
                runs: 4
            })
        );
        assert_eq!(store.load_agent("b").await.unwrap(), None);

        for id in 1..=3 {
            let mut run = RunRecord::started("a", id, None, i64::try_from(id).unwrap() * 10);
            run.counters.insert("pages".into(), 7);
            store.upsert_run(&run).await.unwrap();
        }
        let mut done = RunRecord::started("a", 1, Some("send".into()), 10);
        done.outcome = RunOutcome::Completed;
        done.ended_at_ms = Some(11);
        store.upsert_run(&done).await.unwrap();
        store
            .upsert_run(&RunRecord::started("b", 9, None, 5))
            .await
            .unwrap();
        let runs = store.recent_runs(Some("a"), 2).await.unwrap();
        let ids: Vec<u64> = runs.iter().map(|r| r.run_id).collect();
        assert_eq!(ids, [3, 2]);
        assert_eq!(runs[0].counters.get("pages"), Some(&7));
        assert_eq!(store.recent_runs(None, 10).await.unwrap().len(), 4);

        let mut interrupted = store.mark_interrupted("a", 50).await.unwrap();
        interrupted.sort_unstable();
        assert_eq!(interrupted, [2, 3]);
        let all = store.recent_runs(Some("a"), 10).await.unwrap();
        assert!(
            all.iter()
                .any(|r| r.run_id == 1 && r.outcome == RunOutcome::Completed)
        );
        assert!(
            all.iter()
                .filter(|r| r.run_id != 1)
                .all(|r| r.outcome == RunOutcome::Interrupted)
        );

        assert_eq!(store.load_checkpoint("a").await.unwrap(), None);
        store.save_checkpoint("a", "{\"n\":1}", 1).await.unwrap();
        store.save_checkpoint("a", "{\"n\":2}", 2).await.unwrap();
        assert_eq!(
            store.load_checkpoint("a").await.unwrap().as_deref(),
            Some("{\"n\":2}")
        );
    }

    #[tokio::test]
    async fn memory_store_round_trips() {
        round_trip(&MemoryStore::default()).await;
    }

    #[tokio::test]
    async fn db_store_round_trips_on_sqlite() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let schema = smeltery_core::db::migration::Schema::new(&db);
        assert!(!DbStore::ready(&db).await);
        crate::migrations::up(&schema).await.unwrap();
        assert!(DbStore::ready(&db).await);
        round_trip(&DbStore::open(db).await).await;
    }

    /// Two processes run an agent each under one name (queue workers, `per_process` agents, `schedule:run`):
    /// their run ids collide, their records do not, and only the ended process's runs are marked interrupted.
    pub(crate) async fn per_process_runs(store: &dyn Store) {
        let mut a = RunRecord::started("queue#0", 1, None, 10);
        a.process = "host:1-a".into();
        let mut b = RunRecord::started("queue#0", 1, None, 11);
        b.process = "host:2-b".into();
        store.upsert_run(&a).await.unwrap();
        store.upsert_run(&b).await.unwrap();
        a.outcome = RunOutcome::Completed;
        store.upsert_run(&a).await.unwrap();
        let runs = store.recent_runs(Some("queue#0"), 10).await.unwrap();
        assert_eq!(runs.len(), 2, "one record per process");
        let mut c = RunRecord::started("queue#0", 2, None, 12);
        c.process = "host:2-b".into();
        store.upsert_run(&c).await.unwrap();
        assert_eq!(
            store.running_processes("host:9-z").await.unwrap(),
            ["host:2-b"]
        );
        assert!(
            store
                .running_processes("host:2-b")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .mark_process_interrupted("host:2-b", 20)
                .await
                .unwrap(),
            2
        );
        let runs = store.recent_runs(Some("queue#0"), 10).await.unwrap();
        assert_eq!(
            runs.iter()
                .filter(|r| r.outcome == RunOutcome::Interrupted)
                .count(),
            2
        );
        assert!(
            runs.iter()
                .any(|r| r.process == "host:1-a" && r.outcome == RunOutcome::Completed)
        );

        // Restoring after a false sweep: only the rows the sweep marked come back as running.
        c.counters.insert("pages".into(), 3);
        assert!(store.restore_run(&c).await.unwrap());
        let mut b_done = b.clone();
        b_done.outcome = RunOutcome::Completed;
        b_done.ended_at_ms = Some(30);
        store.upsert_run(&b_done).await.unwrap();
        assert!(
            !store.restore_run(&b).await.unwrap(),
            "a final outcome written meanwhile stays"
        );
        assert!(!store.restore_run(&a).await.unwrap(), "never marked");
        let runs = store.recent_runs(Some("queue#0"), 10).await.unwrap();
        let find = |process: &str, id: u64| {
            runs.iter()
                .find(|r| r.process == process && r.run_id == id)
                .cloned()
                .unwrap()
        };
        let restored = find("host:2-b", 2);
        assert_eq!(restored.outcome, RunOutcome::Running);
        assert_eq!(restored.error, None);
        assert_eq!(restored.ended_at_ms, None);
        assert_eq!(restored.counters.get("pages"), Some(&3));
        assert_eq!(find("host:2-b", 1).outcome, RunOutcome::Completed);
        assert_eq!(find("host:1-a", 1).outcome, RunOutcome::Completed);
    }

    #[tokio::test]
    async fn per_process_runs_in_memory() {
        per_process_runs(&MemoryStore::default()).await;
    }

    #[tokio::test]
    async fn per_process_runs_on_sqlite() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        crate::migrations::up(&smeltery_core::db::migration::Schema::new(&db))
            .await
            .unwrap();
        let store = DbStore::open(db).await;
        assert!(store.processes());
        per_process_runs(&store).await;
    }
}
