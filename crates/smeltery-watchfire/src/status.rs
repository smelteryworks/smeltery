//! Agent status, run records and log lines.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::policy::Restart;

/// The lifecycle state of an agent.
///
/// `Starting → Running → (Stopping → Stopped | Paused) | (BackingOff → Starting) | Failed |
/// Completed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentState {
    /// Waiting for a group or global concurrency permit before a run.
    Starting,
    /// A run is executing.
    Running,
    /// Paused: no run, no restarts until `resume`.
    Paused,
    /// Cancelled; waiting for the run to return.
    Stopping,
    /// Not running: never started, or stopped by a command or shutdown.
    Stopped,
    /// Waiting for the backoff delay before an automatic restart.
    BackingOff,
    /// The run returned `Ok(())` and the policy does not restart it.
    Completed,
    /// The run failed and the policy does not restart it, or the restart limit was reached.
    Failed,
    /// Another process sharing the lock store runs this singleton agent; this process takes over when that one
    /// stops or dies (see `WATCHFIRE_LOCK_STORE`).
    Standby,
}

impl AgentState {
    /// The name as stored and shown, e.g. `backing_off`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::BackingOff => "backing_off",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Standby => "standby",
        }
    }

    /// Parse [`AgentState::as_str`] output.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "starting" => Self::Starting,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "stopping" => Self::Stopping,
            "stopped" => Self::Stopped,
            "backing_off" => Self::BackingOff,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "standby" => Self::Standby,
            _ => return None,
        })
    }
}

/// Heartbeat health of a running agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Health {
    /// Not running, or no heartbeat timeout configured.
    Unknown,
    /// A heartbeat arrived within the timeout.
    Healthy,
    /// No heartbeat within the timeout (the run is about to be restarted).
    Stalled,
}

/// A snapshot of one agent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct AgentStatus {
    /// Agent name (`name#i` for pool members, `parent.child` for children).
    pub name: String,
    /// Lifecycle state.
    pub state: AgentState,
    /// Heartbeat health.
    pub health: Health,
    /// Restart policy.
    pub restart_policy: Restart,
    /// The group, if any.
    pub group: Option<String>,
    /// Automatic restarts plus `restart` commands so far.
    pub restarts: u64,
    /// Runs started so far; also the id of the latest run.
    pub runs: u64,
    /// When the current (or last) run started, Unix milliseconds.
    pub started_at_ms: Option<i64>,
    /// The last heartbeat the supervisor saw, Unix milliseconds.
    pub last_heartbeat_ms: Option<i64>,
    /// The error of the last failed run.
    pub last_error: Option<String>,
    /// The pending automatic restart's delay, milliseconds.
    pub backoff_ms: Option<i64>,
    /// When the pending automatic restart fires, Unix milliseconds.
    pub next_restart_at_ms: Option<i64>,
    /// When this snapshot changed, Unix milliseconds.
    pub updated_at_ms: i64,
    /// With a shared lock store (`WATCHFIRE_LOCK_STORE`), the process that holds this singleton agent (this one or
    /// another; `None` when no process holds it or the agent runs in every process).
    pub held_by: Option<String>,
}

impl AgentStatus {
    pub(crate) fn new(name: &str, policy: Restart, group: Option<String>, now_ms: i64) -> Self {
        Self {
            name: name.to_owned(),
            state: AgentState::Stopped,
            health: Health::Unknown,
            restart_policy: policy,
            group,
            restarts: 0,
            runs: 0,
            started_at_ms: None,
            last_heartbeat_ms: None,
            last_error: None,
            backoff_ms: None,
            next_restart_at_ms: None,
            updated_at_ms: now_ms,
            held_by: None,
        }
    }
}

/// How a run ended. Every run gets exactly one, including on shutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunOutcome {
    /// Still running.
    Running,
    /// Returned `Ok(())` on its own.
    Completed,
    /// Returned an error.
    Failed,
    /// Panicked.
    Panicked,
    /// Cancelled by a command or shutdown, and returned in time.
    Stopped,
    /// Cancelled, but did not return within its shutdown timeout and was dropped.
    Killed,
    /// The process ended during the run (found on the next launch).
    Interrupted,
    /// No heartbeat within the heartbeat timeout; cancelled and restarted.
    Stalled,
}

impl RunOutcome {
    /// The name as stored and shown, e.g. `panicked`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Panicked => "panicked",
            Self::Stopped => "stopped",
            Self::Killed => "killed",
            Self::Interrupted => "interrupted",
            Self::Stalled => "stalled",
        }
    }

    /// Parse [`RunOutcome::as_str`] output.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "panicked" => Self::Panicked,
            "stopped" => Self::Stopped,
            "killed" => Self::Killed,
            "interrupted" => Self::Interrupted,
            "stalled" => Self::Stalled,
            _ => return None,
        })
    }

    /// Failed, panicked, killed or stalled.
    pub fn is_failure(self) -> bool {
        matches!(
            self,
            Self::Failed | Self::Panicked | Self::Killed | Self::Stalled
        )
    }
}

/// One run of an agent (or one job / scheduled call handled by it), from start to end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct RunRecord {
    /// Agent name.
    pub agent: String,
    /// Run id, counting from 1 per agent.
    pub run_id: u64,
    /// The job (or scheduled call) this run handled, for queue workers and the scheduler.
    pub job: Option<String>,
    /// Start, Unix milliseconds.
    pub started_at_ms: i64,
    /// End, Unix milliseconds; `None` while running or when interrupted.
    pub ended_at_ms: Option<i64>,
    /// How it ended.
    pub outcome: RunOutcome,
    /// The error or panic message.
    pub error: Option<String>,
    /// Counters (`ctx.counter(name)`) at the end of the run.
    pub counters: BTreeMap<String, i64>,
    /// The process that ran it (host, process id and a random part, unique per launch); empty in history written
    /// before the `process` column existed.
    pub process: String,
}

impl RunRecord {
    pub(crate) fn started(agent: &str, run_id: u64, job: Option<String>, at_ms: i64) -> Self {
        Self {
            agent: agent.to_owned(),
            run_id,
            job,
            started_at_ms: at_ms,
            ended_at_ms: None,
            outcome: RunOutcome::Running,
            error: None,
            counters: BTreeMap::new(),
            process: String::new(),
        }
    }
}

/// A log line kept in an agent's ring buffer (the last 200, see `Agents::logs`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct LogLine {
    /// When, Unix milliseconds.
    pub at_ms: i64,
    /// `debug`, `info`, `warn` or `error`.
    pub level: &'static str,
    /// The run it came from.
    pub run_id: u64,
    /// The message.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for s in [
            AgentState::Starting,
            AgentState::Running,
            AgentState::Paused,
            AgentState::Stopping,
            AgentState::Stopped,
            AgentState::BackingOff,
            AgentState::Completed,
            AgentState::Failed,
        ] {
            assert_eq!(AgentState::parse(s.as_str()), Some(s));
            assert_eq!(
                serde_json::to_value(s).ok(),
                Some(serde_json::Value::from(s.as_str()))
            );
        }
        for o in [
            RunOutcome::Running,
            RunOutcome::Completed,
            RunOutcome::Failed,
            RunOutcome::Panicked,
            RunOutcome::Stopped,
            RunOutcome::Killed,
            RunOutcome::Interrupted,
            RunOutcome::Stalled,
        ] {
            assert_eq!(RunOutcome::parse(o.as_str()), Some(o));
        }
        assert_eq!(AgentState::parse("nope"), None);
    }
}
