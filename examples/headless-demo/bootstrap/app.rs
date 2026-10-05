//! The headless-demo application: its modules and the function that wires them into Smeltery.

#[path = "../app/mod.rs"]
pub mod app;
#[path = "../config/mod.rs"]
pub mod config;
#[path = "../database/mod.rs"]
pub mod database;

use smeltery::bellows::BellowsExt as _;
use smeltery::mail::MailExt as _;
use smeltery::watchfire::AgentsExt as _;

/// Builds the application: registers config, migrations, seeders, commands and agents.
///
/// A headless app serves no routes: `smeltery work` runs its agents, jobs and schedule.
pub fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.config(config::app::app())
        .migrations(database::migrations::register)
        .seeders(database::seeders::register)
        .commands(app::commands::register)
        .bellows()
        .mail()
        .agents(app::agents::register)
}
