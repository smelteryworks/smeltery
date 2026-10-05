//! `smeltery anvil`: a process that serves only the socket endpoint (D-416).
//!
//! It is the app binary with the command `anvil`: the same build (channels, settings, the derived keys), served by
//! core's [`serve_on`](smeltery_core::serve_on) on a listener of its own (`ANVIL_SERVER_HOST:ANVIL_SERVER_PORT`), so
//! the server's limits (`SERVER_MAX_CONNECTIONS`, `SERVER_MAX_CONNECTIONS_PER_IP`, the header timeout) and its
//! shutdown drain hold for it as for `serve`. Every path but the socket endpoint and `/up` answers 404; the auth
//! endpoints stay in the web process (they need the session); grants are stateless and events, auth events and
//! presence cross through the shared PubSub driver.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::Request;
use axum::middleware::Next;
use http::{HeaderValue, StatusCode};
use smeltery_core::console::Args;
use smeltery_core::pubsub::Driver;
use smeltery_core::{Built, Error, Response, Result};

use crate::Anvil;

/// The command's name.
pub(crate) const NAME: &str = "anvil";

/// The command's line in `help`.
pub(crate) const ABOUT: &str =
    "Serve only the WebSocket endpoint, on ANVIL_SERVER_HOST:ANVIL_SERVER_PORT (--host, --port)";

/// The error when no shared PubSub driver is usable.
const NOT_SHARED: &str = "the `anvil` process needs a PubSub driver shared with the app's other processes (events \
                          come from them): set PUBSUB_DRIVER=database (with DATABASE_URL and APP_KEY) or \
                          PUBSUB_DRIVER=redis";

/// The warning when `serve` still serves sockets too.
const ALSO_IN_SERVE: &str = "anvil: ANVIL_IN_SERVE is true, so `serve` processes hold sockets too; set \
                             ANVIL_IN_SERVE=false for the web processes when this process holds them";

/// Run the `anvil` process until Ctrl-C / SIGTERM or the app's shutdown.
pub(crate) async fn run(built: Built, args: Args) -> Result<()> {
    let Built { app, router, .. } = built;
    let Some(anvil) = Anvil::of(&app) else {
        return Err(Error::internal(
            "Anvil is not installed: call `.anvil(...)` in bootstrap/app.rs",
        ));
    };
    let settings = anvil.settings();
    let host = args
        .value("host")
        .unwrap_or(&settings.server_host)
        .to_owned();
    let port = match args.value("port") {
        None => settings.server_port,
        Some(raw) => raw
            .parse::<u16>()
            .map_err(|_| Error::internal(format!("--port must be a port number, not `{raw}`")))?,
    };
    // Before the server starts: the socket endpoint serves here whatever ANVIL_IN_SERVE says.
    anvil.inner.socket_process.store(true, Ordering::Relaxed);
    // No agents, jobs or schedule here: they run in `serve` or `work`.
    app.skip_background();
    let driver = smeltery_core::pubsub::start_as_part(&app).await?;
    if driver == Driver::Local {
        return Err(Error::internal(NOT_SHARED));
    }
    if settings.in_serve {
        tracing::warn!("{ALSO_IN_SERVE}");
    }
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| Error::internal(format!("cannot listen on {addr}: {e}")))?;
    let path: Arc<str> = Arc::from(format!("/app/{}", anvil.app_key()));
    tracing::info!(
        address = %format!("http://{addr}"),
        path = %path,
        driver = driver.name(),
        "anvil: serving the socket endpoint only"
    );
    let router = router.layer(axum::middleware::from_fn(
        move |req: Request, next: Next| {
            let path = Arc::clone(&path);
            async move { only(&path, req, next).await }
        },
    ));
    smeltery_core::serve_on(app, router, listener).await
}

/// Pass the socket endpoint and the health check `/up`; answer 404 to everything else.
async fn only(path: &str, req: Request, next: Next) -> Response {
    let asked = req.uri().path();
    if asked == path || asked == "/up" {
        return next.run(req).await;
    }
    let mut response = crate::endpoint::plain(StatusCode::NOT_FOUND, "Not Found");
    let headers = response.headers_mut();
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    headers.insert(
        http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_have_no_runs_of_spaces() {
        for message in [NOT_SHARED, ALSO_IN_SERVE, ABOUT] {
            assert!(!message.contains("  "), "{message}");
        }
    }
}
