//! Agents and jobs: supervised long-running workers, the job queue and the schedule (Watchfire).

use smeltery::watchfire::prelude::*;

pub mod flaky;
pub mod poller;
// smeltery:mods

/// Register every agent, job and scheduled task.
pub fn register(w: &mut Watchfire) {
    w.agent(poller::Poller::new(crate::config::agents::poller()));
    w.agent(flaky::Flaky::new(crate::config::agents::flaky()));
    // smeltery:agents
}
