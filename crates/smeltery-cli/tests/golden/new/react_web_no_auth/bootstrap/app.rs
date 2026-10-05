//! The my-app application: its modules and the function that wires them into Smeltery.

#[path = "../app/mod.rs"]
pub mod app;
#[path = "../config/mod.rs"]
pub mod config;
#[path = "../database/mod.rs"]
pub mod database;
#[path = "../routes/mod.rs"]
pub mod routes;

use smeltery::alloy::{Alloy, AlloyExt as _};
use smeltery::bellows::BellowsExt as _;
use smeltery::mail::MailExt as _;

/// Builds the application: registers config, migrations, seeders, commands, Alloy (React pages) and routes.
pub fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.config(config::app::app())
        .migrations(database::migrations::register)
        .seeders(database::seeders::register)
        .commands(app::commands::register)
        .bellows()
        .mail()
        .alloy(
            Alloy::new()
                .root::<app::providers::alloy::Root>()
                .entries(["resources/js/app.tsx"])
                .share(app::providers::alloy::shared),
        )
        .routes(routes::web::routes)
        .api_routes(routes::api::routes)
}
