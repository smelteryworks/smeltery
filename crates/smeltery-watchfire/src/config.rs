//! Per-agent configuration and name rules.

use std::ops::RangeInclusive;
use std::time::Duration;

use crate::error::Error;
use crate::policy::{Backoff, Restart};

/// Longest accepted agent, group or schedule name.
pub const MAX_NAME_LEN: usize = 64;

/// How the runtime supervises one agent. Every setter is also on the builder handle that
/// `Watchfire::run` / `every` / `agent` / `pool` return.
///
/// ```
/// use smeltery_watchfire::prelude::*;
///
/// let config = AgentConfig::default()
///     .restart(Restart::OnFailure)
///     .backoff(1.secs()..=60.secs())
///     .max_restarts(5, 10.mins())
///     .heartbeat_timeout(2.mins())
///     .group("scrapers");
/// assert_eq!(config.restart_policy(), Restart::OnFailure);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct AgentConfig {
    pub(crate) restart: Restart,
    pub(crate) backoff: Backoff,
    pub(crate) max_restarts: Option<(u32, Duration)>,
    pub(crate) heartbeat_timeout: Option<Duration>,
    pub(crate) group: Option<String>,
    pub(crate) autostart: bool,
    pub(crate) shutdown_timeout: Duration,
    pub(crate) concurrency: usize,
    pub(crate) per_process: bool,
}

impl Default for AgentConfig {
    /// Restart on failure, backoff 1 s..=60 s, no restart limit, no heartbeat timeout, no
    /// group, autostart on, 10 s shutdown timeout, concurrency 1, one process at a time.
    fn default() -> Self {
        Self {
            restart: Restart::OnFailure,
            backoff: Backoff::default(),
            max_restarts: None,
            heartbeat_timeout: None,
            group: None,
            autostart: true,
            shutdown_timeout: Duration::from_secs(10),
            concurrency: 1,
            per_process: false,
        }
    }
}

impl AgentConfig {
    /// The restart policy.
    pub fn restart(mut self, policy: Restart) -> Self {
        self.restart = policy;
        self
    }

    /// Backoff between automatic restarts: `initial..=cap`, doubling, full jitter.
    pub fn backoff(mut self, range: RangeInclusive<Duration>) -> Self {
        self.backoff = Backoff::new(range);
        self
    }

    /// At most `count` automatic restarts within `per`; one more and the agent is `Failed`
    /// (with an alert) until started again.
    pub fn max_restarts(mut self, count: u32, per: Duration) -> Self {
        self.max_restarts = Some((count, per));
        self
    }

    /// A run that has not heartbeated (`ctx.heartbeat()`, or a `Ticker` tick) for this long is
    /// stalled: it is cancelled, recorded as `Stalled` and restarted like a failure.
    pub fn heartbeat_timeout(mut self, timeout: Duration) -> Self {
        self.heartbeat_timeout = Some(timeout);
        self
    }

    /// Put the agent in a group (see `Watchfire::group(..).limit(n)`).
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// Start the agent when Watchfire launches (default `true`).
    pub fn autostart(mut self, autostart: bool) -> Self {
        self.autostart = autostart;
        self
    }

    /// How long `stop`, `pause`, `restart` and shutdown wait for a cancelled run to return
    /// before dropping it (outcome `Killed`). Shutdown also caps it by the app's budget. With a
    /// shared lock store a singleton agent's lease must cover it: `WATCHFIRE_LEASE_TTL` of at least
    /// 12/7 of it, or the launch is refused.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// How many [`AgentCtx::acquire`](crate::AgentCtx::acquire) permits a run may hold at once
    /// (default 1; `0` is treated as 1).
    pub fn concurrency(mut self, permits: usize) -> Self {
        self.concurrency = permits.clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
        self
    }

    /// Run the agent in every process (`serve`, `work`) instead of in one process at a time. With a shared lock
    /// store (`WATCHFIRE_LOCK_STORE`) every agent is a singleton by default; without one, every process runs its
    /// own copy anyway.
    pub fn per_process(mut self) -> Self {
        self.per_process = true;
        self
    }

    /// Whether the agent runs in every process ([`AgentConfig::per_process`]).
    pub fn is_per_process(&self) -> bool {
        self.per_process
    }

    /// The restart policy.
    pub fn restart_policy(&self) -> Restart {
        self.restart
    }

    /// The backoff.
    pub fn backoff_policy(&self) -> &Backoff {
        &self.backoff
    }

    /// The group, if any.
    pub fn group_name(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The heartbeat timeout, if any.
    pub fn heartbeat_limit(&self) -> Option<Duration> {
        self.heartbeat_timeout
    }
}

/// Check a user-given name: 1 to 64 characters out of `a-z`, `0-9`, `_` and `-`.
pub(crate) fn validate_name(name: &str) -> Result<(), Error> {
    let reason = if name.is_empty() {
        Some("must not be empty")
    } else if name.len() > MAX_NAME_LEN {
        Some("must be at most 64 characters")
    } else if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        Some("may only contain a-z, 0-9, '_' and '-'")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(Error::InvalidName {
            name: name.to_owned(),
            reason,
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        assert!(validate_name("price_poller-2").is_ok());
        for bad in ["", "Price", "a b", "x/y", "p#1", &"a".repeat(65)] {
            assert!(
                matches!(validate_name(bad), Err(Error::InvalidName { .. })),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn concurrency_is_clamped() {
        assert_eq!(AgentConfig::default().concurrency(0).concurrency, 1);
    }
}
