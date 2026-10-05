//! The my-app application: its modules and the function that wires them into Smeltery.

#[path = "../app/mod.rs"]
pub mod app;
#[path = "../config/mod.rs"]
pub mod config;
#[path = "../database/mod.rs"]
pub mod database;
#[path = "../routes/mod.rs"]
pub mod routes;

use smeltery::anvil::AnvilExt as _;
use smeltery::bellows::BellowsExt as _;
use smeltery::mail::MailExt as _;
use smeltery::sparks::SparksExt as _;

/// Builds the application: registers config, broadcasting, migrations, seeders, commands, Sparks and routes.
pub fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.config(config::app::app())
        // WebSockets and broadcasting: the channels of `routes/channels.rs`; `ANVIL_*` in `.env`.
        .anvil(routes::channels::channels)
        .migrations(database::migrations::register)
        .seeders(database::seeders::register)
        .commands(app::commands::register)
        .bellows()
        .mail()
        .sparks(app::sparks::register)
        .routes(routes::web::routes)
        .api_routes(routes::api::routes)
}
