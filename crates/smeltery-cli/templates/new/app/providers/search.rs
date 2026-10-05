//! Search (Prospect): the searchable models. `smeltery make:model Name … --searchable` adds them here.

use smeltery::prospect::Models;

/// Registers every searchable model (`bootstrap/app.rs` calls it through `.prospect(…)`).
pub fn register(p: &mut Models) {
    let _ = &p; // keeps `p` used while nothing is registered
    // smeltery:searchables
}
