//! The authentication invariants of SECURITY.md, proved through Temper's routes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::*;
use smeltery_core::http::Method;
use smeltery_core::session::Session;
use smeltery_temper::TemperEvent;
use smeltery_temper::testing::log_in;

#[test]
fn registration_and_login_use_the_normalized_address() {
    let h = harness();
    assert_eq!(
        register(&h.app, " Ada@Example.COM ").header("location"),
        Some("/dashboard")
    );
    assert_eq!(user(&h.app, 1).email, "ada@example.com");
    h.app.post_form("/logout", &[]);
    assert_eq!(
        log_in(&h.app, "ADA@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
    h.app.post_form("/logout", &[]);
    register(&h.app, "ada@EXAMPLE.com");
    assert!(
        h.app
            .get("/register")
            .text()
            .contains("The email has already been taken.")
    );
}

#[test]
fn the_reset_flow_mails_the_stored_address_and_changes_the_account_by_id() {
    let h = harness();
    let ada = create_user(&h.app, "ada@example.com", "analytical-engine");
    let other = create_user(&h.app, "bob@example.com", "bobs-password");
    h.app
        .post_form("/forgot-password", &[("email", " ADA@example.com ")]);
    let sent = h.outbox.resets();
    assert_eq!(sent.len(), 1);
    // The stored address, never the typed one.
    assert_eq!(sent[0].0, "ada@example.com");
    let link = h.outbox.last_reset_path();
    let res = h.app.post_form(
        link.split('?').next().unwrap(),
        &[
            ("email", "ada@example.com"),
            ("password", "a-new-password"),
            ("password_confirmation", "a-new-password"),
        ],
    );
    assert_eq!(res.header("location"), Some("/login"));
    assert_eq!(*h.resets.calls.lock().unwrap(), vec![ada]);
    // Only Ada's row changed.
    let bob_hash = user(&h.app, other).password;
    assert_eq!(
        log_in(&h.app, "bob@example.com", "bobs-password").header("location"),
        Some("/dashboard")
    );
    assert_eq!(user(&h.app, other).password, bob_hash);
    // An address without an account gets the same answer and no mail.
    h.app.post_form("/logout", &[]);
    let res = h
        .app
        .post_form("/forgot-password", &[("email", "nobody@example.com")]);
    assert_eq!(res.header("location"), Some("/forgot-password"));
    assert_eq!(h.outbox.resets().len(), 1);
}

#[test]
fn every_form_route_carries_its_throttle() {
    let h = harness();
    let routes = h.app.app().routes();
    let middleware = |name: &str| {
        routes
            .iter()
            .find(|r| r.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("route {name}"))
            .middleware
            .clone()
    };
    assert_eq!(middleware("login.store"), ["guest", "throttle:30,1"]);
    for name in ["register.store", "password.email", "password.update"] {
        assert_eq!(middleware(name), ["guest", "throttle:6,1"], "{name}");
    }
    for name in [
        "verification.verify",
        "user-password.update",
        "password.confirm.store",
    ] {
        assert_eq!(middleware(name), ["auth", "throttle:6,1"], "{name}");
    }
    // The address decides where reset links go: changing it needs the password again (W3-01).
    assert_eq!(
        middleware("user-profile-information.update"),
        ["auth", "password.confirm", "throttle:6,1"]
    );
    // Core counts verification mails per user itself.
    assert_eq!(middleware("verification.send"), ["auth"]);
    for name in ["login", "register", "password.request", "password.reset"] {
        assert_eq!(middleware(name), ["guest"], "{name}");
    }
    for name in [
        "logout",
        "verification.notice",
        "password.confirm",
        "password.confirmation",
    ] {
        assert_eq!(middleware(name), ["auth"], "{name}");
    }
    // No Temper route is an API route (they need sessions and CSRF).
    assert!(routes.iter().all(|r| !r.api || r.path == "/up"));
}

#[test]
fn forgot_password_is_throttled() {
    let h = harness();
    // `throttle:6,1` allows 6 a minute per client and route; 13 requests fill one minute even across its end.
    let refused = (0..13).any(|_| {
        assert_eq!(
            h.app
                .post_form("/forgot-password", &[("email", "nobody@example.com")])
                .status(),
            303
        );
        h.app
            .get("/forgot-password")
            .text()
            .contains("Too many attempts")
    });
    assert!(refused, "the forgot-password form is throttled");
}

#[test]
fn parallel_logins_through_temper_get_exactly_five_password_checks() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let body = serde_json::json!({ "email": "ada@example.com", "password": "a-guess" });
    let statuses: Vec<u16> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..20)
            .map(|_| s.spawn(|| json_request(&h.app, Method::POST, "/login", &body).status()))
            .collect();
        handles.into_iter().map(|t| t.join().unwrap()).collect()
    });
    // Five wrong passwords were checked; the other fifteen were refused before any check.
    assert_eq!(
        statuses.iter().filter(|s| **s == 422).count(),
        5,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == 429).count(),
        15,
        "{statuses:?}"
    );
    assert_eq!(
        h.events.count(|e| matches!(e, TemperEvent::Failed { .. })),
        5
    );
    assert_eq!(
        h.events.count(|e| matches!(e, TemperEvent::Lockout { .. })),
        15
    );
    // Even the right password waits now.
    assert_eq!(
        json_request(
            &h.app,
            Method::POST,
            "/login",
            &serde_json::json!({ "email": "ada@example.com", "password": "analytical-engine" })
        )
        .status(),
        429
    );
}

/// Sign in as a new "device": a fresh cookie jar; returns that device's cookies.
fn device(h: &Harness, email: &str, password: &str) -> String {
    h.app.clear_cookies();
    assert_eq!(
        log_in(&h.app, email, password).header("location"),
        Some("/dashboard")
    );
    h.app
        .cookie(&session_cookie(&h.app))
        .expect("a session cookie")
}

fn use_device(h: &Harness, cookie: &str) {
    h.app.clear_cookies();
    h.app.set_cookie(&session_cookie(&h.app), cookie);
}

#[test]
fn updating_the_password_keeps_this_device_and_signs_out_the_others() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let laptop = device(&h, "ada@example.com", "analytical-engine");
    let phone = device(&h, "ada@example.com", "analytical-engine");
    use_device(&h, &laptop);
    let res = form_request(
        &h.app,
        Method::PUT,
        "/user/password",
        &[
            ("current_password", "analytical-engine"),
            ("password", "a-new-password"),
            ("password_confirmation", "a-new-password"),
        ],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(h.app.get("/dashboard").status(), 200, "this device stays");
    use_device(&h, &phone);
    assert_eq!(
        h.app.get("/dashboard").status(),
        303,
        "the other device is signed out"
    );
}

#[test]
fn a_reset_through_temper_signs_out_every_session() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let laptop = device(&h, "ada@example.com", "analytical-engine");
    h.app.clear_cookies();
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
    use_device(&h, &laptop);
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

#[test]
fn signing_in_through_temper_regenerates_the_session_and_its_csrf_secret() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let before = h.app.get("/_session").text();
    let res = log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(res.status(), 303);
    let after = h.app.get("/_session").text();
    let (id_before, token_before) = before.split_once('|').unwrap();
    let (id_after, token_after) = after.split_once('|').unwrap();
    assert_ne!(id_before, id_after);
    assert_ne!(token_before, token_after);
}

#[test]
fn login_follows_only_a_local_intended_page() {
    let h = harness_with(
        |t| t,
        |b| {
            b.routes(|r| {
                // A forged remembered page (as if planted in the session).
                r.get("/plant", |session: Session| async move {
                    session.insert("_intended_url", "//evil.example/x");
                    "planted"
                });
            })
        },
    );
    create_user(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(
        h.app.get("/dashboard?tab=2").header("location"),
        Some("/login")
    );
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").header("location"),
        Some("/dashboard?tab=2")
    );
    h.app.post_form("/logout", &[]);
    h.app.get("/plant");
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
}

#[test]
fn the_code_field_is_never_flashed_as_old_input() {
    let h = harness();
    // A failed validation (no password) with a `code` field.
    let res = h.app.post_form(
        "/login",
        &[
            ("email", "ada@example.com"),
            ("code", "123456"),
            ("password", ""),
        ],
    );
    assert_eq!(res.status(), 303);
    let page = h.app.get("/login").text();
    assert!(page.contains("old_email=ada@example.com"), "{page}");
    assert!(page.contains("old_code=|"), "{page}");
}

#[test]
fn temper_forms_need_the_csrf_token() {
    let h = harness();
    let app = h.app.with_csrf();
    create_user(&app, "ada@example.com", "analytical-engine");
    assert_eq!(
        log_in(&app, "ada@example.com", "analytical-engine").status(),
        419
    );
    assert_eq!(
        app.post_form("/forgot-password", &[("email", "ada@example.com")])
            .status(),
        419
    );
}

#[test]
fn a_confirmation_does_not_survive_a_new_sign_in() {
    let h = harness_with(
        |t| t,
        |b| {
            b.routes(|r| {
                // Keys a sign-in owns, planted in a guest session (as if left from an earlier sign-in).
                r.get("/plant-keys", |session: Session| async move {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    session.insert("_auth.confirmed_at", now);
                    session.insert("_temper.left_over", "x");
                    "planted"
                });
                r.get("/temper-key", |session: Session| async move {
                    session.has("_temper.left_over").to_string()
                });
            })
        },
    );
    create_user(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(h.app.get("/plant-keys").text(), "planted");
    assert_eq!(h.app.get("/temper-key").text(), "true");
    // No logout in between: only the sign-in itself may drop them.
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").status(),
        303
    );
    assert_eq!(h.app.get("/secret").status(), 303);
    assert_eq!(
        h.app.get("/user/confirmed-password-status").json(),
        serde_json::json!({ "confirmed": false })
    );
    assert_eq!(h.app.get("/temper-key").text(), "false");
}

#[test]
fn events_never_carry_secrets() {
    let h = harness_verifying();
    register(&h.app, "ada@example.com");
    h.app.get(&h.outbox.last_verification_path());
    h.app.post_form("/logout", &[]);
    log_in(&h.app, "ada@example.com", "wrong-password");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    smeltery_temper::testing::confirm_password(&h.app, "analytical-engine");
    let all = serde_json::to_string(&h.events.events()).unwrap();
    for secret in ["ada@", "analytical-engine", "wrong-password"] {
        assert!(!all.contains(secret), "{secret} in {all}");
    }
    assert!(all.contains(r#""type":"registered""#), "{all}");
}

/// W3-01 (sweep 3): the address decides where reset links go, so a session alone (a stolen cookie, an unattended
/// device) cannot change it: every answer mode is sent to the password confirmation first, and nothing changes.
#[test]
fn an_address_change_needs_a_recent_password_confirmation() {
    let h = harness();
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let fields = [("name", "Mallory"), ("email", "mallory@evil.example")];
    // A form goes to the confirmation page.
    let res = form_request(&h.app, Method::PUT, "/user/profile-information", &fields);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/user/confirm-password"));
    // So does an Inertia visit.
    let mut inertia = smeltery_core::http::HeaderMap::new();
    inertia.insert("x-inertia", "true".parse().unwrap());
    inertia.insert(
        smeltery_core::http::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    let body = serde_json::json!({ "name": "Mallory", "email": "mallory@evil.example" });
    let res = h.app.request(
        Method::PUT,
        "/user/profile-information",
        inertia,
        body.to_string().into(),
    );
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/user/confirm-password"));
    // A JSON client gets 423.
    let res = json_request(&h.app, Method::PUT, "/user/profile-information", &body);
    assert_eq!(res.status(), 423);
    let ada = user(&h.app, id);
    assert_eq!(ada.email, "ada@example.com");
    assert_eq!(ada.name, "Ada Lovelace");
    // After the password: the change goes through.
    smeltery_temper::testing::confirm_password(&h.app, "analytical-engine");
    let res = json_request(&h.app, Method::PUT, "/user/profile-information", &body);
    assert_eq!(res.status(), 200);
    assert_eq!(user(&h.app, id).email, "mallory@evil.example");
}

/// W8-01 (sweep 8): the reset page puts the link's token into its form's `action`. A path segment that is no reset
/// token (`abc%2F..%2F..%2Flogin`, decoded by the router) is an invalid link, never a page whose form posts elsewhere.
#[test]
fn a_reset_link_with_a_token_of_another_shape_is_invalid() {
    let h = harness();
    let res = h
        .app
        .get("/reset-password/abc%2F..%2F..%2Flogin?email=a%40b.example");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/forgot-password"));
    assert!(
        h.app
            .get("/forgot-password")
            .text()
            .contains("error=This password reset link is invalid or has expired.")
    );
    let short = h.app.get("/reset-password/abc");
    assert_eq!(short.header("location"), Some("/forgot-password"));
    // A real link shows its page with the token.
    create_user(&h.app, "ada@example.com", "analytical-engine");
    h.app
        .post_form("/forgot-password", &[("email", "ada@example.com")]);
    let link = h.outbox.last_reset_path();
    let page = h.app.get(&link);
    assert_eq!(page.status(), 200);
    let token = link.split('?').next().unwrap().rsplit('/').next().unwrap();
    assert!(page.text().contains(&format!("token={token}")));
}
