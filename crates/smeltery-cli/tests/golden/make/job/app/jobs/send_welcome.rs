//! The `send_welcome` job.

use serde::{Deserialize, Serialize};
use smeltery::watchfire::prelude::*;

/// A queued job: `SendWelcome {}.dispatch(&app).await?` puts it on the queue, a worker runs `handle`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SendWelcome {}

impl Job for SendWelcome {
    const NAME: &'static str = "send_welcome";

    async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
        ctx.log().info("handled");
        Ok(())
    }
}
