//! Agents and jobs: supervised long-running workers, the job queue and the schedule (Watchfire).

use smeltery::watchfire::prelude::*;

pub mod price_poller;
// smeltery:mods

/// Register every agent, job and scheduled task.
pub fn register(w: &mut Watchfire) {
    w.agent(price_poller::PricePoller::default());
    w.job::<crate::app::jobs::send_welcome::SendWelcome>();
    // smeltery:agents
}
