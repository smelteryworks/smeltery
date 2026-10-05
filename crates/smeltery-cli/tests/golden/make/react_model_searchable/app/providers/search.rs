//! Search (Prospect): the searchable models. `smeltery make:model Name … --searchable` adds them here.

use smeltery::prospect::Models;

/// Registers every searchable model (`bootstrap/app.rs` calls it through `.prospect(…)`).
pub fn register(p: &mut Models) {
    p.model::<crate::app::models::Post>();
    // smeltery:searchables
}
