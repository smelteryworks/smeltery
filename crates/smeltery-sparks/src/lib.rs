#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod assets;
mod broadcast;
mod component;
mod ctx;
mod listen;
mod runtime;
mod snapshot;
pub mod testing;
mod update;
mod upload;

use std::sync::Arc;

use smeltery_core::view::SparkRenderer;
use smeltery_core::{AppBuilder, Error};

pub use assets::SPARKS_JS;
pub use broadcast::{Broadcast, Target};
pub use component::{ActionInfo, Actions, Guard, ListenerInfo, Spark, UploadRule};
pub use ctx::SparkCtx;
pub use runtime::Sparks;
pub use upload::TemporaryUpload;

/// The wire protocol version (the `v` of snapshots and requests).
pub const PROTOCOL_VERSION: u32 = 3;

/// The crate version, which the client runtime's URL carries (`/_sparks/sparks.js?v=…`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Installs Sparks on an [`AppBuilder`]: `.sparks(app::sparks::register)` in `bootstrap/app.rs`.
pub trait SparksExt: Sized {
    /// Register the app's components with `register`, render `@spark` / `@sparksScripts` in views, and add the
    /// routes `POST /_sparks/update` and `POST /_sparks/upload` (web routes: session and CSRF),
    /// `GET /_sparks/sparks.js` and `GET /_sparks/stream`. Also registers the [`Broadcast`] service.
    /// Two components with one name fail the build.
    fn sparks(self, register: impl FnOnce(&mut Sparks)) -> Self;
}

impl SparksExt for AppBuilder {
    fn sparks(self, register: impl FnOnce(&mut Sparks)) -> Self {
        let mut sparks = Sparks::new();
        register(&mut sparks);
        let duplicates: Vec<&'static str> = sparks.duplicates().to_vec();
        let broadcast = Broadcast::new();
        let runtime = Arc::new(runtime::Runtime::new(sparks, broadcast.clone()));
        let renderer: Arc<dyn SparkRenderer> = runtime.clone();
        let attached = broadcast.clone();
        let relayed = broadcast.clone();
        let builder = self
            .service(broadcast)
            // Pushes also go to the app's other processes through its PubSub ...
            .on_boot(move |app| async move {
                if let Some(bus) = smeltery_core::pubsub::PubSub::of(&app) {
                    attached.attach(bus);
                }
                // Listeners need `stream`, an installed broadcasting crate and the fields their channels name.
                runtime::Runtime::of(&app)?.check(&app)
            })
            // ... and a serving process hands theirs to its own streams (only a web process holds streams).
            .on_serve(move |app| async move {
                if let Some(bus) = smeltery_core::pubsub::PubSub::of(&app) {
                    // Owned by the app: `serve` waits for it at shutdown (it ends on the token).
                    let token = app.shutdown_token().clone();
                    app.spawn_owned(broadcast::relay(relayed, bus, token));
                }
                Ok(())
            })
            .service(runtime)
            .service(renderer)
            .routes(|r| {
                r.post("/_sparks/update", update::update)
                    .name("sparks.update");
                r.post("/_sparks/upload", upload::upload)
                    .name("sparks.upload");
            })
            .api_routes_at("/_sparks", |r| {
                r.get("/sparks.js", assets::script).name("sparks.script");
                r.get("/stream", broadcast::stream).name("sparks.stream");
            });
        if duplicates.is_empty() {
            builder
        } else {
            builder.on_boot(move |_| async move {
                Err(Error::internal(format!(
                    "two Sparks share the name `{}`",
                    duplicates.join("`, `")
                )))
            })
        }
    }
}

/// The stream token of instance `id` of component `name`: the value of `wire:stream="…"` that lets a page subscribe
/// to that name and id on `GET /_sparks/stream`. Renders of `#[spark(stream)]` components write it themselves
/// (when their `can_stream` hook allows); this is for components whose wrapper is rendered outside the normal mount
/// path. Call it only after checking that the viewer may receive the component's pushes. It is valid for the
/// snapshot time to live (`Sparks::snapshot_ttl`, default the session lifetime) and bound to `session` and the user
/// signed in with `auth` (the request's): the stream accepts it only from that session and user, and ends when that
/// session is signed out. Pass `None` for both only on a page without a session.
///
/// ```
/// use smeltery_sparks::SparksExt as _;
///
/// let app = smeltery_core::testing::TestApp::new(|b| b.sparks(|_| {}));
/// let token = smeltery_sparks::stream_token(app.app(), "watchfire.agents", "a1", None, None)?;
/// let wrapper = format!(r#"<div wire:id="a1" wire:name="watchfire.agents" wire:stream="{token}"></div>"#);
/// assert!(wrapper.contains("wire:stream=\""));
/// # Ok::<(), smeltery_core::Error>(())
/// ```
///
/// # Errors
/// The app cannot sign (no `APP_KEY`).
pub fn stream_token(
    app: &smeltery_core::App,
    name: &str,
    id: &str,
    session: Option<&smeltery_core::session::Session>,
    auth: Option<&smeltery_core::auth::Auth>,
) -> Result<String, Error> {
    let ttl = runtime::Runtime::of(app).map_or_else(
        |_| app.settings().session_lifetime,
        |runtime| runtime.snapshot_ttl(app),
    );
    broadcast::stream_token(
        app,
        name,
        id,
        ttl,
        broadcast::Viewer { session, auth },
        Vec::new(),
    )
}

/// Add components to an app whose Sparks are installed, from an `on_boot` hook: how a framework crate (such as
/// Watchfire's live dashboard) registers its own components without the app listing them. Returns `false` when
/// the app has no Sparks (`.sparks(…)` was not called).
///
/// # Errors
/// A name is already taken, or a component's listeners do not fit the app (see the README, "Listening to
/// broadcasts").
pub fn extend(app: &smeltery_core::App, register: impl FnOnce(&mut Sparks)) -> Result<bool, Error> {
    let Ok(runtime) = runtime::Runtime::of(app) else {
        return Ok(false);
    };
    let mut sparks = Sparks::new();
    register(&mut sparks);
    let taken = runtime.extend(sparks);
    if taken.is_empty() {
        runtime.check(app)?;
        Ok(true)
    } else {
        Err(Error::internal(format!(
            "two Sparks share the name `{}`",
            taken.join("`, `")
        )))
    }
}

/// What the `#[derive(Spark)]` and `#[actions]` expansions use. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use serde_json;
    pub use smeltery_core::{BoxFuture, Result};
    pub use smeltery_mold as mold;
    pub use smeltery_mold_macros::template as mold_template;

    use smeltery_core::Error;

    /// Action parameter `index` of `method`, deserialized.
    ///
    /// # Errors
    /// 400 when it is missing or has another type.
    pub fn param<T: serde::de::DeserializeOwned>(
        params: &[serde_json::Value],
        index: usize,
        method: &str,
    ) -> Result<T> {
        let value = params
            .get(index)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        serde_json::from_value(value).map_err(|_| {
            tracing::warn!(method, index, "Sparks call rejected: parameter type");
            Error::bad_request(format!(
                "parameter {index} of `{method}` has the wrong type"
            ))
        })
    }

    /// Check that `method` got `count` parameters.
    ///
    /// # Errors
    /// 400 otherwise.
    pub fn param_count(params: &[serde_json::Value], count: usize, method: &str) -> Result<()> {
        if params.len() == count {
            return Ok(());
        }
        tracing::warn!(
            method,
            expected = count,
            got = params.len(),
            "Sparks call rejected: parameter count"
        );
        Err(Error::bad_request(format!(
            "`{method}` takes {count} parameter(s), got {}",
            params.len()
        )))
    }

    /// A listener's event data, deserialized into its argument.
    ///
    /// # Errors
    /// 400 when the data does not fit the argument's type.
    pub fn listen_payload<T: serde::de::DeserializeOwned>(
        payload: serde_json::Value,
        method: &str,
    ) -> Result<T> {
        serde_json::from_value(payload).map_err(|_| {
            tracing::warn!(
                method,
                "Sparks listener rejected: the event data has another type"
            );
            Error::bad_request(format!(
                "the event data does not fit the argument of `{method}`"
            ))
        })
    }

    /// The error for a method that is not an action (403).
    pub fn unknown_action(method: &str) -> Error {
        Error::http(
            http::StatusCode::FORBIDDEN,
            format!("`{method}` is not an action"),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_client_runtime_speaks_this_protocol() {
        assert!(
            super::SPARKS_JS.contains(&format!("var PROTOCOL = {};", super::PROTOCOL_VERSION)),
            "js/sparks.js and PROTOCOL_VERSION disagree"
        );
    }
}
