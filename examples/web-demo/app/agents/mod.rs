//! Agents and jobs: supervised long-running workers, the job queue and the schedule (Watchfire).

use smeltery::watchfire::prelude::*;

use crate::app::jobs::bump_counter::BumpCounter;

pub mod scraper;
// smeltery:mods

/// Register every agent, job and scheduled task.
pub fn register(w: &mut Watchfire) {
    w.job::<BumpCounter>();
    scraper::register(w, crate::config::scraper::scraper());
    // The live counter on the dashboard: a job every 5 seconds, skipped while the previous one still runs.
    w.schedule()
        .job(BumpCounter {})
        .every(5.secs())
        .overlap(Overlap::Skip);
    // smeltery:agents
}
