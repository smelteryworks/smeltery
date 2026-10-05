//! Agents and jobs: supervised long-running workers, the job queue and the schedule (Watchfire).

use smeltery::watchfire::prelude::*;

// smeltery:mods

/// Register every agent, job and scheduled task.
pub fn register(w: &mut Watchfire) {
    let _ = &w; // keeps `w` used while nothing is registered
    // smeltery:agents
}
