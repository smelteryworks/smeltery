//! The `counter` Spark, shown on the home page; its view is `resources/views/sparks/counter.mold.html`.

use serde::{Deserialize, Serialize};
use smeltery::prelude::*;

/// A counter the visitor changes without reloading the page.
#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(name = "counter")]
pub struct Counter {
    /// The current count; only the actions change it.
    pub count: i64,
    /// How much one click adds or removes; the page sets it through `wire:model`.
    #[spark(model)]
    pub step: i64,
}

#[actions]
impl Counter {
    /// Runs on the first render: starts at the `start` prop of `@spark("counter", { start: 0 })`.
    pub async fn mount(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        self.count = ctx.prop("start").unwrap_or(0);
        self.step = 1;
        Ok(())
    }

    /// `wire:click="increment"`.
    pub async fn increment(&mut self) -> Result<()> {
        self.count += self.step;
        Ok(())
    }

    /// `wire:click="decrement"`.
    pub async fn decrement(&mut self) -> Result<()> {
        self.count -= self.step;
        Ok(())
    }
}
