//! The HTTP kernel of the [Smeltery](https://github.com/smelteryworks/smeltery) framework:
//! routing, configuration, the application container, errors, middleware, the server, the
//! database layer, PubSub between the app's processes and the console kernel.
//!
//! Apps use it through the `smeltery` facade crate; this crate's items are re-exported there.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod app;
pub mod auth;
pub mod cache;
pub mod channels;
pub(crate) mod client;
pub mod config;
pub mod console;
pub mod cors;
pub mod crypto;
pub mod db;
mod error;
mod fsx;
pub mod html;
pub mod http;
pub mod logging;
pub mod middleware;
mod multipart;
pub mod pubsub;
mod rate_limit;
pub mod routing;
mod server;
pub mod session;
pub mod testing;
mod upload;
pub mod validation;
pub mod view;

pub use app::{App, AppBuilder, Background, BoxFuture, Built, WeakApp};
pub use console::run;
pub use error::{Error, Result};
pub use server::{UpgradeHold, serve, serve_on};

/// An HTTP response.
pub type Response = axum::response::Response;
