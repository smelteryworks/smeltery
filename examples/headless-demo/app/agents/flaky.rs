//! The `flaky` agent: a worker that fails on purpose, to show supervision at work.
//!
//! A run processes `FLAKY_BATCHES_PER_RUN` batches and ends; about `FLAKY_FAILURE_PERCENT` of the runs fail at a
//! random batch instead (seeded, so a seed gives the same failures every time). The supervisor restarts every
//! run (`Restart::Always`) after an exponential backoff with jitter, and marks the agent `failed` when it needs
//! more than 5 restarts within a minute. `smeltery agents:runs flaky` lists the runs and their outcomes.

use smeltery::db::factory::Fake;
use smeltery::watchfire::prelude::*;

use crate::config::agents::FlakyConfig;

/// The agent's name.
pub const NAME: &str = "flaky";

/// A worker processing batches; some runs fail.
pub struct Flaky {
    config: FlakyConfig,
    /// The random numbers. A field of the agent, so it survives restarts and the sequence goes on.
    rng: Fake,
    /// Batches processed over every run.
    batches: u64,
}

impl Flaky {
    /// A worker with these settings.
    pub fn new(config: FlakyConfig) -> Self {
        let rng = Fake::seeded(config.seed);
        Self {
            config,
            rng,
            batches: 0,
        }
    }
}

impl Agent for Flaky {
    fn name(&self) -> String {
        NAME.into()
    }

    fn config(&self) -> AgentConfig {
        AgentConfig::default()
            .restart(Restart::Always)
            .backoff(1.secs()..=10.secs())
            .max_restarts(5, 1.mins())
            .heartbeat_timeout(2.mins())
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        let per_run = self.config.batches_per_run.max(1);
        let fail_at = (self.rng.int(1..=100) <= self.config.failure_percent)
            .then(|| self.rng.int(1..=per_run));
        for batch in 1..=per_run {
            // `sleep` returns false once the agent is asked to stop.
            if !ctx.sleep(self.config.batch).await {
                return Ok(());
            }
            ctx.heartbeat();
            self.batches += 1;
            ctx.counter("batches").inc();
            if fail_at == Some(batch) {
                return Err(AgentError::msg(format!(
                    "batch {} failed (a simulated failure)",
                    self.batches
                )));
            }
            ctx.log().info(format!("batch {} done", self.batches));
        }
        Ok(())
    }
}
