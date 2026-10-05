//! The JSON API, the SSE stream and the dashboard under `/_watchfire`, and who may use them.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, Mac};
use http::request::Parts;
use http::{StatusCode, header};
use sha2::Sha256;
use smeltery_core::App;
use smeltery_core::auth::Auth;
use smeltery_core::config::loopback_url;
use smeltery_core::http::ClientInfo;

use crate::app::WatchfireSettings;

pub(crate) mod api;
pub(crate) mod dashboard;
pub(crate) mod live;

/// Where the dashboard lives; the API is under `/_watchfire/api`.
pub const PREFIX: &str = "/_watchfire";
/// The API prefix.
pub const API_PREFIX: &str = "/_watchfire/api";

/// Who may use the dashboard, and the API without its token: `WATCHFIRE_DASHBOARD`.
///
/// The dashboard (page, buttons, live panels) admits a signed-in user whom the app's
/// [`dashboard_gate`](crate::Watchfire::dashboard_gate) lets in; without a gate nobody. The only way in without
/// signing in is local development: `local` mode, `APP_ENV=local`, an `APP_URL` on this machine, and a request from
/// a loopback address that did not come through a `TRUSTED_PROXIES` proxy and carries none of the 13 reverse-proxy and
/// CDN headers Watchfire checks (`Forwarded`, `X-Forwarded-For|Host|Proto|Port|Server`, `X-Real-IP`,
/// `X-Original-Forwarded-For`, `X-Client-IP`, `Via`, `CF-Connecting-IP`, `True-Client-IP`, `Fastly-Client-IP`). The
/// request must also be addressed to this machine (a `Host` of `localhost`, `*.localhost`, a loopback address or the
/// `APP_URL` host: another name is a DNS-rebinding page) and must not come from another site (an `Origin` other than
/// the app's own, host and port; more than one `Origin`; or a `Sec-Fetch-Site` other than `same-origin` / `none`; a
/// top-level `GET` navigation from a link stays allowed). That same rule lets such a request call the API without the token; every other API request needs
/// the token.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Access {
    /// Signed-in users the gate admits, and local development without sign-in (default).
    #[default]
    Local,
    /// Signed-in users the gate admits, also in local development.
    Auth,
    /// No dashboard; the API answers only requests with the token.
    Off,
}

impl Access {
    /// `local`, `auth` or `off` (anything else: `local`, with a warning).
    pub fn parse(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "local" | "" => Self::Local,
            "auth" => Self::Auth,
            "off" | "none" | "false" => Self::Off,
            other => {
                tracing::warn!(
                    value = other,
                    "WATCHFIRE_DASHBOARD must be local, auth or off; using local"
                );
                Self::Local
            }
        }
    }
}

/// The app's dashboard gate (`Watchfire::dashboard_gate`).
pub(crate) type GateFn = Arc<
    dyn Fn(Auth, App) -> smeltery_core::BoxFuture<'static, smeltery_core::Result<bool>>
        + Send
        + Sync,
>;

/// The gate as a service of the app (registered at boot, also when Watchfire does not run in the process).
#[derive(Clone, Default)]
pub(crate) struct DashboardGate(pub(crate) Option<GateFn>);

/// The session key that marks a browser admitted by the local-development rule (the live panels check it, since
/// their update requests do not show the request headers).
pub(crate) const LOCAL_MARK: &str = "_watchfire_local";

/// Headers reverse proxies and CDNs add: a request with one came through a proxy, whatever its peer address.
pub(crate) const PROXY_HEADERS: [&str; 13] = [
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-port",
    "x-forwarded-server",
    "x-real-ip",
    "x-original-forwarded-for",
    "x-client-ip",
    "via",
    "cf-connecting-ip",
    "true-client-ip",
    "fastly-client-ip",
];

fn proxied(headers: &http::HeaderMap) -> bool {
    PROXY_HEADERS.iter().any(|h| headers.contains_key(*h))
}

/// Local development: `local` mode, `APP_ENV=local` and an `APP_URL` on this machine (a server sets its public
/// URL there for mail links and secure cookies, so a forgotten `APP_ENV` behind a proxy does not open the door).
pub(crate) fn local_mode(app: &App) -> bool {
    settings(app).dashboard == Access::Local
        && app.settings().env == "local"
        && loopback_url(&app.settings().url)
}

/// The local-development rule for a request: local mode, a loopback client not behind a trusted proxy, no proxy
/// header, addressed to this machine and not sent by another site.
fn local_request(parts: &Parts, app: &App) -> bool {
    local_mode(app)
        && from_loopback(parts, app)
        && !proxied(&parts.headers)
        && local_host(parts, app)
        && !cross_site(parts, app)
}

/// Whether `host` (no port, brackets allowed) names this machine: `localhost`, `*.localhost`, a loopback address, or
/// the host of `APP_URL`.
fn host_is_local(host: &str, app: &App) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return false;
    }
    if loopback_url(&format!("http://{}", bracket(host))) {
        return true;
    }
    app.settings()
        .url
        .trim()
        .parse::<http::Uri>()
        .ok()
        .and_then(|uri| {
            uri.host()
                .map(|h| h.trim_start_matches('[').trim_end_matches(']').to_owned())
        })
        .is_some_and(|app_host| app_host.eq_ignore_ascii_case(host))
}

fn bracket(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

/// The request's own authority: the `Host` header, or the URI authority (absolute-form targets, HTTP/2). `Err` when
/// both are present and disagree (an absolute-form target naming this machine with a foreign `Host`); `Ok(None)` when
/// there is none (HTTP/1.0).
fn request_authority(parts: &Parts) -> Result<Option<String>, ()> {
    let from_uri = parts
        .uri
        .authority()
        .map(|a| a.as_str().to_ascii_lowercase());
    let from_header = match parts.headers.get(header::HOST) {
        Some(v) => Some(v.to_str().map_err(|_| ())?.trim().to_ascii_lowercase()),
        None => None,
    };
    match (from_uri, from_header) {
        (Some(a), Some(b)) if a != b => Err(()),
        (a, b) => Ok(b.or(a)),
    }
}

/// `(host, port)` of an `http(s)` URL or of `scheme://authority`; the port defaults by scheme.
fn host_port(url: &str) -> Option<(String, u16)> {
    let uri = url.trim().parse::<http::Uri>().ok()?;
    let default = match uri.scheme_str()? {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let host = uri.host()?.trim_start_matches('[').trim_end_matches(']');
    Some((host.to_ascii_lowercase(), uri.port_u16().unwrap_or(default)))
}

/// The request names this machine as its host. A page on an attacker's domain that resolves to 127.0.0.1 (DNS
/// rebinding) is same-origin to the browser, but its requests carry the attacker's host. A request without any host
/// (HTTP/1.0) has none to check.
fn local_host(parts: &Parts, app: &App) -> bool {
    let Ok(authority) = request_authority(parts) else {
        return false;
    };
    let Some(authority) = authority else {
        return true;
    };
    match format!("http://{authority}/").parse::<http::Uri>() {
        Ok(uri) => uri.host().is_some_and(|h| host_is_local(h, app)),
        Err(_) => false,
    }
}

/// The browser says another site sent the request: more than one `Origin`, an `Origin` that is not this app's own
/// origin (host AND port of the request's `Host`, or of `APP_URL`; `null` included: another dev server on
/// `localhost:3000` is another site), or a `Sec-Fetch-Site` other than `same-origin` / `none`, except for a top-level
/// `GET`/`HEAD` navigation (a link the developer followed, which sends no `Origin`): the page it opens is shown to the
/// developer, not to the other site. Clients without these headers (curl, the console commands) are not browsers.
fn cross_site(parts: &Parts, app: &App) -> bool {
    let safe = parts.method == http::Method::GET || parts.method == http::Method::HEAD;
    let navigation = safe
        && parts
            .headers
            .get("sec-fetch-mode")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("navigate"));
    let mut origins = parts.headers.get_all(header::ORIGIN).iter();
    if let Some(origin) = origins.next() {
        if origins.next().is_some() {
            return true;
        }
        let Some((host, port)) = origin.to_str().ok().and_then(host_port) else {
            return true;
        };
        let scheme_port =
            |authority: &str, scheme: &str| host_port(&format!("{scheme}://{authority}"));
        let own = request_authority(parts).ok().flatten();
        let same_as_request = own.as_deref().is_some_and(|a| {
            // The request's own scheme is not known here: either default port may apply to a bare host.
            scheme_port(a, "http") == Some((host.clone(), port))
                || scheme_port(a, "https") == Some((host.clone(), port))
        });
        let same_as_app = host_port(&app.settings().url) == Some((host.clone(), port));
        if !(host_is_local(&host, app) && (same_as_request || same_as_app)) {
            return true;
        }
    }
    if let Some(site) = parts.headers.get("sec-fetch-site") {
        let site = site.to_str().unwrap_or_default().to_ascii_lowercase();
        if site != "same-origin" && site != "none" && !navigation {
            return true;
        }
    }
    false
}

/// Ask the app's gate about a signed-in user: `Ok(false)` without a gate.
pub(crate) async fn gate_allows(app: &App, auth: &Auth) -> smeltery_core::Result<bool> {
    let gate = app.service::<DashboardGate>().and_then(|g| g.0.clone());
    match gate {
        Some(gate) => gate(auth.clone(), app.clone()).await,
        None => Ok(false),
    }
}

fn wants_json(headers: &http::HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/json") || a.contains("+json"))
}

/// The API token for an app key: hex HMAC-SHA256(APP_KEY, "watchfire-api"). `None` without a
/// usable `APP_KEY`.
///
/// ```
/// let token = smeltery_watchfire::web::api_token("0123456789abcdef0123456789abcdef").unwrap();
/// assert_eq!(token.len(), 64);
/// assert!(smeltery_watchfire::web::api_token("short").is_none());
/// ```
pub fn api_token(app_key: &str) -> Option<String> {
    // Core's key rule: the token stays derivable from the key text alone.
    let key = smeltery_core::config::app_key_bytes(app_key)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).ok()?;
    mac.update(b"watchfire-api");
    let digest = mac.finalize().into_bytes();
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// The running app's API base URL, where the console commands send the token: `WATCHFIRE_API_ADDR` (an address or a
/// URL), else `SERVER_HOST:SERVER_PORT` (`0.0.0.0` and `::` mean this machine).
///
/// # Errors
/// The URL is plain `http://` to an address that is not this machine (`localhost`, `*.localhost` or a loopback
/// address): the token would cross the network unencrypted. Use a loopback `WATCHFIRE_API_ADDR` or an `https://`
/// URL.
pub fn api_base_url(app: &App) -> Result<String, crate::Error> {
    let settings = settings(app);
    let addr = settings.api_addr.unwrap_or_else(|| {
        let s = app.settings();
        let host = match s.host.as_str() {
            "0.0.0.0" | "" => "127.0.0.1",
            "::" => "[::1]",
            other => other,
        };
        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{}", s.port)
        } else {
            format!("{host}:{}", s.port)
        }
    });
    let addr = addr.trim().trim_end_matches('/');
    let url = if addr.starts_with("http://") || addr.starts_with("https://") {
        format!("{addr}{API_PREFIX}")
    } else {
        format!("http://{addr}{API_PREFIX}")
    };
    if url.starts_with("http://") && !loopback_url(&url) {
        return Err(crate::Error::Config(format!(
            "refusing to send the Watchfire API token over plain http to {url}: set WATCHFIRE_API_ADDR to a \
             loopback address (e.g. 127.0.0.1:8001) or an https:// URL"
        )));
    }
    Ok(url)
}

fn has_valid_bearer(parts: &Parts, app: &App) -> bool {
    let Some(expected) = api_token(&app.settings().key) else {
        return false;
    };
    let Some(given) = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    use subtle::ConstantTimeEq as _;
    given.trim().as_bytes().ct_eq(expected.as_bytes()).into()
}

/// The request's client as the HTTP stack resolved it (`TRUSTED_PROXIES`), or, in the API router `work` serves
/// itself, resolved here the same way.
fn client(parts: &Parts, app: &App) -> ClientInfo {
    if let Some(found) = parts.extensions.get::<ClientInfo>() {
        return found.clone();
    }
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    ClientInfo::resolve(app.trusted_proxies(), peer, &parts.headers, &parts.uri)
}

/// A request straight from this machine: a loopback client that did not come through a (trusted) proxy. A same-host
/// proxy that is not in `TRUSTED_PROXIES` also looks like loopback, which is why the local rule also refuses
/// forwarding headers and holds only under `APP_ENV=local`.
fn from_loopback(parts: &Parts, app: &App) -> bool {
    let client = client(parts, app);
    !client.is_proxied() && client.ip().is_some_and(|ip| ip.is_loopback())
}

/// The app's Watchfire settings (registered at boot), or read now.
pub(crate) fn settings(app: &App) -> WatchfireSettings {
    app.service::<WatchfireSettings>()
        .map_or_else(WatchfireSettings::from_env, |s| (*s).clone())
}

/// A JSON error answer: `{"error": "…"}`.
pub(crate) fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        axum::Json(serde_json::json!({ "error": message.into() })),
    )
        .into_response()
}

/// Admission to the API: a valid bearer token, or a request under the local-development rule
/// ([`Access`]).
pub(crate) struct ApiAccess;

impl FromRequestParts<App> for ApiAccess {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self, Self::Rejection> {
        if has_valid_bearer(parts, app) {
            return Ok(Self);
        }
        if local_request(parts, app) {
            return Ok(Self);
        }
        if parts.headers.contains_key(header::AUTHORIZATION) {
            return Err(json_error(StatusCode::UNAUTHORIZED, "invalid API token"));
        }
        Err(json_error(
            StatusCode::UNAUTHORIZED,
            "send Authorization: Bearer <token> (the Watchfire API token of this app)",
        ))
    }
}

/// Admission to the dashboard ([`Access`]): `off` answers 404, a guest is sent to the `login` route (401 JSON for
/// JSON clients), a signed-in user the gate does not admit gets 403.
pub(crate) struct DashboardAccess;

impl FromRequestParts<App> for DashboardAccess {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self, Self::Rejection> {
        if settings(app).dashboard == Access::Off {
            return Err(smeltery_core::Error::not_found().into_response());
        }
        // The live panels' update requests show no request headers; they check this session mark, which only a
        // request the local rule admitted sets, and any other dashboard request removes.
        let local = local_request(parts, app);
        if let Some(session) = parts.extensions.get::<smeltery_core::session::Session>() {
            if local {
                session.insert(LOCAL_MARK, true);
            } else if session.get::<bool>(LOCAL_MARK).is_some() {
                session.remove(LOCAL_MARK);
            }
        }
        if local {
            return Ok(Self);
        }
        let auth = parts
            .extensions
            .get::<Auth>()
            .filter(|a| a.check())
            .cloned();
        let Some(auth) = auth else {
            if wants_json(&parts.headers) {
                return Err(json_error(StatusCode::UNAUTHORIZED, "Unauthenticated."));
            }
            let login = app
                .url("login", &[])
                .unwrap_or_else(|_| "/login".to_owned());
            return Err(axum::response::Redirect::to(&login).into_response());
        };
        match gate_allows(app, &auth).await {
            Ok(true) => Ok(Self),
            Ok(false) => Err(smeltery_core::Error::forbidden().into_response()),
            Err(e) => Err(e.into_response()),
        }
    }
}

/// Register the routes: the API without sessions (`/_watchfire/api/…`), the dashboard and
/// its forms as web routes (sessions, CSRF), and the dashboard's stylesheet without sessions
/// (`/_watchfire/assets/watchfire.css`). The dashboard needs sessions, which need a usable
/// `APP_KEY`: without one (or with a malformed or short one; the public test key only under
/// `APP_ENV=testing` with an empty `APP_KEY`) it is left out, so an app without web routes still
/// starts without a key. The API token is never derived from the test key: under it only the
/// local rule admits API requests.
pub(crate) fn mount(builder: smeltery_core::AppBuilder) -> smeltery_core::AppBuilder {
    let settings = builder.settings();
    // Core's rule (D-344): a usable `APP_KEY`, or the public test key, which needs `APP_ENV=testing` AND an empty
    // `APP_KEY`; a malformed or short key is never replaced by it.
    let sessions = settings.has_signing_key();
    let builder = builder.api_routes_at(API_PREFIX, api::routes);
    if sessions {
        builder
            .routes(dashboard::routes)
            .api_routes_at(dashboard::ASSETS_PREFIX, dashboard::asset_routes)
    } else {
        tracing::debug!("no APP_KEY: the Watchfire dashboard is not mounted");
        builder
    }
}

/// Limits of the API server `work` runs itself (`WATCHFIRE_API_ADDR`): it has none of the app's HTTP middleware.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ApiServerLimits {
    /// How long a client may take to send a request's headers (also the idle keep-alive limit).
    pub(crate) header_timeout: std::time::Duration,
    /// Connections served at once; more wait in the listen backlog.
    pub(crate) max_connections: usize,
    /// How long open connections (an SSE stream) may take to end after shutdown.
    pub(crate) drain: std::time::Duration,
}

impl Default for ApiServerLimits {
    fn default() -> Self {
        Self {
            header_timeout: std::time::Duration::from_secs(10),
            max_connections: 64,
            drain: std::time::Duration::from_secs(5),
        }
    }
}

/// Serve the API alone on `addr` (headless `work` with `WATCHFIRE_API_ADDR`), until `shutdown`.
///
/// # Errors
/// The address cannot be bound.
pub(crate) async fn serve_api(
    app: App,
    addr: &str,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<impl Future<Output = ()> + Send + 'static, crate::Error> {
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        crate::Error::Config(format!("cannot listen on {addr} (WATCHFIRE_API_ADDR): {e}"))
    })?;
    tracing::info!(address = %format!("http://{addr}{API_PREFIX}"), "Watchfire API listening");
    Ok(serve_api_on(
        app,
        listener,
        shutdown,
        ApiServerLimits::default(),
    ))
}

/// The API server on a bound listener: HTTP/1.1 with a header-read timeout (slow or idle clients are cut off), at
/// most `max_connections` connections at once, `X-Content-Type-Options: nosniff` on every answer, and a graceful
/// end on `shutdown` (open connections get `drain` to finish).
pub(crate) async fn serve_api_on(
    app: App,
    listener: tokio::net::TcpListener,
    shutdown: tokio_util::sync::CancellationToken,
    limits: ApiServerLimits,
) {
    use tower::ServiceExt as _;
    let router = axum::Router::new()
        .nest(API_PREFIX, api::axum_router())
        .with_state(app);
    let permits = Arc::new(tokio::sync::Semaphore::new(limits.max_connections.max(1)));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        // A permit first: past the cap, connections wait in the backlog instead of holding a task each.
        let permit = tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            permit = Arc::clone(&permits).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => break,
            },
        };
        let (stream, peer) = tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) => {
                    // Out of file descriptors and the like: wait a moment instead of spinning.
                    tracing::warn!(error = %e, "the Watchfire API server cannot accept a connection");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        // Finished connections leave the set.
        while connections.try_join_next().is_some() {}
        let router = router.clone();
        let token = shutdown.clone();
        connections.spawn(async move {
            let _permit = permit;
            let service = hyper::service::service_fn(
                move |mut request: http::Request<hyper::body::Incoming>| {
                    request.extensions_mut().insert(ConnectInfo(peer));
                    let router = router.clone();
                    async move {
                        let mut response = router
                            .oneshot(request.map(axum::body::Body::new))
                            .await
                            .unwrap_or_else(|never| match never {});
                        response.headers_mut().insert(
                            header::X_CONTENT_TYPE_OPTIONS,
                            http::HeaderValue::from_static("nosniff"),
                        );
                        Ok::<_, std::convert::Infallible>(response)
                    }
                },
            );
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(limits.header_timeout);
            let connection =
                builder.serve_connection(hyper_util::rt::TokioIo::new(stream), service);
            tokio::pin!(connection);
            let served = tokio::select! {
                served = connection.as_mut() => served,
                () = token.cancelled() => {
                    connection.as_mut().graceful_shutdown();
                    connection.await
                }
            };
            if let Err(e) = served {
                tracing::debug!(error = %e, "a Watchfire API connection ended with an error");
            }
        });
    }
    drop(listener);
    let drained = tokio::time::timeout(limits.drain, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        tracing::warn!("Watchfire API connections still open after shutdown; closing them");
        connections.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_access_values() {
        let a = api_token("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(a, api_token("0123456789abcdef0123456789abcdef").unwrap());
        assert_ne!(a, api_token("0123456789abcdef0123456789abcdeX").unwrap());
        // base64: keys are decoded.
        let b64 = "base64:MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
        assert_eq!(api_token(b64).unwrap(), a);
        assert!(api_token("").is_none());
        assert_eq!(Access::parse("AUTH"), Access::Auth);
        assert_eq!(Access::parse("off"), Access::Off);
        assert_eq!(Access::parse("bogus"), Access::Local);
        for url in [
            "http://127.0.0.1:8000",
            "http://localhost:3000",
            "http://app.localhost",
            "http://[::1]:8000",
            "http://127.0.0.2",
        ] {
            assert!(loopback_url(url), "{url}");
        }
        for url in [
            "https://example.com",
            "http://10.0.0.5:8000",
            "http://localhost.example.com",
            "",
            "not a url",
        ] {
            assert!(!loopback_url(url), "{url}");
        }
    }

    async fn api_server(
        limits: ApiServerLimits,
    ) -> (
        SocketAddr,
        tokio_util::sync::CancellationToken,
        tokio::task::JoinHandle<()>,
    ) {
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.key = "0123456789abcdef0123456789abcdef".to_owned();
        let app = smeltery_core::AppBuilder::new(settings)
            .build()
            .await
            .unwrap()
            .app;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        let server = tokio::spawn(serve_api_on(app, listener, token.clone(), limits));
        (addr, token, server)
    }

    async fn read_some(
        stream: &mut tokio::net::TcpStream,
        within: std::time::Duration,
    ) -> Option<String> {
        use tokio::io::AsyncReadExt as _;
        let mut buf = vec![0_u8; 4096];
        match tokio::time::timeout(within, stream.read(&mut buf)).await {
            Ok(Ok(n)) => Some(String::from_utf8_lossy(&buf[..n]).into_owned()),
            Ok(Err(_)) => Some(String::new()),
            Err(_) => None,
        }
    }

    /// S4-10: a client that never finishes its headers (slowloris) is cut off; answers carry `nosniff`.
    #[tokio::test]
    async fn the_api_server_cuts_off_slow_clients() {
        use tokio::io::AsyncWriteExt as _;
        let limits = ApiServerLimits {
            header_timeout: std::time::Duration::from_millis(300),
            ..ApiServerLimits::default()
        };
        let (addr, token, server) = api_server(limits).await;
        let mut slow = tokio::net::TcpStream::connect(addr).await.unwrap();
        slow.write_all(b"GET /_watchfire/api/agents HTTP/1.1\r\nHost: 127.0.0.1\r\n")
            .await
            .unwrap();
        // The server closes it (an empty read, possibly after a 408) well before 5 s.
        let closed = read_some(&mut slow, std::time::Duration::from_secs(5)).await;
        assert!(closed.is_some(), "the slow connection stayed open");
        // A normal request is answered, with nosniff.
        let mut ok = tokio::net::TcpStream::connect(addr).await.unwrap();
        ok.write_all(b"GET /_watchfire/api/agents HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let answer = read_some(&mut ok, std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
        assert!(
            answer
                .to_ascii_lowercase()
                .contains("x-content-type-options: nosniff"),
            "{answer}"
        );
        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap();
    }

    /// S4-10: at most `max_connections` connections are served; the next waits until one closes.
    #[tokio::test]
    async fn the_api_server_caps_its_connections() {
        use tokio::io::AsyncWriteExt as _;
        let limits = ApiServerLimits {
            max_connections: 2,
            ..ApiServerLimits::default()
        };
        let (addr, token, server) = api_server(limits).await;
        let first = tokio::net::TcpStream::connect(addr).await.unwrap();
        let second = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut third = tokio::net::TcpStream::connect(addr).await.unwrap();
        third
            .write_all(b"GET /_watchfire/api/agents HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        assert_eq!(
            read_some(&mut third, std::time::Duration::from_millis(500)).await,
            None,
            "a third connection was served"
        );
        drop(first);
        let answer = read_some(&mut third, std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
        drop(second);
        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap();
    }
}
