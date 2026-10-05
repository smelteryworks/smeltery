//! The `live_counter` Spark, shown on the dashboard; its view is `resources/views/sparks/live_counter.mold.html`.

use serde::{Deserialize, Serialize};
use smeltery::prelude::*;

use crate::app::models::Metric;

/// The component's name: the `BumpCounter` job pushes refreshes to it.
pub const NAME: &str = "live_counter";
/// The row of the `metrics` table it shows.
pub const METRIC: &str = "live_counter";

/// A counter the server pushes: `#[spark(stream)]` keeps the page subscribed to `/_sparks/stream`, and every
/// `Broadcast::to("live_counter").refresh()` re-renders it with the value from the database.
#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(name = "live_counter", stream)]
pub struct LiveCounter {
    /// The counter's value at the last render.
    pub value: i64,
}

#[actions]
impl LiveCounter {
    /// Before every render (the first one and each pushed refresh): read the current value.
    pub async fn rendering(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        self.value = Metric::current(&ctx.db()?, METRIC).await?;
        Ok(())
    }
}
