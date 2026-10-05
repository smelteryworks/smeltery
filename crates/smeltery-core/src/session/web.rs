//! The middleware every web route runs inside: load the session, sign in from a remember-me
//! cookie, check the CSRF token, turn validation failures into a redirect back, hand the
//! session's view data to the view layer, and save the session (plus, when enabled, the
//! `XSRF-TOKEN` cookie for Inertia clients).

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use http::{HeaderMap, Method, StatusCode, header};

use super::store::{self, Driver};
use super::{ERRORS_KEY, OLD_KEY, Queued, Session};
use crate::app::App;
use crate::auth::{self, Auth};
use crate::crypto::Keys;
use crate::error::{Error, Result, render_error, wants_json};
use crate::http::Back;
use crate::middleware::{Next, Request};
use crate::validation::InvalidMarker;
use crate::view::ViewData;

/// What the web stack needs, fixed at boot.
#[derive(Clone, Debug)]
pub(crate) struct WebConfig {
    pub(crate) keys: Keys,
    pub(crate) driver: Driver,
}

/// `TestApp::acting_as`: sign this user in for the request.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ActingAs(pub(crate) i64);

/// The session / CSRF / auth middleware of web routes.
pub(crate) async fn web_stack(app: App, req: Request, next: Next) -> Response {
    web_stack_then(app, req, move |req| next.run(req)).await
}

/// The web stack around `inner` (the rest of the request): the one code path for web routes and for first-party
/// requests on API routes ([`crate::session::run_web_stack`], the `auth:` family's stateful branch).
pub(crate) async fn web_stack_then<F, Fut>(app: App, req: Request, inner: F) -> Response
where
    F: FnOnce(Request) -> Fut,
    Fut: std::future::Future<Output = Response>,
{
    let mut response = match run(&app, req, inner).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    };
    add_vary(&mut response, app.web_vary());
    response
}

/// Append each of `names` to `Vary` unless it is listed already.
fn add_vary(response: &mut Response, names: &[&'static str]) {
    for name in names {
        let listed = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|v| v.trim().eq_ignore_ascii_case(name) || v.trim() == "*");
        if !listed && let Ok(value) = http::HeaderValue::from_str(name) {
            response.headers_mut().append(header::VARY, value);
        }
    }
}

/// Every method but the reading ones (`GET`, `HEAD`, `OPTIONS`, `TRACE`) needs the CSRF
/// token: `Router::any` answers extension methods (`PROPFIND`, …) too.
fn changes_state(method: &Method) -> bool {
    !matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// Whether the request is an Inertia visit (`X-Inertia: true`, sent by every Inertia client request).
pub fn is_inertia(headers: &HeaderMap) -> bool {
    headers
        .get("x-inertia")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// The cookie Inertia's client reads and echoes as `X-XSRF-TOKEN` (D-276).
pub(crate) const XSRF_COOKIE: &str = "XSRF-TOKEN";

pub(crate) fn is_json_body(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json") || ct.contains("+json"))
}

async fn run<F, Fut>(app: &App, mut req: Request, next: F) -> Result<Response>
where
    F: FnOnce(Request) -> Fut,
    Fut: std::future::Future<Output = Response>,
{
    let web = app
        .web_config()
        .ok_or_else(|| Error::internal("sessions are not configured (APP_KEY)"))?;
    let jar = store::request_jar(req.headers());
    let session = Session::from_state(store::load(app, &web.keys, web.driver, &jar).await?);
    // The client after TRUSTED_PROXIES, so the login throttle keys on the visitor, not on a
    // proxy every visitor shares.
    let ip = crate::client::of_request(&req)
        .ip()
        .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    let auth = Auth::new(app.clone(), session.clone(), ip);

    let acting_as = req.extensions().get::<ActingAs>().copied();
    if let Some(ActingAs(id)) = acting_as
        && session.auth_id() != Some(id)
    {
        session.insert(super::AUTH_KEY, id);
    }
    // A signed-in session whose user changed their password, signed out (which clears the
    // remember token) or no longer exists is signed out.
    // `TestApp::acting_as` signs in without a password, so it is not checked.
    if acting_as.is_none() && !auth::session_is_current(&auth).await? {
        session.invalidate();
        auth.forget_user();
    }
    if session.auth_id().is_none() && app.user_provider().is_some() {
        let name = auth::remember_cookie(app);
        if let Some(value) = store::decrypt(&jar, &web.keys, &name) {
            match auth::login_from_remember(app, &session, &value).await {
                Ok(true) => {}
                Ok(false) => session.queue(Queued::Remove { name }),
                Err(e) => tracing::warn!(error = %e, "remember-me sign-in failed"),
            }
        } else if jar.get(&name).is_some() {
            // Not ours (tampered, or from an old APP_KEY): drop it.
            session.queue(Queued::Remove { name });
        }
    }

    // The principal of a signed-in session: what `throttle:`, `verified`, `auth:web` and `Authenticated` read.
    if let Some(id) = session.auth_id() {
        let mut principal = auth::Principal::new(
            id,
            auth::WEB_GUARD,
            auth::Credential::session(session.binding()),
        );
        if app.user_provider().is_some() {
            principal = principal.with_loaded_user(auth.auth_user().await?);
        }
        req.extensions_mut().insert(principal);
    }
    req.extensions_mut().insert(session.clone());
    req.extensions_mut().insert(auth.clone());
    // Inertia sends forms as JSON but wants the web answers (a redirect back with the errors
    // flashed, D-275), never 422 / 419 JSON, which would open its error modal.
    let inertia = is_inertia(req.headers());
    let json = wants_json(req.headers()) || (is_json_body(req.headers()) && !inertia);
    let back = Back::for_client(
        req.headers(),
        &crate::client::of_request(&req),
        &app.settings().url,
    );

    let mut form_old = None;
    let mut response = if changes_state(req.method()) {
        // `X-CSRF-TOKEN` (the meta tag, sparks.js), then `X-XSRF-TOKEN` (Inertia's echo of the
        // `XSRF-TOKEN` cookie), then the body's `_token` field (D-276).
        let header_token = ["x-csrf-token", "x-xsrf-token"].iter().find_map(|name| {
            req.headers()
                .get(*name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        });
        let (req, body_token, old) = inspect_body(app, req).await;
        form_old = old;
        let token = header_token.or(body_token);
        let expected = session.token();
        if app.csrf_enabled() && !token.is_some_and(|t| crate::crypto::csrf_matches(&t, &expected))
        {
            if inertia {
                // D-277: the action never runs; the page shows the message after the redirect.
                session.flash("error", "The page expired. Please try again.");
                back.redirect().into_response()
            } else {
                csrf_failure(json)
            }
        } else {
            next(req).await
        }
    } else {
        next(req).await
    };

    if !json && let Some(InvalidMarker(invalid)) = response.extensions().get().cloned() {
        session.flash(ERRORS_KEY, &invalid.errors);
        let old = if invalid.old.is_empty() {
            form_old.unwrap_or_default()
        } else {
            invalid.old
        };
        session.flash(OLD_KEY, old);
        response = back.redirect().into_response();
    }
    if response.extensions().get::<ViewData>().is_none() {
        let page = crate::view::is_view(&response);
        let mut data = session.view_data(page);
        data.set_web(session.clone(), auth);
        response.extensions_mut().insert(data);
    }
    if let Some(reason) = session.with_state(|s| s.failed.clone()) {
        return Err(Error::internal(reason));
    }
    // Before saving: a session without a token gets one now, and it must be stored.
    let xsrf = if app.xsrf_cookie() {
        Some(session.csrf_token()?)
    } else {
        None
    };
    session.age_flash();
    store::save(app, &web.keys, web.driver, &session, response.headers_mut()).await?;
    if let Some(token) = xsrf {
        store::set_xsrf_cookie(app, token, response.headers_mut())?;
    }
    Ok(response)
}

/// 419 "Page Expired", or `{"error": "CSRF token mismatch"}` for JSON clients.
fn csrf_failure(json: bool) -> Response {
    let status = StatusCode::from_u16(419).unwrap_or(StatusCode::FORBIDDEN);
    if json {
        return (
            status,
            axum::Json(serde_json::json!({ "error": "CSRF token mismatch" })),
        )
            .into_response();
    }
    render_error(status, "Page Expired", None, false)
}

/// Read a form body for its `_token` field and, for URL-encoded forms, the input to flash
/// back; the handler gets the same bytes. A URL-encoded body is read whole (up to `limit`); a
/// multipart body only until its `_token` field (at most `limit` bytes), so uploads stream on.
async fn inspect_body(
    app: &App,
    req: Request,
) -> (Request, Option<String>, Option<crate::validation::Input>) {
    let limit = app.settings().body_limit;
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if let Some(boundary) = crate::multipart::boundary(&content_type) {
        let (parts, body) = req.into_parts();
        let (body, found) =
            crate::multipart::peek_fields(body, &boundary, &["_token"], limit).await;
        let token = found.into_iter().next().map(|(_, v)| v);
        return (Request::from_parts(parts, body), token, None);
    }
    if !content_type.starts_with("application/x-www-form-urlencoded") {
        return (req, None, None);
    }
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        // Too large: the handler's own extractor reports it.
        return (Request::from_parts(parts, Body::empty()), None, None);
    };
    let token = form_urlencoded::parse(&bytes)
        .find(|(k, _)| k == "_token")
        .map(|(_, v)| v.into_owned());
    let old = Some(crate::validation::old_from_form(app, &bytes));
    (Request::from_parts(parts, Body::from(bytes)), token, old)
}
