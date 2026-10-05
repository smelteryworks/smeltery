//! The `bump_counter` job: adds one to the live counter and pushes a refresh to the pages showing it.

use serde::{Deserialize, Serialize};
use smeltery::sparks::Broadcast;
use smeltery::watchfire::prelude::*;

use crate::app::models::Metric;
use crate::app::sparks::live_counter;

/// A queued job: the schedule in `app/agents/mod.rs` dispatches one every 5 seconds, a queue worker runs `handle`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BumpCounter {}

impl Job for BumpCounter {
    const NAME: &'static str = "bump_counter";

    async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
        let value = Metric::increment(&ctx.db()?, live_counter::METRIC).await?;
        // Every open dashboard re-fetches its `live_counter` over the Sparks stream (server-sent events).
        let pages = ctx
            .service::<Broadcast>()
            .map_or(0, |b| b.to(live_counter::NAME).refresh());
        ctx.log().debug(format!(
            "live counter is {value}, pushed to {pages} page(s)"
        ));
        Ok(())
    }
}
