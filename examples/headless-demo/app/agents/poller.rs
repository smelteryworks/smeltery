//! The `poller` agent: reads a number every `POLLER_EVERY_SECS` seconds and keeps the latest one in its
//! checkpoint, so a restart continues the count instead of starting over.

use serde::{Deserialize, Serialize};
use smeltery::watchfire::prelude::*;

use crate::config::agents::PollerConfig;

/// The agent's name.
pub const NAME: &str = "poller";

/// What the poller remembers across restarts (its checkpoint).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PollState {
    /// Polls so far.
    pub polls: u64,
    /// The latest value.
    pub last: Option<f64>,
}

/// A supervised agent: restarted with backoff when a poll fails, restarted when it stops sending heartbeats.
pub struct Poller {
    config: PollerConfig,
}

impl Poller {
    /// A poller with these settings.
    pub fn new(config: PollerConfig) -> Self {
        Self { config }
    }
}

impl Agent for Poller {
    fn name(&self) -> String {
        NAME.into()
    }

    fn config(&self) -> AgentConfig {
        AgentConfig::default()
            .restart(Restart::OnFailure)
            .backoff(1.secs()..=60.secs())
            .heartbeat_timeout((self.config.every * 3).max(2.mins()))
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        let mut state = ctx.checkpoint_get::<PollState>().await?.unwrap_or_default();
        if state.polls > 0 {
            ctx.log()
                .info(format!("resuming after {} polls", state.polls));
        }
        let source = if self.config.url.is_empty() {
            "simulated"
        } else {
            "http"
        };
        // The first tick is at once; every tick is also a heartbeat.
        let mut ticker = ctx.interval(self.config.every);
        while ticker.tick().await {
            let value = if self.config.url.is_empty() {
                simulate(state.polls)
            } else {
                // Timeouts and retries come from `ctx.http()`; an error left over fails the run, and the
                // supervisor restarts it after a backoff.
                fetch(&ctx, &self.config.url).await?
            };
            state.polls += 1;
            state.last = Some(value);
            ctx.counter("polls").inc();
            ctx.log()
                .info(format!("poll {}: {value} ({source})", state.polls));
            ctx.checkpoint(&state).await?;
        }
        Ok(())
    }
}

/// A value without a URL: a slow wave around 20, the same for the same poll number.
pub fn simulate(poll: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let x = poll as f64 * 0.3;
    ((20.0 + 5.0 * x.sin()) * 100.0).round() / 100.0
}

/// GETs `url` and reads a number from the body: plain text (`21.5`) or JSON with a `value` field.
async fn fetch(ctx: &AgentCtx, url: &str) -> Result<f64, AgentError> {
    #[derive(Deserialize)]
    struct Reading {
        value: f64,
    }
    let res = ctx.http().get(url).await?.error_for_status()?;
    if let Ok(n) = res.text().await?.trim().parse::<f64>() {
        return Ok(n);
    }
    match res.json::<Reading>().await {
        Ok(reading) => Ok(reading.value),
        Err(_) => Err(AgentError::msg(format!("{url} did not answer a number"))),
    }
}
