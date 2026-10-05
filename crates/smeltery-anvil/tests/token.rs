//! `POST /api/broadcasting/auth`: bearer credentials of a stateless guard (a fake guard here; Anvil does not depend
//! on any token crate), the ability check, the shared channel rules and the grant's credential and expiry.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use axum::body::Body;
use http::{HeaderMap, HeaderValue, Method};
use serde_json::json;
use smeltery_anvil::testing::{TestSocket, auth_of};
use smeltery_anvil::{AnvilExt as _, ChannelCtx, Channels};
use smeltery_core::auth::{Credential, Guard, Principal};
use smeltery_core::db::prelude::DateTimeUtc;
use smeltery_core::http::request::Parts;
use smeltery_core::testing::{TestApp, TestResponse};
use smeltery_core::{App, AppBuilder, BoxFuture, Result};

/// `X-Test-Token: <user>:<token id>:<abilities, comma-separated>[:<seconds left>]` (a test double).
pub struct FakeTokens;

impl Guard for FakeTokens {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn stateless(&self) -> bool {
        true
    }

    fn authenticate<'a>(
        &'a self,
        _app: &'a App,
        parts: &'a mut Parts,
    ) -> BoxFuture<'a, Result<Option<Principal>>> {
        let header = parts
            .headers
            .get("x-test-token")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Box::pin(async move {
            let Some(header) = header else {
                return Ok(None);
            };
            let fields: Vec<&str> = header.split(':').collect();
            let user: i64 = fields[0].parse().unwrap();
            let id: i64 = fields[1].parse().unwrap();
            let abilities: Vec<&str> = fields[2].split(',').filter(|a| !a.is_empty()).collect();
            let mut principal = Principal::new(user, "fake", Credential::token(id, abilities));
            if let Some(left) = fields.get(3) {
                let left: i64 = left.parse().unwrap();
                let now = std::time::SystemTime::now();
                let wait = std::time::Duration::from_secs(left.unsigned_abs());
                let at = if left >= 0 { now + wait } else { now - wait };
                principal = principal.expires_at(DateTimeUtc::from(at));
            }
            Ok(Some(principal))
        })
    }
}

fn channels(c: &mut Channels) {
    c.public("news");
    // In this test, user n owns order n.
    c.private("orders.{order}", |ctx: ChannelCtx| async move {
        let order: i64 = ctx.param("order")?;
        Ok(ctx.user_id() == Some(order))
    });
}

fn build(b: AppBuilder) -> AppBuilder {
    b.guard(FakeTokens).anvil(channels)
}

fn token_auth(app: &TestApp, token: Option<&str>, socket: &str, channel: &str) -> TestResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    if let Some(token) = token {
        headers.insert("x-test-token", HeaderValue::from_str(token).unwrap());
    }
    let body = format!("socket_id={socket}&channel_name={channel}");
    app.request(
        Method::POST,
        "/api/broadcasting/auth",
        headers,
        Body::from(body),
    )
}

#[test]
fn without_a_bearer_credential_the_answer_is_401() {
    let app = TestApp::new(build);
    let res = token_auth(&app, None, "1.2", "private-orders.7");
    assert_eq!(res.status(), 401);
    assert_eq!(res.header("www-authenticate"), Some("Bearer"));
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert_eq!(res.json(), json!({ "error": "Unauthenticated." }));
    // A signed-in browser session is no bearer credential on this route.
    app.acting_as(7);
    assert_eq!(
        token_auth(&app, None, "1.2", "private-orders.7").status(),
        401
    );
}

#[test]
fn a_token_without_the_broadcasting_ability_gets_the_uniform_403() {
    let app = TestApp::new(build);
    for token in ["7:5:orders-read", "7:5:"] {
        let res = token_auth(&app, Some(token), "1.2", "private-orders.7");
        assert_eq!(res.status(), 403, "{token}");
        assert_eq!(res.json(), json!({ "error": "Forbidden" }));
        assert_eq!(res.header("cache-control"), Some("no-store"));
    }
}

#[test]
fn a_token_with_the_ability_is_signed_for_its_own_channels_only() {
    let app = TestApp::new(build);
    for token in ["7:5:broadcasting", "7:5:*"] {
        let res = token_auth(&app, Some(token), "1.2", "private-orders.7");
        assert_eq!(res.status(), 200, "{token}: {}", res.text());
        let auth = auth_of(&res).unwrap();
        assert!(
            auth.contains(":7.fake~token~5."),
            "the grant names the token: {auth}"
        );
    }
    // The pattern's callback decides as on the cookie endpoint.
    let res = token_auth(&app, Some("7:5:broadcasting"), "1.2", "private-orders.8");
    assert_eq!(res.status(), 403);
    let res = token_auth(&app, Some("7:5:broadcasting"), "1.2", "private-unknown");
    assert_eq!(res.status(), 403);
    let res = token_auth(&app, Some("7:5:broadcasting"), "1.2", "news");
    assert_eq!(res.status(), 400);
}

#[test]
fn a_token_grant_subscribes_the_socket_it_was_made_for() {
    let app = TestApp::new(build);
    let mut socket = TestSocket::connect(app.app());
    let res = token_auth(
        &app,
        Some("7:5:broadcasting"),
        socket.socket_id(),
        "private-orders.7",
    );
    let auth = auth_of(&res).unwrap();
    assert_eq!(
        socket.subscribe("private-orders.7", Some(&auth))["event"],
        "pusher_internal:subscription_succeeded"
    );
}

#[test]
fn a_grant_never_outlives_its_token() {
    let app = TestApp::new(build);
    let res = token_auth(&app, Some("7:5:broadcasting:60"), "1.2", "private-orders.7");
    let auth = auth_of(&res).unwrap();
    // `<key>:<user>.<credential>.<issued>.<expires>:<hex>`
    let grant: Vec<&str> = auth.split(':').nth(1).unwrap().split('.').collect();
    let issued: u64 = grant[2].parse().unwrap();
    let expires: u64 = grant[3].parse().unwrap();
    assert!(
        expires <= issued + 61,
        "capped at the token's expiry: {auth}"
    );
    // An expired token gets no grant.
    let res = token_auth(&app, Some("7:5:broadcasting:-5"), "1.2", "private-orders.7");
    assert_eq!(res.status(), 403);
}

#[test]
fn without_a_stateless_guard_the_route_answers_404() {
    let app = TestApp::new(|b| b.anvil(channels));
    let res = token_auth(&app, Some("7:5:broadcasting"), "1.2", "private-orders.7");
    assert_eq!(res.status(), 404);
}

/// A guard whose principals have keys of unusual shapes: `X-Odd: session` (a session binding that is not hex) or
/// `X-Odd: long` (a token of a guard with a long name, which core allows).
struct OddGuard(&'static str);

const LONG_GUARD: &str =
    "a-guard-with-a-name-longer-than-sixty-four-characters-which-core-accepts-as-a-guard-name";

impl Guard for OddGuard {
    fn name(&self) -> &'static str {
        self.0
    }

    fn stateless(&self) -> bool {
        true
    }

    fn authenticate<'a>(
        &'a self,
        _app: &'a App,
        parts: &'a mut Parts,
    ) -> BoxFuture<'a, Result<Option<Principal>>> {
        let header = parts
            .headers
            .get("x-odd")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let name = self.0;
        Box::pin(async move {
            Ok(match (header.as_deref(), name) {
                (Some("session"), "odd") => Some(Principal::new(
                    7,
                    "odd",
                    Credential::session("not a hex binding"),
                )),
                (Some("long"), LONG_GUARD) => Some(Principal::new(
                    7,
                    LONG_GUARD,
                    Credential::token(5, ["broadcasting"]),
                )),
                _ => None,
            })
        })
    }
}

#[test]
fn a_credential_without_a_revocable_key_gets_no_grant() {
    let app = TestApp::new(|b| {
        b.guard(OddGuard("odd"))
            .guard(OddGuard(LONG_GUARD))
            .anvil(channels)
    });
    let request = |odd: &'static str| {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        headers.insert("x-odd", HeaderValue::from_static(odd));
        app.request(
            Method::POST,
            "/api/broadcasting/auth",
            headers,
            Body::from("socket_id=1.2&channel_name=private-orders.7"),
        )
    };
    // A key no revocation event could name: refused, never signed.
    let res = request("session");
    assert_eq!(res.status(), 500, "{}", res.text());
    assert_eq!(res.json(), json!({ "error": "Server Error" }));
    // A long guard name is a valid key.
    let res = request("long");
    assert_eq!(res.status(), 200, "{}", res.text());
    assert!(
        auth_of(&res)
            .unwrap()
            .contains(&format!(":7.{LONG_GUARD}~token~5."))
    );
}
