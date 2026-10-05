//! The `todo_list` Spark; its view is `resources/views/sparks/todo_list.mold.html`.

use serde::{Deserialize, Serialize};
use smeltery::prelude::*;

/// A live component: `@spark("todo_list")` shows it on a page.
#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(name = "todo_list")]
pub struct TodoList {
    /// Set by the page through `wire:model="message"`.
    #[spark(model)]
    pub message: String,
    /// How many times `save` ran.
    pub saved: i64,
}

#[actions]
impl TodoList {
    /// `wire:click="save"`.
    pub async fn save(&mut self) -> Result<()> {
        self.saved += 1;
        Ok(())
    }
}
