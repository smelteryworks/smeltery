#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub use smeltery_core::{App, AppBuilder, BoxFuture, Built, Error, Response, Result, run, serve};
pub use smeltery_core::{
    auth, cache, channels, config, console, crypto, db, html, http, middleware, pubsub, routing,
    session, testing, validation, view,
};

/// Watchfire, the agent runtime: supervised agents, jobs and the queue, the scheduler
/// (`smeltery::watchfire::{Watchfire, Agent, AgentCtx, Job, AgentsExt, …}`).
pub use smeltery_watchfire as watchfire;

/// Bellows, AI-agent support: the `bellows:mcp` MCP server (`smeltery::bellows::BellowsExt`).
pub use smeltery_bellows as bellows;

/// Alloy, the React and Vue bridge (the Inertia protocol): `smeltery::alloy::{Alloy, AlloyExt, render, Page, Props,
/// SharedCtx, …}`.
pub use smeltery_alloy as alloy;

/// Anvil, the real-time layer: a WebSocket server speaking the Pusher Channels protocol, broadcasting and channel
/// authorization (`smeltery::anvil::{Anvil, AnvilExt, Channels, ChannelCtx, BroadcastEvent, Channel, SocketId, …}`).
pub use smeltery_anvil as anvil;

/// Hallmark, API tokens: personal access tokens with abilities and the `hallmark` bearer guard
/// (`smeltery::hallmark::{Hallmark, HallmarkExt, Tokens, CurrentToken, HasApiTokens, …}`).
pub use smeltery_hallmark as hallmark;

/// Temper, the authentication routes: login, registration, password reset, e-mail verification, password
/// confirmation, profile and password updates (`smeltery::temper::{Temper, TemperExt, TemperViews, CreatesNewUsers,
/// TemperResponses, TemperEvent, …}`).
pub use smeltery_temper as temper;

/// Prospect, full-text search for models: `smeltery::prospect::{Prospect, ProspectExt, Searchable, IndexSpec, Hit,
/// Search, migration::SearchIndex, …}`.
pub use smeltery_prospect as prospect;

/// Sparks, live components: `smeltery::sparks::{Sparks, SparksExt, SparkCtx, Broadcast, TemporaryUpload, …}`.
pub use smeltery_sparks as sparks;

/// Mail: mail classes with Mold templates, the `Mailer`, SMTP / log / fake transports
/// (`smeltery::mail::{Mailable, Mailer, Envelope, MailExt, …}`). Queued and alert mail live in
/// `smeltery::watchfire::mail`.
pub use smeltery_mail as mail;

/// Mold, the template engine: `smeltery::mold::{Template, Host, Value, Engine, …}`.
pub use smeltery_mold as mold;

/// `#[derive(Mold)]`: compiles `resources/views/<name>.mold.html` for a struct and makes it a response.
pub use smeltery_mold_macros::Mold;

/// `#[derive(Validate)]`: validation rules on a form struct (see [`validation`]).
pub use smeltery_macros::Validate;

/// `#[derive(Alloy)]`: a struct as an Alloy page (see [`alloy`]).
pub use smeltery_macros::Alloy;

/// `#[derive(Spark)]` and `#[actions]`: a live component's state, view and callable actions (see [`sparks`]).
pub use smeltery_macros::{Spark, actions};

/// Build a `serde_json::Value` with JSON syntax, e.g. `json!({"status": "ok"})`.
pub use serde_json::json;

/// The version of Smeltery the app is built with (the new app's welcome page shows it).
///
/// ```
/// assert!(!smeltery::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The items most apps use, in one import: `use smeltery::prelude::*;`.
pub mod prelude {
    pub use smeltery_alloy::AlloyExt as _;
    pub use smeltery_anvil::AnvilExt as _;
    pub use smeltery_bellows::BellowsExt as _;
    pub use smeltery_core::auth::{Auth, Authenticated};
    pub use smeltery_core::cache::Cache;
    pub use smeltery_core::config::{Config, env};
    pub use smeltery_core::db::{Db, Found, Page, PageQuery, Record};
    pub use smeltery_core::http::Back;
    pub use smeltery_core::http::{Form, Html, IntoResponse, Json, Path, Query, Redirect};
    pub use smeltery_core::routing::Router;
    pub use smeltery_core::session::Session;
    pub use smeltery_core::validation::{Valid, Validate};
    pub use smeltery_core::view::view;
    pub use smeltery_core::{App, AppBuilder, Error, Response, Result};
    pub use smeltery_hallmark::HallmarkExt as _;
    pub use smeltery_macros::{Spark, Validate, actions};
    pub use smeltery_mail::MailExt as _;
    pub use smeltery_mail::{Attachment, Envelope, Mailable, Mailer};
    pub use smeltery_mold::Template;
    pub use smeltery_mold_macros::Mold;
    pub use smeltery_prospect::ProspectExt as _;
    pub use smeltery_prospect::Searchable as _;
    pub use smeltery_sparks::SparksExt as _;
    pub use smeltery_sparks::{Broadcast, SparkCtx, Sparks, TemporaryUpload};
    pub use smeltery_temper::TemperExt as _;
    pub use smeltery_watchfire::AgentsExt as _;
    pub use smeltery_watchfire::mail::QueueMail as _;
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_readme_matches_the_repo_readme() {
        // The crate README is a copy of the repository README (published with the crate).
        assert_eq!(
            include_str!("../README.md"),
            include_str!("../../../README.md")
        );
    }
}
