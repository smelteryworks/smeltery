//! The auth endpoints: `POST /broadcasting/auth`, a web route (session and CSRF), and `POST /api/broadcasting/auth`,
//! an API route for the bearer credentials of a stateless guard. Both sign a private subscription when the channel's
//! callback allows it (D-413, D-415).

use axum::body::Bytes;
use http::header::{CACHE_CONTROL, CONTENT_TYPE, WWW_AUTHENTICATE};
use http::request::Parts;
use http::{HeaderMap, HeaderValue, StatusCode};
use serde::Deserialize;
use smeltery_core::auth::{Auth, Authenticated, GuardSet, authenticate};
use smeltery_core::{App, Response};

use crate::Anvil;
use crate::channels::{Asker, Authorizer, ChannelCtx};
use crate::protocol::{Kind, valid_channel, valid_socket_id};
use crate::signature::{self, Credential, GRANT_LIFETIME, Grant};

/// The request: form fields or JSON, as Pusher clients send them.
#[derive(Debug, Deserialize)]
struct AuthRequest {
    socket_id: String,
    channel_name: String,
}

fn json(status: StatusCode, body: &serde_json::Value) -> Response {
    let mut response = Response::new(axum::body::Body::from(body.to_string()));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The one answer for "no such channel", "not signed in" and "not allowed": the endpoint never tells which
/// channels exist.
fn forbidden() -> Response {
    json(
        StatusCode::FORBIDDEN,
        &serde_json::json!({ "error": "Forbidden" }),
    )
}

fn server_error_body() -> serde_json::Value {
    serde_json::json!({ "error": "Server Error" })
}

fn server_error() -> Response {
    json(StatusCode::INTERNAL_SERVER_ERROR, &server_error_body())
}

fn bad_request(message: &str) -> Response {
    json(
        StatusCode::BAD_REQUEST,
        &serde_json::json!({ "error": message }),
    )
}

fn parse(headers: &HeaderMap, body: &[u8]) -> Option<AuthRequest> {
    let json = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.trim_start()
                .to_ascii_lowercase()
                .starts_with("application/json")
        });
    if json {
        serde_json::from_slice(body).ok()
    } else {
        let mut socket_id = None;
        let mut channel_name = None;
        for (key, value) in form_urlencoded::parse(body) {
            match key.as_ref() {
                "socket_id" => socket_id = Some(value.into_owned()),
                "channel_name" => channel_name = Some(value.into_owned()),
                _ => {}
            }
        }
        Some(AuthRequest {
            socket_id: socket_id?,
            channel_name: channel_name?,
        })
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `POST /broadcasting/auth` (web route: the session and its CSRF check).
pub(crate) async fn authorize(
    app: App,
    auth: Auth,
    who: Option<Authenticated>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let user = auth.id();
    // The signed-in session's key, as core's auth events name it at logout (`web:session:<binding>`).
    let credential = who.and_then(|who| Credential::from_key(&who.key()));
    sign_for(
        &app,
        Asker::Session(auth),
        user,
        credential,
        None,
        &headers,
        &body,
    )
    .await
}

/// The ability a token needs for `/api/broadcasting/auth` (or `*`).
pub(crate) const ABILITY: &str = "broadcasting";

/// `POST /api/broadcasting/auth` (API route: a stateless guard's bearer credential, no session, no cookie).
pub(crate) async fn authorize_token(app: App, mut parts: Parts, body: Bytes) -> Response {
    if !app.has_stateless_guard() {
        return json(
            StatusCode::NOT_FOUND,
            &serde_json::json!({ "error": "Not Found" }),
        );
    }
    let principal = match authenticate(&app, &mut parts, GuardSet::Stateless).await {
        Ok(Some(principal)) => principal,
        Ok(None) => {
            let mut response = json(
                StatusCode::UNAUTHORIZED,
                &serde_json::json!({ "error": "Unauthenticated." }),
            );
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return response;
        }
        // A guard's client error is an expected refusal (a credential guesser over its budget: 429), answered with
        // its status and logged quietly; only a server error is a failure.
        Err(error) if !error.status().is_server_error() => {
            tracing::debug!(
                status = error.status().as_u16(),
                "anvil: a guard refused the token auth request"
            );
            let status = error.status();
            let reason = status.canonical_reason().unwrap_or("Refused");
            return json(status, &serde_json::json!({ "error": reason }));
        }
        Err(error) => {
            tracing::error!(error = %error, "anvil: a guard failed on the token auth endpoint");
            return json(error.status(), &server_error_body());
        }
    };
    if !principal.can(ABILITY) {
        return forbidden();
    }
    let user = Some(principal.user_id);
    let credential = Credential::from_key(&principal.key());
    // A grant never outlives its credential.
    let until = principal
        .expires_at
        .map(|at| u64::try_from(at.timestamp()).unwrap_or(0));
    sign_for(
        &app,
        Asker::Principal(principal),
        user,
        credential,
        until,
        &parts.headers,
        &body,
    )
    .await
}

/// The flow both endpoints share: the request's checks, the pattern, the callback, the signed grant.
async fn sign_for(
    app: &App,
    asker: Asker,
    user: Option<i64>,
    credential: Option<Credential>,
    until: Option<u64>,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    if user.is_some() && credential.is_none() {
        // A grant that names no credential could never be ended by a revocation: never signed.
        tracing::error!(
            "anvil: the signed-in user's credential has no key a revocation can name; no grant is signed"
        );
        return server_error();
    }
    let Some(anvil) = Anvil::of(app) else {
        return forbidden();
    };
    let Some(request) = parse(headers, body) else {
        return bad_request("socket_id and channel_name are required");
    };
    if !valid_socket_id(&request.socket_id) {
        return bad_request("invalid socket_id");
    }
    if !valid_channel(&request.channel_name) {
        return bad_request("invalid channel_name");
    }
    let (kind, bare) = Kind::of(&request.channel_name);
    match kind {
        Kind::Public => return bad_request("public channels need no authorization"),
        Kind::Unsupported => return forbidden(),
        Kind::Private | Kind::Presence => {}
    }
    let Some((authorizer, params, guests)) = anvil.inner.channels.authorizer(kind, bare) else {
        return forbidden();
    };
    if user.is_none() && !guests {
        return forbidden();
    }
    let ctx = ChannelCtx::new(
        app.clone(),
        request.channel_name.clone(),
        request.socket_id.clone(),
        params,
        asker,
    );
    let decided = match authorizer {
        Authorizer::Private(callback) => callback(ctx).await.map(|allowed| allowed.then_some(None)),
        Authorizer::Presence(callback) => callback(ctx).await.map(|member| member.map(Some)),
    };
    // `Some(None)`: a private channel allowed; `Some(Some(member))`: a presence channel joined as `member`.
    let member = match decided {
        Ok(Some(member)) => member,
        Ok(None) => return forbidden(),
        // A client error from the callback (a parameter that does not parse, a record not found) is a denial.
        Err(error) if !error.status().is_server_error() => return forbidden(),
        Err(error) => {
            tracing::error!(error = %error, "anvil: a channel authorization callback failed");
            return json(error.status(), &server_error_body());
        }
    };
    let channel_data = match &member {
        None => None,
        Some(member) => {
            let data = member.channel_data();
            let max = anvil.inner.settings.max_member_bytes;
            if !member.valid_id() || data.len() > max {
                // The app's callback named a member no client could join as: its mistake, never a grant.
                tracing::error!(
                    max,
                    "anvil: a presence callback returned a member whose user id is empty or longer than 128 bytes, \
                     or whose channel data is larger than ANVIL_MAX_MEMBER_BYTES"
                );
                return server_error();
            }
            Some(data)
        }
    };
    let Some(config) = anvil.inner.session.get() else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            &serde_json::json!({ "error": "Service Unavailable" }),
        );
    };
    let now = unix_now();
    let mut expires = now.saturating_add(GRANT_LIFETIME.as_secs());
    if let Some(until) = until {
        if until <= now {
            return forbidden();
        }
        expires = expires.min(until);
    }
    let grant = Grant {
        user,
        credential,
        issued: now,
        expires,
    };
    let auth = signature::authorize(
        &config.app_key,
        &config.secret,
        &request.socket_id,
        &request.channel_name,
        &grant,
        channel_data.as_deref(),
    );
    match channel_data {
        Some(data) => json(
            StatusCode::OK,
            &serde_json::json!({ "auth": auth, "channel_data": data }),
        ),
        None => json(StatusCode::OK, &serde_json::json!({ "auth": auth })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_parse_from_forms_and_json() {
        let mut headers = HeaderMap::new();
        let form = parse(&headers, b"socket_id=1.2&channel_name=private-a").unwrap();
        assert_eq!(
            (form.socket_id.as_str(), form.channel_name.as_str()),
            ("1.2", "private-a")
        );
        assert!(parse(&headers, b"socket_id=1.2").is_none());
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        let json = parse(
            &headers,
            br#"{"socket_id":"1.2","channel_name":"private-a"}"#,
        )
        .unwrap();
        assert_eq!(json.channel_name, "private-a");
        assert!(parse(&headers, b"socket_id=1.2&channel_name=private-a").is_none());
    }
}
