//! The `hallmark` guard: bearer tokens in the `Authorization` header, the guess budget, `last_used_at`, and the
//! response headers of requests the guard looked at.

use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::response::Response;
use http::request::Parts;
use http::{HeaderMap, HeaderValue, StatusCode, header};
use sea_orm::prelude::ChronoUtc;
use smeltery_core::auth::{Credential, Guard, Principal};
use smeltery_core::cache::RateLimit;
use smeltery_core::crypto::{constant_time_eq, sha256_hex};
use smeltery_core::http::ClientInfo;
use smeltery_core::middleware::{Next, Request};
use smeltery_core::{App, BoxFuture, Error, Result};

use crate::extract::TokenMeta;
use crate::tokens::{BINDING_PURPOSE, Tokens, well_formed};

/// The guard's name: routes use it as `auth:hallmark`.
pub const GUARD: &str = "hallmark";

/// The guess budget's window.
pub(crate) const GUESS_WINDOW: Duration = Duration::from_secs(60);

/// The guard, registered by [`HallmarkExt::hallmark`](crate::HallmarkExt::hallmark).
pub(crate) struct HallmarkGuard;

impl Guard for HallmarkGuard {
    fn name(&self) -> &'static str {
        GUARD
    }

    fn stateless(&self) -> bool {
        true
    }

    fn authenticate<'a>(
        &'a self,
        app: &'a App,
        parts: &'a mut Parts,
    ) -> BoxFuture<'a, Result<Option<Principal>>> {
        Box::pin(authenticate(app, parts))
    }

    /// SPA mode only, and only for browser requests: `Sec-Fetch-Site: same-origin`, or (without that header) an
    /// `Origin` / `Referer` origin that is first-party. A request without browser signals (a bearer call from an
    /// app or a server) is never first-party, so the web stack never runs for it (no session stored, no cookie set).
    fn first_party(&self, app: &App, parts: &Parts) -> bool {
        app.service::<crate::State>()
            .and_then(|state| state.first_party.get().map(|fp| fp.allows(&parts.headers)))
            .unwrap_or(false)
    }
}

/// What the guard noticed about one request, for [`response_headers`].
#[derive(Default)]
struct Outcome {
    /// The guard ran.
    consulted: bool,
    /// The request was refused for too many guesses: seconds until the budget refills.
    retry_after: Option<u64>,
}

/// The slot [`response_headers`] puts in every request.
#[derive(Clone, Default)]
pub(crate) struct Seen(Arc<Mutex<Outcome>>);

impl Seen {
    fn update(&self, f: impl FnOnce(&mut Outcome)) {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

fn seen(parts: &Parts, f: impl FnOnce(&mut Outcome)) {
    if let Some(seen) = parts.extensions.get::<Seen>() {
        seen.update(f);
    }
}

/// Marks a request the guard already looked at, so a second extractor never repeats the lookup or counts a guess
/// twice.
#[derive(Clone, Copy)]
struct Checked;

async fn authenticate(app: &App, parts: &mut Parts) -> Result<Option<Principal>> {
    let Ok(tokens) = Tokens::of(app) else {
        return Ok(None);
    };
    seen(parts, |o| o.consulted = true);
    // A found principal is kept by core; a refusal is final for the request.
    if parts.extensions.insert(Checked).is_some() {
        return Ok(None);
    }
    let Some(token) = bearer(&parts.headers) else {
        return Ok(None);
    };
    let client = client_key(parts);
    let hash = well_formed(&token).then(|| sha256_hex(&token));
    // Past the budget the request is refused before any database work, unless its token was accepted here
    // recently: one client's guesses never lock out the valid tokens of others behind the same address.
    if let Some(client) = &client
        && let Some(retry_after) = blocked(&tokens, client).await?
    {
        let known = match &hash {
            Some(hash) => tokens.state().accepted.contains_key(hash),
            None => false,
        };
        if !known {
            return Err(too_many(parts, retry_after));
        }
    }
    match check(&tokens, &token).await? {
        Some((principal, meta)) => {
            if let Some(hash) = hash {
                tokens.state().accepted.insert(hash, ()).await;
            }
            schedule_touch(&tokens, meta.0.id).await;
            parts.extensions.insert(meta);
            Ok(Some(principal))
        }
        None => {
            if let Some(hash) = &hash {
                tokens.state().accepted.invalidate(hash).await;
            }
            let Some(client) = client else {
                return Ok(None);
            };
            match guessed(&tokens, &client).await? {
                Some(retry_after) => Err(too_many(parts, retry_after)),
                None => Ok(None),
            }
        }
    }
}

fn too_many(parts: &Parts, retry_after: u64) -> Error {
    seen(parts, |o| o.retry_after = Some(retry_after));
    Error::http(StatusCode::TOO_MANY_REQUESTS, "Too Many Requests")
}

/// The token of the request's one `Authorization: Bearer <token>` header; `None` for no header, two headers or
/// another scheme (not this guard's credential). Query strings, cookies, bodies and other headers are never read.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let text = value.to_str().ok()?;
    let (scheme, rest) = text.split_at_checked(6)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    rest.strip_prefix(' ').map(str::to_owned)
}

/// Who the guess budget counts: the client address after `TRUSTED_PROXIES`, an IPv6 client by its /64. When a
/// trusted proxy forwarded an unreadable address, the proxy itself (`proxy:<address>`). `None` when the request
/// carries no connection information at all (only outside the app's server: a router called directly, a test app
/// without `from_addr`): such requests are not budgeted, never pooled under one shared key.
fn client_key(parts: &Parts) -> Option<String> {
    let info = ClientInfo::from_parts(parts);
    match info.ip() {
        Some(ip) => Some(address_key(ip)),
        None => info
            .peer()
            .map(|peer| format!("proxy:{}", address_key(peer.ip()))),
    }
}

fn address_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => {
            let [a, b, c, d, ..] = v6.segments();
            format!("{a:x}:{b:x}:{c:x}:{d:x}::/64")
        }
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map_or_else(|| v6.to_string(), |v4| v4.to_string()),
        IpAddr::V4(v4) => v4.to_string(),
    }
}

/// The principal of a valid token; `None` for a malformed, unknown, expired or unbound one (an expired or unbound
/// token is deleted and its revocation published).
async fn check(tokens: &Tokens, token: &str) -> Result<Option<(Principal, TokenMeta)>> {
    if !well_formed(token) {
        return Ok(None);
    }
    let Some(stored) = tokens.lookup(token).await? else {
        return Ok(None);
    };
    let meta = stored.token;
    if meta.is_expired_at(ChronoUtc::now()) {
        tokens.end(meta.user_id, meta.id).await;
        return Ok(None);
    }
    let Some(user) = tokens.app().find_user(meta.user_id).await? else {
        return Ok(None);
    };
    // A password written by any code, or `end_credentials` (the credentials epoch), changes the binding: the token
    // ends at its next use.
    if !constant_time_eq(&stored.binding, &user.credential_binding(BINDING_PURPOSE)?) {
        tokens.end(meta.user_id, meta.id).await;
        return Ok(None);
    }
    let mut principal = Principal::new(
        meta.user_id,
        GUARD,
        Credential::token(meta.id, meta.abilities.iter().cloned()),
    );
    if let Some(at) = meta.expires_at {
        principal = principal.expires_at(at);
    }
    let principal = principal.with_user(user)?;
    Ok(Some((principal, TokenMeta(meta))))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Seconds the client must wait when it used up its guess budget, `None` when it may go on (core's
/// [`RateLimiter::peek`](smeltery_core::cache::RateLimiter::peek): nothing is counted).
///
/// # Errors
/// The cache store fails (the request is refused: fail closed).
async fn blocked(tokens: &Tokens, client: &str) -> Result<Option<u64>> {
    Ok(
        match tokens.state().guesses.peek(tokens.app(), client).await? {
            RateLimit::Limited { retry_after, .. } => Some(retry_after),
            _ => None,
        },
    )
}

/// Count one invalid token for the client; `Some(retry_after)` when this one went past the budget (parallel
/// requests that all passed the peek).
///
/// # Errors
/// The cache store fails (fail closed).
async fn guessed(tokens: &Tokens, client: &str) -> Result<Option<u64>> {
    Ok(
        match tokens.state().guesses.hit(tokens.app(), client).await? {
            RateLimit::Limited { retry_after, .. } => Some(retry_after),
            _ => None,
        },
    )
}

/// Write the token's `last_used_at` in the background, at most once a minute per token in this process (and
/// once a minute across processes, by the conditional update).
async fn schedule_touch(tokens: &Tokens, id: i64) {
    let state = tokens.state();
    if !state.recently_used.entry(id).or_insert(()).await.is_fresh() {
        return;
    }
    let tokens = tokens.clone();
    tokens.app().clone().spawn_owned(async move {
        let failed = match tokio::time::timeout(Duration::from_secs(2), tokens.touch(id)).await {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => Some(e.to_string()),
            Err(_) => Some("timed out".to_owned()),
        };
        if let Some(error) = failed {
            // One warning a minute at most: a database outage must not flood the log.
            let now = now_secs();
            let last = tokens.state().last_warned.load(Ordering::Relaxed);
            if now >= last + 60
                && tokens
                    .state()
                    .last_warned
                    .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                tracing::warn!(token_id = id, error = %error, "a token's last_used_at could not be written");
            }
        }
    });
}

/// Global middleware: a request the guard looked at answers with `Vary: Authorization, Cookie`; its 401 with
/// `WWW-Authenticate: Bearer` and `Cache-Control: no-store`; its 429 with `Retry-After` and `no-store`.
pub(crate) async fn response_headers(mut req: Request, next: Next) -> Response {
    let slot = Seen::default();
    req.extensions_mut().insert(slot.clone());
    let mut response = next.run(req).await;
    let outcome = std::mem::take(&mut *slot.0.lock().unwrap_or_else(PoisonError::into_inner));
    if !outcome.consulted {
        return response;
    }
    let status = response.status();
    let headers = response.headers_mut();
    add_vary(headers);
    if status == StatusCode::UNAUTHORIZED {
        headers
            .entry(header::WWW_AUTHENTICATE)
            .or_insert(HeaderValue::from_static("Bearer"));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if status == StatusCode::TOO_MANY_REQUESTS
        && let Some(retry_after) = outcome.retry_after
    {
        headers.insert(header::RETRY_AFTER, HeaderValue::from(retry_after));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

/// Name `Authorization` and `Cookie` in `Vary` (keeping what is there).
fn add_vary(headers: &mut HeaderMap) {
    let present: Vec<String> = headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|v| v.trim().to_ascii_lowercase())
        .collect();
    if present.iter().any(|v| v == "*") {
        return;
    }
    for name in ["Authorization", "Cookie"] {
        if !present.iter().any(|v| v.eq_ignore_ascii_case(name)) {
            headers.append(header::VARY, HeaderValue::from_static(name));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for v in values {
            map.append(header::AUTHORIZATION, HeaderValue::from_str(v).unwrap());
        }
        map
    }

    #[test]
    fn only_one_bearer_header_counts() {
        assert_eq!(
            bearer(&headers(&["Bearer smt_x"])).as_deref(),
            Some("smt_x")
        );
        assert_eq!(
            bearer(&headers(&["bEaReR smt_x"])).as_deref(),
            Some("smt_x")
        );
        assert_eq!(
            bearer(&headers(&["Bearer  smt_x"])).as_deref(),
            Some(" smt_x")
        );
        assert_eq!(bearer(&headers(&["Bearer "])).as_deref(), Some(""));
        assert_eq!(bearer(&headers(&[])), None);
        assert_eq!(bearer(&headers(&["Bearer"])), None);
        assert_eq!(bearer(&headers(&["Basic YTpi"])), None);
        assert_eq!(bearer(&headers(&["Bearersmt_x"])), None);
        assert_eq!(bearer(&headers(&["Bearer a", "Bearer b"])), None);
    }

    #[test]
    fn vary_names_both_headers_once() {
        let mut map = HeaderMap::new();
        map.insert(
            header::VARY,
            HeaderValue::from_static("Accept-Encoding, cookie"),
        );
        add_vary(&mut map);
        let all: Vec<_> = map.get_all(header::VARY).iter().collect();
        assert_eq!(all, ["Accept-Encoding, cookie", "Authorization"]);
        let mut map = HeaderMap::new();
        add_vary(&mut map);
        add_vary(&mut map);
        assert_eq!(map.get_all(header::VARY).iter().count(), 2);
    }
}
