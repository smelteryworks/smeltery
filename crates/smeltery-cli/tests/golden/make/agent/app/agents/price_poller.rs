//! The `price_poller` agent.

use smeltery::watchfire::prelude::*;

/// A supervised agent: restarted with backoff when `run` fails, restarted when it stops sending heartbeats.
#[derive(Default)]
pub struct PricePoller {}

impl Agent for PricePoller {
    fn name(&self) -> String {
        "price_poller".into()
    }

    fn config(&self) -> AgentConfig {
        AgentConfig::default()
            .restart(Restart::OnFailure)
            .backoff(1.secs()..=60.secs())
            .heartbeat_timeout(2.mins())
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        // Every tick is also a heartbeat; `tick()` returns false once the agent is stopped.
        let mut ticker = ctx.interval(30.secs());
        while ticker.tick().await {
            ctx.log().info("tick");
        }
        Ok(())
    }
}
