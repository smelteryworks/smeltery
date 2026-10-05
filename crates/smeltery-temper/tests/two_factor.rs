//! Two-factor authentication through Temper's routes: enrolment, the challenge, recovery codes, and the attack tests
//! of the design (replay, races, budgets, expiry, bindings, secrets at rest).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::*;
use smeltery_core::AppBuilder;
use smeltery_core::auth::{SecondFactorVerdict, passwords};
use smeltery_core::console::{Args, Command, Output};
use smeltery_core::db::Record;
use smeltery_core::db::prelude::Set;
use smeltery_core::http::IntoResponse as _;
use smeltery_core::http::{HeaderMap, HeaderValue, Method, header};
use smeltery_core::testing::{TestApp, TestResponse};
use smeltery_temper::testing::{
    code_for_secret, confirm_password, enable_two_factor, fake_clock, has_pending_two_factor,
    log_in, two_factor_code, two_factor_code_at,
};
use smeltery_temper::two_factor::DisableCommand;
use smeltery_temper::{Temper, TemperEvent, TemperExt as _, TemperViews, TwoFactor};

/// A fixed time (a step boundary is 30 s away).
const T: i64 = 1_700_000_010;

fn two_factor_harness() -> Harness {
    let h = harness_with(|t| t.two_factor(TwoFactor::new()), |b| b);
    fake_clock(h.app.app(), T);
    h
}

/// Ada with a confirmed enrolment: her id and recovery codes.
fn enrolled(h: &Harness) -> (i64, Vec<String>) {
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    let codes = enable_two_factor::<User>(&h.app, id);
    (id, codes)
}

fn submit(app: &TestApp, field: &str, code: &str) -> TestResponse {
    app.post_form("/two-factor-challenge", &[(field, code)])
}

/// Move the clock by `steps` steps of 30 seconds.
fn advance(h: &Harness, steps: i64) {
    let now = T + steps * 30;
    fake_clock(h.app.app(), now);
}

// ---- the challenge ------------------------------------------------------------------------------------------------

#[test]
fn a_two_factor_login_needs_the_code_and_then_signs_in() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    let res = log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/two-factor-challenge"));
    assert_eq!(h.app.get("/dashboard").status(), 303, "not signed in yet");
    assert!(has_pending_two_factor(&h.app));
    assert!(
        h.app
            .get("/two-factor-challenge")
            .text()
            .starts_with("page=two-factor-challenge")
    );
    let wrong = submit(&h.app, "code", "000000");
    assert_eq!(wrong.header("location"), Some("/two-factor-challenge"));
    assert!(
        h.app
            .get("/two-factor-challenge")
            .text()
            .contains("The provided two factor authentication code was invalid.")
    );
    let res = submit(&h.app, "code", &two_factor_code::<User>(&h.app, id));
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    assert_eq!(h.app.get("/dashboard").status(), 200);
    assert!(!has_pending_two_factor(&h.app));
    let names: Vec<&str> = h.events.events().iter().map(TemperEvent::name).collect();
    assert_eq!(
        names,
        ["two_factor_challenged", "two_factor_failed", "login"]
    );
    assert!(matches!(
        h.events.events().last().unwrap(),
        TemperEvent::Login {
            two_factor: true,
            ..
        }
    ));
}

#[test]
fn json_clients_get_two_factor_true_and_no_session_login() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    let res = json_request(
        &h.app,
        Method::POST,
        "/login",
        &serde_json::json!({ "email": "ada@example.com", "password": "analytical-engine" }),
    );
    assert_eq!(res.status(), 200);
    assert_eq!(res.json(), serde_json::json!({ "two_factor": true }));
    let dashboard = h
        .app
        .request(Method::GET, "/dashboard", json_headers(), "".into());
    assert_eq!(dashboard.status(), 401);
    let wrong = json_request(
        &h.app,
        Method::POST,
        "/two-factor-challenge",
        &serde_json::json!({ "code": "000000" }),
    );
    assert_eq!(wrong.status(), 422);
    assert!(wrong.json()["errors"]["code"].is_array());
    let ok = json_request(
        &h.app,
        Method::POST,
        "/two-factor-challenge",
        &serde_json::json!({ "code": two_factor_code::<User>(&h.app, id) }),
    );
    assert_eq!(ok.status(), 204);
}

#[test]
fn users_without_two_factor_sign_in_as_before() {
    let h = two_factor_harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
}

#[test]
fn the_challenge_without_a_pending_login_goes_to_login() {
    let h = two_factor_harness();
    enrolled(&h);
    assert_eq!(
        h.app.get("/two-factor-challenge").header("location"),
        Some("/login")
    );
    let res = submit(&h.app, "code", "123456");
    assert_eq!(res.header("location"), Some("/login"));
    assert!(
        h.app
            .get("/login")
            .text()
            .contains("Your login has expired")
    );
}

#[test]
fn a_pending_login_expires() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    advance(&h, 10); // five minutes
    let res = submit(&h.app, "code", &two_factor_code::<User>(&h.app, id));
    assert_eq!(res.header("location"), Some("/login"));
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

#[test]
fn a_reset_kills_a_pending_login() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    // The owner resets the password meanwhile.
    h.app.block_on(async {
        passwords::send_reset_link(h.app.app(), "ada@example.com")
            .await
            .unwrap()
    });
    let link = h.outbox.last_reset_path();
    let token = link
        .trim_start_matches("/reset-password/")
        .split('?')
        .next()
        .unwrap()
        .to_owned();
    h.app.block_on(async {
        passwords::reset(h.app.app(), "ada@example.com", &token, "a-new-password")
            .await
            .unwrap()
            .unwrap()
    });
    let res = submit(&h.app, "code", &two_factor_code::<User>(&h.app, id));
    assert_eq!(res.header("location"), Some("/login"));
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

#[test]
fn five_wrong_codes_need_the_password_again() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    for _ in 0..4 {
        assert_eq!(
            submit(&h.app, "code", "000000").header("location"),
            Some("/two-factor-challenge")
        );
    }
    assert_eq!(
        submit(&h.app, "code", "000000").header("location"),
        Some("/login")
    );
    let res = submit(&h.app, "code", &two_factor_code::<User>(&h.app, id));
    assert_eq!(res.header("location"), Some("/login"));
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

#[test]
fn the_two_factor_login_regenerates_the_session_twice() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    let first = h.app.get("/_session").text();
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let pending = h.app.get("/_session").text();
    submit(&h.app, "code", &two_factor_code::<User>(&h.app, id));
    let signed_in = h.app.get("/_session").text();
    let id_of = |s: &str| s.split('|').next().unwrap().to_owned();
    let token_of = |s: &str| s.split('|').nth(1).unwrap().to_owned();
    assert_ne!(
        id_of(&first),
        id_of(&pending),
        "a new id for the pending login"
    );
    assert_ne!(id_of(&pending), id_of(&signed_in), "a new id at sign-in");
    assert_ne!(
        token_of(&pending),
        token_of(&signed_in),
        "a new CSRF secret at sign-in"
    );
}

// ---- replay and races ---------------------------------------------------------------------------------------------

#[test]
fn a_code_cannot_be_used_twice() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    let code = two_factor_code::<User>(&h.app, id);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(
        submit(&h.app, "code", &code).header("location"),
        Some("/dashboard")
    );
    h.app.post_form("/logout", &[]);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(
        submit(&h.app, "code", &code).header("location"),
        Some("/two-factor-challenge"),
        "an observed code is refused"
    );
    // The previous step's code is refused too (an older step than the last accepted one).
    let earlier = two_factor_code_at::<User>(&h.app, id, -1);
    assert_eq!(
        submit(&h.app, "code", &earlier).header("location"),
        Some("/two-factor-challenge")
    );
    // The next step's code works.
    advance(&h, 1);
    let next = two_factor_code::<User>(&h.app, id);
    assert_eq!(
        submit(&h.app, "code", &next).header("location"),
        Some("/dashboard")
    );
}

/// Log in on a new "device" (a fresh cookie jar); its cookie header with the pending login.
fn pending_device(h: &Harness) -> String {
    h.app.clear_cookies();
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let name = session_cookie(&h.app);
    format!("{name}={}", h.app.cookie(&name).unwrap())
}

fn submit_as(app: &TestApp, cookie: &str, field: &str, code: &str) -> TestResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_str(cookie).unwrap());
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    app.request(
        Method::POST,
        "/two-factor-challenge",
        headers,
        format!("{field}={code}").into(),
    )
}

#[test]
fn two_parallel_submissions_of_one_code_sign_in_once() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    let devices: Vec<String> = (0..4).map(|_| pending_device(&h)).collect();
    let code = two_factor_code::<User>(&h.app, id);
    let signed_in = std::thread::scope(|s| {
        let handles: Vec<_> = devices
            .iter()
            .map(|cookie| s.spawn(|| submit_as(&h.app, cookie, "code", &code)))
            .collect();
        handles
            .into_iter()
            .map(|t| t.join().unwrap())
            .filter(|r| r.header("location") == Some("/dashboard"))
            .count()
    });
    assert_eq!(signed_in, 1);
}

#[test]
fn a_recovery_code_works_once_even_in_parallel() {
    let h = two_factor_harness();
    let (id, codes) = enrolled(&h);
    let devices: Vec<String> = (0..4).map(|_| pending_device(&h)).collect();
    let signed_in = std::thread::scope(|s| {
        let handles: Vec<_> = devices
            .iter()
            .map(|cookie| s.spawn(|| submit_as(&h.app, cookie, "recovery_code", &codes[0])))
            .collect();
        handles
            .into_iter()
            .map(|t| t.join().unwrap())
            .filter(|r| r.header("location") == Some("/dashboard"))
            .count()
    });
    assert_eq!(signed_in, 1);
    let ada = user(&h.app, id);
    let left: Vec<String> =
        serde_json::from_str(ada.two_factor_recovery_codes.as_deref().unwrap()).unwrap();
    assert_eq!(left.len(), 7);
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::RecoveryCodeUsed { left: 7, .. })),
        1
    );
    // Another code still works, once.
    h.app.clear_cookies();
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(
        submit(&h.app, "recovery_code", &codes[1]).header("location"),
        Some("/dashboard")
    );
}

// ---- budgets ------------------------------------------------------------------------------------------------------

#[test]
fn the_challenge_budget_is_per_account_across_sessions() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    let first = pending_device(&h);
    let second = pending_device(&h);
    for _ in 0..3 {
        submit_as(&h.app, &first, "code", "000000");
    }
    for _ in 0..2 {
        submit_as(&h.app, &second, "code", "000000");
    }
    // Five wrong codes for the account: the right one is refused before it is checked.
    let res = submit_as(
        &h.app,
        &second,
        "code",
        &two_factor_code::<User>(&h.app, id),
    );
    assert_eq!(res.header("location"), Some("/two-factor-challenge"));
    assert!(h.app.get("/dashboard").status() == 303, "not signed in");
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::TwoFactorLockout { .. })),
        1
    );
}

#[test]
fn the_challenge_budget_is_shared_by_processes_through_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().to_owned();
    let app = |cache: std::path::PathBuf| {
        harness_with(
            |t| t.two_factor(TwoFactor::new()),
            move |mut b: AppBuilder| {
                b.settings_mut().cache_store = "file".into();
                b.settings_mut().cache_path = cache;
                b
            },
        )
    };
    let (one, two) = (app(cache.clone()), app(cache));
    for h in [&one, &two] {
        fake_clock(h.app.app(), T);
        enrolled(h);
        log_in(&h.app, "ada@example.com", "analytical-engine");
    }
    for _ in 0..3 {
        submit(&one.app, "code", "000000");
    }
    for _ in 0..2 {
        submit(&two.app, "code", "000000");
    }
    let json = json_request(
        &two.app,
        Method::POST,
        "/two-factor-challenge",
        &serde_json::json!({ "code": two_factor_code::<User>(&two.app, 1) }),
    );
    assert_eq!(json.status(), 429, "{}", json.text());
}

// ---- management ---------------------------------------------------------------------------------------------------

/// Signed in, password confirmed.
fn signed_in(h: &Harness) -> i64 {
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(confirm_password(&h.app, "analytical-engine").status(), 303);
    id
}

#[test]
fn two_factor_can_be_enabled_confirmed_used_and_disabled() {
    let h = two_factor_harness();
    let id = signed_in(&h);
    let res = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-authentication",
        &serde_json::json!({}),
    );
    assert_eq!(res.status(), 200);
    let codes: Vec<String> = serde_json::from_value(res.json()["recovery_codes"].clone()).unwrap();
    assert_eq!(codes.len(), 8);
    assert!(
        codes
            .iter()
            .all(|c| c.len() == 21 && c.as_bytes()[10] == b'-')
    );
    // Not confirmed yet: a login needs no code.
    let qr = h.app.get("/user/two-factor-qr-code");
    assert_eq!(qr.status(), 200);
    assert_eq!(qr.header("cache-control"), Some("no-store"));
    assert!(
        qr.json()["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/svg+xml;base64,")
    );
    let secret = h.app.get("/user/two-factor-secret-key").json()["secretKey"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(secret.len(), 32, "160 bits in base32");
    assert_eq!(
        h.app.get("/user/two-factor-recovery-codes").json(),
        serde_json::json!({ "remaining": 8 })
    );
    let wrong = json_request(
        &h.app,
        Method::POST,
        "/user/confirmed-two-factor-authentication",
        &serde_json::json!({ "code": "000000" }),
    );
    assert_eq!(wrong.status(), 422);
    let code = code_for_secret(&secret, T).unwrap();
    assert_eq!(code, two_factor_code::<User>(&h.app, id));
    let ok = json_request(
        &h.app,
        Method::POST,
        "/user/confirmed-two-factor-authentication",
        &serde_json::json!({ "code": code }),
    );
    assert_eq!(ok.status(), 200);
    assert!(user(&h.app, id).two_factor_confirmed_at.is_some());
    assert_eq!(h.app.get("/dashboard").status(), 200, "this session stays");
    // Now a login needs the code; the confirming code itself is used up.
    h.app.post_form("/logout", &[]);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(
        submit(&h.app, "code", &code).header("location"),
        Some("/two-factor-challenge")
    );
    assert_eq!(
        submit(&h.app, "recovery_code", &codes[3]).header("location"),
        Some("/dashboard")
    );
    // New codes replace the old ones.
    confirm_password(&h.app, "analytical-engine");
    let fresh = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-recovery-codes",
        &serde_json::json!({}),
    );
    let fresh: Vec<String> =
        serde_json::from_value(fresh.json()["recovery_codes"].clone()).unwrap();
    assert_eq!(fresh.len(), 8);
    assert!(!fresh.contains(&codes[4]));
    // Turned off: the columns are empty and a login needs no code.
    let off = form_request(
        &h.app,
        Method::DELETE,
        "/user/two-factor-authentication",
        &[],
    );
    assert_eq!(off.status(), 303);
    let ada = user(&h.app, id);
    assert!(ada.two_factor_secret.is_none() && ada.two_factor_recovery_codes.is_none());
    assert!(ada.two_factor_confirmed_at.is_none() && ada.two_factor_last_step.is_none());
    h.app.post_form("/logout", &[]);
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
    let names: Vec<&str> = h.events.events().iter().map(TemperEvent::name).collect();
    // Two wrong codes: one at confirmation, the replayed confirming code at login.
    assert_eq!(
        names.iter().filter(|n| **n == "two_factor_failed").count(),
        2
    );
    for name in [
        "two_factor_enabled",
        "two_factor_confirmed",
        "recovery_code_used",
        "recovery_codes_generated",
        "two_factor_disabled",
    ] {
        assert_eq!(
            names.iter().filter(|n| **n == name).count(),
            1,
            "{name} in {names:?}"
        );
    }
}

#[test]
fn the_browser_flow_flashes_the_codes_once() {
    let h = two_factor_harness();
    signed_in(&h);
    let res = h.app.post_form("/user/two-factor-authentication", &[]);
    assert_eq!(res.status(), 303);
    let shown = h.app.get("/settings-2fa").text();
    assert!(shown.contains("codes=8"), "{shown}");
    assert!(
        h.app.get("/settings-2fa").text().contains("codes=none"),
        "shown once"
    );
}

#[test]
fn management_routes_need_a_recent_confirmation() {
    let h = two_factor_harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let res = h.app.post_form("/user/two-factor-authentication", &[]);
    assert_eq!(res.header("location"), Some("/user/confirm-password"));
    for (method, path) in [
        (Method::POST, "/user/two-factor-authentication"),
        (Method::POST, "/user/confirmed-two-factor-authentication"),
        (Method::DELETE, "/user/two-factor-authentication"),
        (Method::GET, "/user/two-factor-qr-code"),
        (Method::GET, "/user/two-factor-secret-key"),
        (Method::GET, "/user/two-factor-recovery-codes"),
        (Method::POST, "/user/two-factor-recovery-codes"),
    ] {
        let res = json_request(&h.app, method.clone(), path, &serde_json::json!({}));
        assert_eq!(res.status(), 423, "{method} {path}");
    }
    // Guests never reach them.
    h.app.post_form("/logout", &[]);
    assert_eq!(
        h.app.get("/user/two-factor-qr-code").header("location"),
        Some("/login")
    );
}

#[test]
fn every_two_factor_route_has_its_name_middleware_and_throttle() {
    let h = two_factor_harness();
    let routes = h.app.app().routes();
    let middleware = |name: &str| {
        routes
            .iter()
            .find(|r| r.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("{name}"))
            .middleware
            .clone()
    };
    assert_eq!(middleware("two-factor.login"), ["guest"]);
    assert_eq!(
        middleware("two-factor.login.store"),
        ["guest", "throttle:30,1"]
    );
    for name in [
        "two-factor.enable",
        "two-factor.confirm",
        "two-factor.disable",
        "two-factor.regenerate-recovery-codes",
    ] {
        assert_eq!(
            middleware(name),
            ["auth", "password.confirm", "throttle:6,1"],
            "{name}"
        );
    }
    for name in [
        "two-factor.qr-code",
        "two-factor.secret-key",
        "two-factor.recovery-codes",
    ] {
        assert_eq!(middleware(name), ["auth", "password.confirm"], "{name}");
    }
}

#[test]
fn enabling_twice_does_not_replace_a_confirmed_secret() {
    let h = two_factor_harness();
    let id = signed_in(&h);
    enable_two_factor::<User>(&h.app, id);
    let before = user(&h.app, id).two_factor_secret;
    let res = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-authentication",
        &serde_json::json!({}),
    );
    assert_eq!(res.status(), 409);
    assert_eq!(user(&h.app, id).two_factor_secret, before);
    let browser = h.app.post_form("/user/two-factor-authentication", &[]);
    assert_eq!(browser.status(), 303);
    assert_eq!(user(&h.app, id).two_factor_secret, before);
}

#[test]
fn remember_cookies_from_before_two_factor_stop_working() {
    let h = two_factor_harness();
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    let remember = format!("remember_{}", session_cookie(&h.app));
    h.app.post_form(
        "/login",
        &[
            ("email", "ada@example.com"),
            ("password", "analytical-engine"),
            ("remember", "true"),
        ],
    );
    let old_cookie = h.app.cookie(&remember).expect("a remember-me cookie");
    // Another device turns two-factor on.
    h.app.clear_cookies();
    log_in(&h.app, "ada@example.com", "analytical-engine");
    confirm_password(&h.app, "analytical-engine");
    let secret = {
        h.app.post_form("/user/two-factor-authentication", &[]);
        h.app.get("/user/two-factor-secret-key").json()["secretKey"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let code = code_for_secret(&secret, T).unwrap();
    assert_eq!(
        form_request(
            &h.app,
            Method::POST,
            "/user/confirmed-two-factor-authentication",
            &[("code", &code)]
        )
        .status(),
        303
    );
    assert!(user(&h.app, id).two_factor_confirmed_at.is_some());
    // The first device comes back with only its remember-me cookie.
    h.app.clear_cookies();
    h.app.set_cookie(&remember, &old_cookie);
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

#[test]
fn the_qr_code_holds_no_markup_from_user_data() {
    let h = two_factor_harness();
    let hostile = "a\"><script>alert(1)</script>@example.com";
    let id = create_user(&h.app, hostile, "analytical-engine");
    enable_two_factor::<User>(&h.app, id);
    h.app.acting_as(id);
    // `acting_as` signs in without the session stack's confirmation: confirm through the route.
    assert_eq!(confirm_password(&h.app, "analytical-engine").status(), 303);
    let qr = h.app.get("/user/two-factor-qr-code");
    assert_eq!(qr.status(), 200, "{}", qr.text());
    let svg = qr.json()["svg"].as_str().unwrap().to_owned();
    assert!(!svg.contains("script") && !svg.contains("example") && !svg.contains("alert"));
    let body = qr.text();
    assert!(!body.contains("<script>"), "{body}");
}

// ---- secrets at rest ----------------------------------------------------------------------------------------------

#[test]
fn secrets_are_stored_encrypted_and_bound_to_the_user() {
    let h = two_factor_harness();
    let ada = signed_in(&h);
    h.app.post_form("/user/two-factor-authentication", &[]);
    let secret = h.app.get("/user/two-factor-secret-key").json()["secretKey"]
        .as_str()
        .unwrap()
        .to_owned();
    let stored = user(&h.app, ada).two_factor_secret.unwrap();
    assert!(!stored.contains(&secret));
    let codes = user(&h.app, ada).two_factor_recovery_codes.unwrap();
    assert!(
        serde_json::from_str::<Vec<String>>(&codes)
            .unwrap()
            .iter()
            .all(|c| c.len() == 64)
    );
    // Bob's row with a copy of Ada's sealed secret: it does not open for Bob, and his login still needs a code.
    let bob = create_user(&h.app, "bob@example.com", "bobs-password");
    let bob_row = user(&h.app, bob);
    let db = h.app.db();
    let sealed = stored.clone();
    h.app.block_on(async move {
        bob_row
            .update(&db, |m| {
                m.two_factor_secret = Set(Some(sealed));
                m.two_factor_recovery_codes = Set(Some("[]".into()));
                m.two_factor_confirmed_at = Set(Some(smeltery_core::db::prelude::ChronoUtc::now()));
            })
            .await
            .unwrap()
    });
    h.app.clear_cookies();
    log_in(&h.app, "bob@example.com", "bobs-password");
    assert!(has_pending_two_factor(&h.app), "never silently off");
    let res = submit(&h.app, "code", &code_for_secret(&secret, T).unwrap());
    assert_eq!(res.header("location"), Some("/two-factor-challenge"));
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

#[test]
fn a_secret_under_another_app_key_fails_closed() {
    let h = two_factor_harness();
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    let secret = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";
    // Sealed by an app with another APP_KEY, for the same user id.
    let sealed = h.app.block_on(async {
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.key = "base64:QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU=".into();
        let other = AppBuilder::new(settings).build().await.unwrap().app;
        other
            .encrypt(
                "temper.two-factor",
                id.to_string().as_bytes(),
                secret.as_bytes(),
            )
            .unwrap()
    });
    let row = user(&h.app, id);
    let db = h.app.db();
    h.app.block_on(async move {
        row.update(&db, |m| {
            m.two_factor_secret = Set(Some(sealed));
            m.two_factor_recovery_codes = Set(Some("[]".into()));
            m.two_factor_confirmed_at = Set(Some(smeltery_core::db::prelude::ChronoUtc::now()));
        })
        .await
        .unwrap()
    });
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert!(has_pending_two_factor(&h.app));
    let res = submit(&h.app, "code", &code_for_secret(secret, T).unwrap());
    assert_eq!(res.header("location"), Some("/two-factor-challenge"));
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

// ---- other seams --------------------------------------------------------------------------------------------------

#[test]
fn the_second_factor_seam_answers_for_other_endpoints() {
    let h = two_factor_harness();
    let (id, codes) = enrolled(&h);
    let other = create_user(&h.app, "bob@example.com", "bobs-password");
    let app = h.app.app().clone();
    let code = two_factor_code::<User>(&h.app, id);
    let (needs, needs_not, right, again, recovery) = h.app.block_on(async move {
        let factor = app.second_factor().expect("registered by two_factor");
        let ada = app.find_user(id).await.unwrap().unwrap();
        let bob = app.find_user(other).await.unwrap().unwrap();
        (
            factor.required(&app, &ada).await.unwrap(),
            factor.required(&app, &bob).await.unwrap(),
            factor.verify(&app, &ada, &code).await.unwrap(),
            factor.verify(&app, &ada, &code).await.unwrap(),
            factor.verify(&app, &ada, &codes[0]).await.unwrap(),
        )
    });
    assert!(needs && !needs_not);
    assert_eq!(right, SecondFactorVerdict::Valid);
    assert_eq!(again, SecondFactorVerdict::Invalid, "single use");
    assert_eq!(recovery, SecondFactorVerdict::Valid);
}

#[test]
fn a_spent_code_budget_is_reported_as_too_many_attempts_by_the_seam() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    let app = h.app.app().clone();
    let code = two_factor_code::<User>(&h.app, id);
    let verdicts = h.app.block_on(async move {
        let factor = app.second_factor().expect("registered by two_factor");
        let ada = app.find_user(id).await.unwrap().unwrap();
        let mut verdicts = Vec::new();
        for _ in 0..5 {
            verdicts.push(factor.verify(&app, &ada, "000000").await.unwrap());
        }
        // The right code over the budget is not even checked.
        verdicts.push(factor.verify(&app, &ada, &code).await.unwrap());
        verdicts
    });
    assert!(
        verdicts[..5]
            .iter()
            .all(|v| *v == SecondFactorVerdict::Invalid)
    );
    let Some(SecondFactorVerdict::TooManyAttempts { retry_after }) = verdicts.last().copied()
    else {
        panic!("expected TooManyAttempts, got {:?}", verdicts.last());
    };
    assert!((1..=300).contains(&retry_after));
    let err = SecondFactorVerdict::TooManyAttempts { retry_after }
        .into_result("code", "invalid")
        .unwrap_err()
        .into_response();
    assert_eq!(err.status(), 429);
    assert_eq!(
        err.headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some(retry_after.to_string().as_str())
    );
}

#[test]
fn a_reset_that_verifies_the_address_removes_an_earlier_enrolment() {
    let h = harness_with(
        |t| t.two_factor(TwoFactor::new()),
        |b| b.verify_email::<User>(),
    );
    fake_clock(h.app.app(), T);
    let (id, _) = enrolled(&h);
    assert!(user(&h.app, id).email_verified_at.is_none());
    h.app
        .post_form("/forgot-password", &[("email", "ada@example.com")]);
    let link = h.outbox.last_reset_path();
    h.app.post_form(
        link.split('?').next().unwrap(),
        &[
            ("email", "ada@example.com"),
            ("password", "a-new-password"),
            ("password_confirmation", "a-new-password"),
        ],
    );
    let ada = user(&h.app, id);
    assert!(ada.email_verified_at.is_some());
    assert!(
        ada.two_factor_secret.is_none(),
        "the enrolment from before is gone"
    );
    // A verified user's enrolment survives a later reset.
    let id2 = create_user(&h.app, "bob@example.com", "bobs-password");
    h.app.block_on(async {
        smeltery_core::auth::mark_verified(h.app.app(), id2)
            .await
            .unwrap()
    });
    enable_two_factor::<User>(&h.app, id2);
    h.app
        .post_form("/forgot-password", &[("email", "bob@example.com")]);
    let link = h.outbox.last_reset_path();
    h.app.post_form(
        link.split('?').next().unwrap(),
        &[
            ("email", "bob@example.com"),
            ("password", "a-new-password"),
            ("password_confirmation", "a-new-password"),
        ],
    );
    assert!(user(&h.app, id2).two_factor_secret.is_some());
}

#[test]
fn the_operator_command_turns_two_factor_off_only_with_force() {
    let h = two_factor_harness();
    let (id, _) = enrolled(&h);
    let app = h.app.app().clone();
    let run = |words: &[&str]| {
        let out = Output::default();
        let args = Args::new(words.iter().copied());
        let result = h
            .app
            .block_on(DisableCommand::<User>::new().run_with_output(&app, args, out.clone()));
        (result, out.take())
    };
    let (result, text) = run(&["ada@example.com"]);
    assert!(result.is_ok());
    assert!(text.contains("--force"), "{text}");
    assert!(user(&h.app, id).two_factor_secret.is_some());
    let (result, text) = run(&["ada@example.com", "--force"]);
    assert!(result.is_ok());
    assert!(text.contains("turned off"), "{text}");
    assert!(!text.contains("ada@"), "prints only the result");
    assert!(user(&h.app, id).two_factor_secret.is_none());
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::TwoFactorDisabled { .. })),
        1
    );
    assert!(run(&["nobody@example.com", "--force"]).0.is_err());
}

#[test]
fn confirm_false_turns_two_factor_on_at_enable() {
    let h = harness_with(|t| t.two_factor(TwoFactor::new().confirm(false)), |b| b);
    fake_clock(h.app.app(), T);
    let id = signed_in(&h);
    json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-authentication",
        &serde_json::json!({}),
    );
    assert!(user(&h.app, id).two_factor_confirmed_at.is_some());
    h.app.post_form("/logout", &[]);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert!(has_pending_two_factor(&h.app));
}

#[test]
fn an_unconfirmed_enrolment_does_not_ask_for_a_code() {
    let h = two_factor_harness();
    signed_in(&h);
    h.app.post_form("/user/two-factor-authentication", &[]);
    h.app.post_form("/logout", &[]);
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
}

#[test]
fn the_code_is_never_flashed_as_old_input() {
    let h = two_factor_harness();
    enrolled(&h);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    // A failed validation (neither field usable) flashes the form back, without `code`.
    h.app.post_form(
        "/two-factor-challenge",
        &[("code", "   "), ("email", "ada@example.com")],
    );
    let page = h.app.get("/two-factor-challenge").text();
    assert!(page.contains("old_email=ada@example.com"), "{page}");
    assert!(page.contains("old_code=|"), "{page}");
}

// ---- set-up -------------------------------------------------------------------------------------------------------

async fn build(builder: impl FnOnce(AppBuilder) -> AppBuilder) -> String {
    let mut settings = smeltery_core::config::Settings::from_env();
    settings.env = "testing".into();
    match builder(AppBuilder::new(settings)).build().await {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    }
}

#[tokio::test]
async fn two_factor_set_up_mistakes_stop_the_app() {
    let error = build(|b| {
        b.temper(
            Temper::<User>::new()
                .two_factor(TwoFactor::new())
                .views(TemperViews::new().login(|_| "l").confirm_password(|_| "c")),
        )
    })
    .await;
    assert!(
        error.contains("TemperViews::two_factor_challenge"),
        "{error}"
    );
    for (options, needle) in [
        (TwoFactor::new().window(3), "window"),
        (TwoFactor::new().recovery_codes(3), "recovery_codes"),
        (TwoFactor::new().recovery_codes(17), "recovery_codes"),
        (
            TwoFactor::new().challenge_ttl(std::time::Duration::from_secs(30)),
            "challenge_ttl",
        ),
        (
            TwoFactor::new().challenge_ttl(std::time::Duration::from_secs(901)),
            "challenge_ttl",
        ),
    ] {
        let error =
            build(move |b| b.temper(Temper::<User>::new().views(false).two_factor(options))).await;
        assert!(error.contains(needle), "{needle}: {error}");
    }
    assert_eq!(
        build(|b| b.temper(
            Temper::<User>::new()
                .views(false)
                .two_factor(TwoFactor::new())
        ))
        .await,
        ""
    );
    let error = build(|b| {
        b.temper(
            Temper::<User>::new()
                .views(false)
                .two_factor(TwoFactor::new())
                .without_route("two-factor.qr-codes"),
        )
    })
    .await;
    assert!(error.contains("two-factor.qr-code,"), "{error}");
}

#[test]
fn without_two_factor_its_routes_do_not_exist() {
    let h = harness();
    assert_eq!(h.app.get("/two-factor-challenge").status(), 404);
    assert_eq!(h.app.post_form("/two-factor-challenge", &[]).status(), 404);
    assert!(h.app.app().second_factor().is_none());
}

#[test]
fn the_password_confirm_guard_can_be_left_off() {
    let h = harness_with(
        |t| t.two_factor(TwoFactor::new().confirm_password(false)),
        |b| b,
    );
    create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let res = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-authentication",
        &serde_json::json!({}),
    );
    assert_eq!(res.status(), 200);
}

// ---- phase 3 review round 2 ---------------------------------------------------------------------------------------

#[test]
fn an_exhausted_code_budget_never_blocks_a_recovery_code() {
    let h = two_factor_harness();
    let (_, codes) = enrolled(&h);
    // The attacker with the password burns the account's code budget from several pending logins.
    for _ in 0..2 {
        h.app.clear_cookies();
        log_in(&h.app, "ada@example.com", "analytical-engine");
        for _ in 0..4 {
            submit(&h.app, "code", "000000");
        }
    }
    h.app.clear_cookies();
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let refused = json_request(
        &h.app,
        Method::POST,
        "/two-factor-challenge",
        &serde_json::json!({ "code": "000000" }),
    );
    assert_eq!(refused.status(), 429);
    let retry_after: u64 = refused
        .header("retry-after")
        .expect("a throttled code answers Retry-After")
        .parse()
        .unwrap();
    assert!((1..=300).contains(&retry_after));
    // The owner's recovery code still signs in.
    let res = submit(&h.app, "recovery_code", &codes[0]);
    assert_eq!(res.header("location"), Some("/dashboard"));
}

#[test]
fn answers_with_plaintext_codes_are_never_cached_nor_flashed_for_json_clients() {
    let h = two_factor_harness();
    signed_in(&h);
    let enable = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-authentication",
        &serde_json::json!({}),
    );
    assert_eq!(enable.status(), 200);
    assert_eq!(enable.header("cache-control"), Some("no-store"));
    assert!(
        h.app.get("/settings-2fa").text().contains("codes=none"),
        "not flashed"
    );
    let fresh = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-recovery-codes",
        &serde_json::json!({}),
    );
    assert_eq!(fresh.header("cache-control"), Some("no-store"));
    assert!(
        h.app.get("/settings-2fa").text().contains("codes=none"),
        "not flashed"
    );
}

#[test]
fn the_pipeline_reads_the_user_again_before_deciding_on_two_factor() {
    let h = harness_with(
        |t| t.two_factor(TwoFactor::new()),
        |b| {
            b.routes(|r| {
                // App code with a user value read before two-factor was enabled.
                r.get(
                    "/stale-login",
                    |ctx: smeltery_temper::TemperCtx| async move {
                        let stale = ctx
                            .app()
                            .find_user(1)
                            .await?
                            .and_then(|u| u.downcast::<User>())
                            .unwrap();
                        enable_two_factor_in(&ctx).await;
                        smeltery_temper::login_pipeline(&ctx, &stale, false).await
                    },
                );
            })
        },
    );
    fake_clock(h.app.app(), T);
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let res = h.app.get("/stale-login");
    assert_eq!(res.header("location"), Some("/two-factor-challenge"));
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

/// Enable and confirm two-factor for user 1 from inside a request (as another device would have).
async fn enable_two_factor_in(ctx: &smeltery_temper::TemperCtx) {
    let row = ctx
        .app()
        .find_user(1)
        .await
        .unwrap()
        .and_then(|u| u.downcast::<User>())
        .unwrap();
    let db = ctx.db().unwrap();
    let sealed = ctx
        .app()
        .encrypt(
            "temper.two-factor",
            b"1",
            b"JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP",
        )
        .unwrap();
    row.update(&db, |m| {
        m.two_factor_secret = Set(Some(sealed));
        m.two_factor_recovery_codes = Set(Some("[]".into()));
        m.two_factor_confirmed_at = Set(Some(smeltery_core::db::prelude::ChronoUtc::now()));
    })
    .await
    .unwrap();
}

#[test]
fn a_confirmed_secret_needs_a_recent_password_even_without_the_guard() {
    let h = harness_with(
        |t| t.two_factor(TwoFactor::new().confirm_password(false)),
        |b| b,
    );
    fake_clock(h.app.app(), T);
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    // Unconfirmed (enrolling): the key is shown.
    h.app.post_form("/user/two-factor-authentication", &[]);
    assert_eq!(h.app.get("/user/two-factor-secret-key").status(), 200);
    assert_eq!(h.app.get("/user/two-factor-qr-code").status(), 200);
    let code = two_factor_code::<User>(&h.app, id);
    h.app.post_form(
        "/user/confirmed-two-factor-authentication",
        &[("code", &code)],
    );
    assert!(user(&h.app, id).two_factor_confirmed_at.is_some());
    // Confirmed: only after a recent password confirmation.
    for path in ["/user/two-factor-secret-key", "/user/two-factor-qr-code"] {
        let res = h.app.request(Method::GET, path, json_headers(), "".into());
        assert_eq!(res.status(), 423, "{path}");
    }
    confirm_password(&h.app, "analytical-engine");
    assert_eq!(h.app.get("/user/two-factor-secret-key").status(), 200);
}

#[test]
fn the_fake_clock_is_for_tests_only() {
    let result = std::panic::catch_unwind(|| {
        let app = futures_free_build();
        fake_clock(&app, 1);
    });
    assert!(result.is_err());
}

/// An app outside `APP_ENV=testing`.
fn futures_free_build() -> smeltery_core::App {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut settings = smeltery_core::config::Settings::from_env();
            settings.env = "production".into();
            AppBuilder::new(settings).build().await.unwrap().app
        })
}

#[test]
fn setup_gives_the_qr_code_and_key_of_the_json_routes_under_the_same_rule() {
    let h = harness_with(
        |t| t.two_factor(TwoFactor::new().confirm_password(false)),
        |b| {
            b.routes(|r| {
                r.get(
                    "/settings-setup",
                    |app: smeltery_core::App, auth: smeltery_core::auth::Auth| async move {
                        let setup = smeltery_temper::two_factor::setup::<User>(&app, &auth).await?;
                        Ok::<_, smeltery_core::Error>(match setup {
                            Some(s) => format!("{}|{}", s.qr_code_url, s.secret_key),
                            None => "none".to_owned(),
                        })
                    },
                );
            })
        },
    );
    fake_clock(h.app.app(), T);
    assert_eq!(h.app.get("/settings-setup").text(), "none", "a guest");
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(h.app.get("/settings-setup").text(), "none", "no enrolment");
    h.app.post_form("/user/two-factor-authentication", &[]);
    let qr = h.app.get("/user/two-factor-qr-code").json()["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let key = h.app.get("/user/two-factor-secret-key").json()["secretKey"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(h.app.get("/settings-setup").text(), format!("{qr}|{key}"));
    let code = two_factor_code::<User>(&h.app, id);
    h.app.post_form(
        "/user/confirmed-two-factor-authentication",
        &[("code", &code)],
    );
    assert!(user(&h.app, id).two_factor_confirmed_at.is_some());
    // Confirmed: only after a recent password confirmation, as on the JSON routes.
    assert_eq!(h.app.get("/settings-setup").text(), "none");
    confirm_password(&h.app, "analytical-engine");
    assert_eq!(h.app.get("/settings-setup").text(), format!("{qr}|{key}"));
}

// ---- login policy and registration (core builder 2) ---------------------------------------------------------------

#[test]
fn the_challenge_asks_the_login_policy_again_before_the_code() {
    let suspended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&suspended);
    let h = harness_with(
        move |t| {
            t.two_factor(TwoFactor::new())
                .login_policy(move |_app, _user: User| {
                    let flag = std::sync::Arc::clone(&flag);
                    async move {
                        Ok(if flag.load(std::sync::atomic::Ordering::SeqCst) {
                            smeltery_core::auth::LoginDecision::refuse(
                                smeltery_core::http::StatusCode::FORBIDDEN,
                                "This account is suspended.",
                            )
                        } else {
                            smeltery_core::auth::LoginDecision::Allow
                        })
                    }
                })
        },
        |b| b,
    );
    fake_clock(h.app.app(), T);
    let (id, codes) = enrolled(&h);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert!(has_pending_two_factor(&h.app));
    // Suspended between the password and the code.
    suspended.store(true, std::sync::atomic::Ordering::SeqCst);
    let res = json_request(
        &h.app,
        Method::POST,
        "/two-factor-challenge",
        &serde_json::json!({ "recovery_code": codes[0] }),
    );
    assert_eq!(res.status(), 403, "{}", res.text());
    assert_eq!(
        res.json()["errors"]["code"][0],
        "This account is suspended."
    );
    assert!(!has_pending_two_factor(&h.app));
    assert_eq!(h.app.get("/settings-2fa").status(), 303, "nobody signed in");
    // The recovery code was not spent.
    let stored = user(&h.app, id).two_factor_recovery_codes.unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&stored).unwrap().len(),
        codes.len()
    );
}

/// A registration action with the classic mistake: it returns the account that already has the address.
struct Upsert;

#[derive(Debug, serde::Deserialize, smeltery::Validate)]
struct UpsertForm {
    #[validate(required, email)]
    email: String,
}

impl smeltery_temper::CreatesNewUsers<User> for Upsert {
    type Input = UpsertForm;

    async fn create(
        &self,
        ctx: &smeltery_temper::TemperCtx,
        input: UpsertForm,
    ) -> smeltery_core::Result<User> {
        smeltery_core::auth::find_by_email::<User>(ctx.app(), &input.email)
            .await?
            .ok_or_else(smeltery_core::Error::not_found)
    }
}

#[test]
fn registration_refuses_an_action_that_returns_an_enrolled_account() {
    let h = harness_with(
        |t| t.two_factor(TwoFactor::new()).registration(Upsert),
        |b| b,
    );
    fake_clock(h.app.app(), T);
    let (id, _) = enrolled(&h);
    let res = h
        .app
        .post_form("/register", &[("email", "ada@example.com")]);
    assert_eq!(res.status(), 500, "{}", res.text());
    assert_eq!(h.app.get("/settings-2fa").status(), 303, "nobody signed in");
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::Registered { .. })),
        0
    );
    // Without an enrolment the same action signs in (a new-looking account; the guard is about 2FA only).
    let bob = create_user(&h.app, "bob@example.com", "bobs-password");
    assert_ne!(bob, id);
    let res = h
        .app
        .post_form("/register", &[("email", "bob@example.com")]);
    assert_eq!(res.status(), 303, "{}", res.text());
    assert_eq!(h.app.get("/settings-2fa").status(), 200);
}

#[test]
fn a_browser_refused_at_the_challenge_sees_the_message_on_the_login_page() {
    let suspended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&suspended);
    let h = harness_with(
        move |t| {
            t.two_factor(TwoFactor::new())
                .login_policy(move |_app, _user: User| {
                    let refuse = flag.load(std::sync::atomic::Ordering::SeqCst);
                    async move {
                        Ok(if refuse {
                            smeltery_core::auth::LoginDecision::refuse(
                                smeltery_core::http::StatusCode::FORBIDDEN,
                                "This account is suspended.",
                            )
                        } else {
                            smeltery_core::auth::LoginDecision::Allow
                        })
                    }
                })
        },
        |b| b,
    );
    fake_clock(h.app.app(), T);
    let (_, codes) = enrolled(&h);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    suspended.store(true, std::sync::atomic::Ordering::SeqCst);
    let res = submit(&h.app, "recovery_code", &codes[0]);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
    let page = h.app.get("/login");
    assert!(
        page.text().contains("This account is suspended."),
        "{}",
        page.text()
    );
}

/// W3-02 (sweep 3): without `password.confirm` on the routes (`confirm_password(false)`), a session alone still
/// cannot touch a confirmed enrolment: no new recovery codes, no turning it off, no enabling over it.
#[test]
fn writes_on_a_confirmed_enrolment_need_a_recent_password_even_without_the_guard() {
    let h = harness_with(
        |t| t.two_factor(TwoFactor::new().confirm_password(false)),
        |b| b,
    );
    fake_clock(h.app.app(), T);
    let (id, _) = enrolled(&h);
    log_in(&h.app, "ada@example.com", "analytical-engine");
    submit(&h.app, "code", &two_factor_code::<User>(&h.app, id));
    assert_eq!(h.app.get("/settings-2fa").status(), 200);
    let before = user(&h.app, id);
    let regenerate = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-recovery-codes",
        &serde_json::json!({}),
    );
    assert_eq!(regenerate.status(), 423);
    assert!(regenerate.json().get("recovery_codes").is_none());
    let disable = json_request(
        &h.app,
        Method::DELETE,
        "/user/two-factor-authentication",
        &serde_json::json!({}),
    );
    assert_eq!(disable.status(), 423);
    // A form goes to the confirmation page.
    let disable = form_request(
        &h.app,
        Method::DELETE,
        "/user/two-factor-authentication",
        &[],
    );
    assert_eq!(disable.status(), 303);
    assert_eq!(disable.header("location"), Some("/user/confirm-password"));
    let enable = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-authentication",
        &serde_json::json!({}),
    );
    assert_eq!(enable.status(), 423);
    let after = user(&h.app, id);
    assert_eq!(after.two_factor_secret, before.two_factor_secret);
    assert_eq!(
        after.two_factor_recovery_codes,
        before.two_factor_recovery_codes
    );
    assert!(after.two_factor_confirmed_at.is_some());
    // With the password, the owner manages it.
    confirm_password(&h.app, "analytical-engine");
    let regenerate = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-recovery-codes",
        &serde_json::json!({}),
    );
    assert_eq!(regenerate.status(), 200);
    let disable = json_request(
        &h.app,
        Method::DELETE,
        "/user/two-factor-authentication",
        &serde_json::json!({}),
    );
    assert_eq!(disable.status(), 200);
    assert!(user(&h.app, id).two_factor_secret.is_none());
}

/// W3-02 (sweep 3): a first enrolment needs no password with `confirm_password(false)` (what the option is for).
#[test]
fn a_first_enrolment_needs_no_password_without_the_guard() {
    let h = harness_with(
        |t| t.two_factor(TwoFactor::new().confirm_password(false)),
        |b| b,
    );
    fake_clock(h.app.app(), T);
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let enabled = json_request(
        &h.app,
        Method::POST,
        "/user/two-factor-authentication",
        &serde_json::json!({}),
    );
    assert_eq!(enabled.status(), 200);
    let key = h.app.request(
        Method::GET,
        "/user/two-factor-secret-key",
        json_headers(),
        "".into(),
    );
    assert_eq!(key.status(), 200);
    let confirmed = json_request(
        &h.app,
        Method::POST,
        "/user/confirmed-two-factor-authentication",
        &serde_json::json!({ "code": two_factor_code::<User>(&h.app, id) }),
    );
    assert_eq!(confirmed.status(), 200);
    assert!(user(&h.app, id).two_factor_confirmed_at.is_some());
}

/// W3-01 (sweep 3): a stolen session cannot move a two-factor account to another mailbox, so the reset that would
/// verify that mailbox (and with it remove the enrolment) never happens.
#[test]
fn a_stolen_session_cannot_move_the_address_and_reset_away_the_second_factor() {
    let h = harness_with(
        |t| t.two_factor(TwoFactor::new()),
        |b| b.verify_email::<User>(),
    );
    fake_clock(h.app.app(), T);
    let (id, _) = enrolled(&h);
    h.app.block_on(async {
        smeltery_core::auth::mark_verified(h.app.app(), id)
            .await
            .unwrap()
    });
    log_in(&h.app, "ada@example.com", "analytical-engine");
    submit(&h.app, "code", &two_factor_code::<User>(&h.app, id));
    assert_eq!(h.app.get("/settings-2fa").status(), 200);
    let res = form_request(
        &h.app,
        Method::PUT,
        "/user/profile-information",
        &[("name", "x"), ("email", "attacker@evil.example")],
    );
    assert_eq!(res.header("location"), Some("/user/confirm-password"));
    h.app.post_form("/logout", &[]);
    h.app
        .post_form("/forgot-password", &[("email", "attacker@evil.example")]);
    assert!(h.outbox.resets().is_empty(), "no account has that address");
    let ada = user(&h.app, id);
    assert_eq!(ada.email, "ada@example.com");
    assert!(ada.two_factor_confirmed_at.is_some());
}
