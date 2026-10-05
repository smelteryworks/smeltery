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
use smeltery::sparks::SparksExt as _;
use smeltery::temper::TemperExt as _;
use smeltery::watchfire::AgentsExt as _;

/// Builds the application: registers config, authentication, migrations, seeders, commands, agents, Alloy (React pages) and routes.
pub fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.config(config::app::app())
        // The authentication routes and their pages: `app/providers/temper.rs`.
        .temper(app::providers::temper::temper())
        // Require verified e-mail addresses (the `verified` middleware): uncomment the next line.
        // .verify_email::<app::models::User>()
        .migrations(database::migrations::register)
        .seeders(database::seeders::register)
        .commands(app::commands::register)
        .bellows()
        .mail()
        .agents(app::agents::register)
        .alloy(
            Alloy::new()
                .root::<app::providers::alloy::Root>()
                .entries(["resources/js/app.tsx"])
                // The browser keeps every page's props in its history; encrypted, the `clear_history` of logging
                // out makes them unreadable, so the back button cannot show a signed-in page (needs HTTPS or
                // 127.0.0.1; elsewhere the browser stores them unencrypted).
                .encrypt_history()
                .share(app::providers::alloy::shared),
        )
        // No Sparks of the app's own: the Watchfire dashboard's live panels need Sparks installed.
        .sparks(|_| {})
        .routes(routes::web::routes)
        .api_routes(routes::api::routes)
}
