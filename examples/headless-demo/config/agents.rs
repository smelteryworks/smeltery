//! Settings of the demo agents.

use std::time::Duration;

use smeltery::config::env;

/// The `poller` agent.
#[derive(Debug, Clone)]
pub struct PollerConfig {
    /// The URL polled for a number (`POLLER_URL`): a plain number or JSON `{"value": n}`. Empty: the poller
    /// simulates a value instead.
    pub url: String,
    /// Time between two polls (`POLLER_EVERY_SECS`).
    pub every: Duration,
}

/// The `flaky` agent.
#[derive(Debug, Clone)]
pub struct FlakyConfig {
    /// The chance, in percent, that a run fails (`FLAKY_FAILURE_PERCENT`).
    pub failure_percent: i64,
    /// The seed of its random numbers (`FLAKY_SEED`): the same seed gives the same failures.
    pub seed: u64,
    /// Time one batch takes (`FLAKY_BATCH_SECS`).
    pub batch: Duration,
    /// Batches in one run (`FLAKY_BATCHES_PER_RUN`).
    pub batches_per_run: i64,
}

/// Reads the poller settings.
pub fn poller() -> PollerConfig {
    PollerConfig {
        url: env("POLLER_URL", ""),
        every: Duration::from_secs(env("POLLER_EVERY_SECS", 30)),
    }
}

/// Reads the flaky worker settings.
pub fn flaky() -> FlakyConfig {
    FlakyConfig {
        failure_percent: env("FLAKY_FAILURE_PERCENT", 30),
        seed: env("FLAKY_SEED", 42),
        batch: Duration::from_secs(env("FLAKY_BATCH_SECS", 5)),
        batches_per_run: env("FLAKY_BATCHES_PER_RUN", 6),
    }
}
