//! Temper's flows through its routes: every route with its feature on and off, browser / Inertia / JSON answers,
//! actions with validated input, events, listeners, the login pipeline and the set-up options.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::sync::atomic::Ordering;

use common::*;
use smeltery_core::auth::{Auth, AuthUser};
use smeltery_core::http::{HeaderMap, Method};
use smeltery_core::session::Session;
use smeltery_core::testing::TestApp;
use smeltery_core::{App, AppBuilder, Error};
use smeltery_temper::testing::{EventRecorder, confirm_password, log_in};
use smeltery_temper::{
    CreatesNewUsers, PipelineStep, SocialUser, Temper, TemperCtx, TemperEvent, TemperExt as _,
    TemperResponses, TemperViews,
};

fn login_events(events: &EventRecorder) -> usize {
    events.count(|e| matches!(e, TemperEvent::Login { .. }))
}

// ---- login and logout ---------------------------------------------------------------------------------------------

#[test]
fn the_login_page_renders_and_a_login_opens_home() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let page = h.app.get("/login");
    assert_eq!(page.status(), 200);
    assert!(page.text().starts_with("page=login"));
    let res = log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    assert_eq!(h.app.get("/dashboard").status(), 200);
    // `guest` sends a signed-in user away from the login page.
    assert_eq!(h.app.get("/login").status(), 303);
    assert_eq!(login_events(&h.events), 1);
    let login = h.events.events().pop().unwrap();
    assert_eq!(login.user_id(), Some(1));
    assert_eq!(login.name(), "login");
}

#[test]
fn a_wrong_password_returns_to_the_login_page_with_the_message_on_email() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let res = log_in(&h.app, "Ada@Example.com", "wrong-password");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
    let page = h.app.get("/login").text();
    assert!(
        page.contains(r#"{"email":["These credentials do not match our records."]}"#),
        "{page}"
    );
    assert!(page.contains("old_email=ada@example.com"), "{page}");
    assert_eq!(h.app.get("/dashboard").status(), 303);
    let failed = h.events.events();
    assert_eq!(failed.len(), 1);
    let TemperEvent::Failed { email_hash, .. } = &failed[0] else {
        panic!("{failed:?}")
    };
    // A keyed hash of the address, never the address.
    assert_eq!(
        email_hash,
        &h.app
            .app()
            .sign("temper.email-hash", b"ada@example.com")
            .unwrap()
    );
    assert!(!format!("{failed:?}").contains("ada@"));
}

#[test]
fn inertia_visits_get_the_browser_answers() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let mut headers = HeaderMap::new();
    headers.insert("x-inertia", "true".parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    let wrong = serde_json::json!({ "email": "ada@example.com", "password": "nope" });
    let res = h.app.request(
        Method::POST,
        "/login",
        headers.clone(),
        wrong.to_string().into(),
    );
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
    assert!(h.app.get("/login").text().contains("These credentials"));
    let right = serde_json::json!({ "email": "ada@example.com", "password": "analytical-engine" });
    let res = h
        .app
        .request(Method::POST, "/login", headers, right.to_string().into());
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
}

#[test]
fn json_clients_get_json_answers_for_login_and_logout() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let wrong = json_request(
        &h.app,
        Method::POST,
        "/login",
        &serde_json::json!({ "email": "ada@example.com", "password": "nope" }),
    );
    assert_eq!(wrong.status(), 422);
    assert_eq!(
        wrong.json()["errors"]["email"][0],
        "These credentials do not match our records."
    );
    let res = json_request(
        &h.app,
        Method::POST,
        "/login",
        &serde_json::json!({ "email": "ada@example.com", "password": "analytical-engine" }),
    );
    assert_eq!(res.status(), 200);
    assert_eq!(res.json(), serde_json::json!({ "two_factor": false }));
    let res = json_request(&h.app, Method::POST, "/logout", &serde_json::json!({}));
    assert_eq!(res.status(), 204);
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

#[test]
fn users_can_log_out() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let res = h.app.post_form("/logout", &[]);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/"));
    assert_eq!(h.app.get("/dashboard").status(), 303);
    assert_eq!(
        h.events.count(|e| matches!(e, TemperEvent::Logout { .. })),
        1
    );
    // A guest cannot log out.
    assert_eq!(
        h.app.post_form("/logout", &[]).header("location"),
        Some("/login")
    );
}

// ---- registration -------------------------------------------------------------------------------------------------

#[test]
fn registration_runs_the_action_signs_in_and_fires_registered_once() {
    let h = harness();
    let res = register(&h.app, "ada@example.com");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    assert_eq!(h.app.get("/dashboard").status(), 200);
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::Registered { .. })),
        1
    );
    assert_eq!(user(&h.app, 1).name, "Ada Lovelace");
    // Without `.verify_email::<User>()` no link goes out.
    assert!(h.outbox.verifications().is_empty());
}

#[test]
fn invalid_registrations_never_reach_the_action() {
    let h = harness();
    let res = h.app.post_form(
        "/register",
        &[
            ("name", "Ada"),
            ("email", "not-an-address"),
            ("password", "short"),
            ("password_confirmation", "other"),
        ],
    );
    assert_eq!(res.status(), 303);
    let db = h.app.db();
    let count = h.app.block_on(async {
        <User as smeltery_core::db::Record>::count(&db)
            .await
            .unwrap()
    });
    assert_eq!(count, 0);
    assert!(h.events.events().is_empty());
    let json = json_request(
        &h.app,
        Method::POST,
        "/register",
        &serde_json::json!({ "name": "Ada", "email": "ada@example.com" }),
    );
    assert_eq!(json.status(), 422);
    assert!(json.json()["errors"]["password"].is_array());
}

#[test]
fn json_registration_answers_201() {
    let h = harness();
    let res = json_request(
        &h.app,
        Method::POST,
        "/register",
        &serde_json::json!({
            "name": "Ada", "email": "ada@example.com",
            "password": "analytical-engine", "password_confirmation": "analytical-engine"
        }),
    );
    assert_eq!(res.status(), 201);
    assert_eq!(h.app.get("/dashboard").status(), 200);
}

// ---- features off -------------------------------------------------------------------------------------------------

#[test]
fn routes_of_features_that_are_off_do_not_exist() {
    let outbox = Outbox::default();
    let mail = outbox.clone();
    let app = TestApp::new(move |b| {
        base(b, &mail).temper(
            Temper::<User>::new().views(
                TemperViews::new()
                    .login(|_| "login")
                    .confirm_password(|_| "confirm"),
            ),
        )
    });
    create_user(&app, "ada@example.com", "analytical-engine");
    for path in [
        "/register",
        "/forgot-password",
        "/reset-password/abc",
        "/email/verify",
        "/email/verify/1/abc",
    ] {
        assert_eq!(app.get(path).status(), 404, "GET {path}");
    }
    for path in [
        "/register",
        "/forgot-password",
        "/reset-password/abc",
        "/email/verification-notification",
    ] {
        assert_eq!(app.post_form(path, &[]).status(), 404, "POST {path}");
    }
    for path in ["/user/profile-information", "/user/password"] {
        assert_eq!(
            form_request(&app, Method::PUT, path, &[]).status(),
            404,
            "PUT {path}"
        );
    }
    // The routes that are always on.
    assert_eq!(app.get("/login").status(), 200);
    log_in(&app, "ada@example.com", "analytical-engine");
    assert_eq!(app.get("/user/confirm-password").status(), 200);
    assert_eq!(app.get("/user/confirmed-password-status").status(), 200);
    assert_eq!(app.post_form("/logout", &[]).status(), 303);
    let names: Vec<_> = app
        .app()
        .routes()
        .iter()
        .filter_map(|r| r.name.clone())
        .collect();
    for name in [
        "login",
        "login.store",
        "logout",
        "password.confirm",
        "password.confirm.store",
        "password.confirmation",
    ] {
        assert!(names.iter().any(|n| n == name), "{name}");
    }
    assert!(!names.iter().any(|n| n == "register"));
}

#[test]
fn every_route_of_every_feature_exists_when_it_is_on() {
    let h = harness();
    let names: Vec<_> = h
        .app
        .app()
        .routes()
        .iter()
        .filter_map(|r| r.name.clone())
        .collect();
    for name in [
        "login",
        "login.store",
        "logout",
        "register",
        "register.store",
        "password.request",
        "password.email",
        "password.reset",
        "password.update",
        "verification.notice",
        "verification.verify",
        "verification.send",
        "user-profile-information.update",
        "user-password.update",
        "password.confirm",
        "password.confirm.store",
        "password.confirmation",
    ] {
        assert!(names.iter().any(|n| n == name), "{name}");
    }
}

// ---- password reset -----------------------------------------------------------------------------------------------

#[test]
fn the_reset_flow_sets_a_new_password_and_runs_the_action_once() {
    let h = harness();
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    let res = h
        .app
        .post_form("/forgot-password", &[("email", "ada@example.com")]);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/forgot-password"));
    assert!(
        h.app
            .get("/forgot-password")
            .text()
            .contains("status=If that e-mail address has an account")
    );
    let link = h.outbox.last_reset_path();
    assert!(link.starts_with("/reset-password/"), "{link}");
    let page = h.app.get(&link).text();
    assert!(page.starts_with("page=reset-password"), "{page}");
    assert!(page.contains("|email=ada@example.com"), "{page}");
    let token = link
        .trim_start_matches("/reset-password/")
        .split('?')
        .next()
        .unwrap();
    assert!(page.contains(&format!("|token={token}|")), "{page}");
    let action = link.split('?').next().unwrap();
    let res = h.app.post_form(
        action,
        &[
            ("email", "ada@example.com"),
            ("password", "a-new-password"),
            ("password_confirmation", "a-new-password"),
        ],
    );
    assert_eq!(res.header("location"), Some("/login"));
    assert!(
        h.app
            .get("/login")
            .text()
            .contains("status=Your password has been reset")
    );
    assert_eq!(*h.resets.calls.lock().unwrap(), vec![id]);
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::PasswordReset { .. })),
        1
    );
    assert_eq!(
        log_in(&h.app, "ada@example.com", "a-new-password").header("location"),
        Some("/dashboard")
    );
    // The link is used up.
    h.app.post_form("/logout", &[]);
    let again = h.app.post_form(
        action,
        &[
            ("email", "ada@example.com"),
            ("password", "another-password"),
            ("password_confirmation", "another-password"),
        ],
    );
    assert_eq!(again.header("location"), Some("/forgot-password"));
    assert!(
        h.app
            .get("/forgot-password")
            .text()
            .contains("error=This password reset link is invalid or has expired.")
    );
    assert_eq!(h.resets.calls.lock().unwrap().len(), 1);
}

#[test]
fn json_clients_get_json_answers_for_resets() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let res = json_request(
        &h.app,
        Method::POST,
        "/forgot-password",
        &serde_json::json!({ "email": "ada@example.com" }),
    );
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.json()["message"],
        "If that e-mail address has an account, a password reset link has been sent to it."
    );
    let bad = json_request(
        &h.app,
        Method::POST,
        "/reset-password/not-the-token",
        &serde_json::json!({
            "email": "ada@example.com", "password": "a-new-password", "password_confirmation": "a-new-password"
        }),
    );
    assert_eq!(bad.status(), 422);
    assert!(bad.json()["errors"]["email"].is_array());
    let link = h.outbox.last_reset_path();
    let good = json_request(
        &h.app,
        Method::POST,
        link.split('?').next().unwrap(),
        &serde_json::json!({
            "email": "ada@example.com", "password": "a-new-password", "password_confirmation": "a-new-password"
        }),
    );
    assert_eq!(good.status(), 200);
    assert!(good.json()["message"].is_string());
}

// ---- e-mail verification ------------------------------------------------------------------------------------------

#[test]
fn the_verification_flow_follows_bootstrap() {
    // Without `.verify_email::<User>()`: no link, the notice sends users home.
    let h = harness();
    register(&h.app, "ada@example.com");
    assert!(h.outbox.verifications().is_empty());
    assert_eq!(h.app.get("/dashboard").status(), 200);
    let res = h.app.get("/email/verify");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));

    // With it: the link after registration, the notice, the link opens the dashboard once.
    let h = harness_verifying();
    register(&h.app, "ada@example.com");
    assert_eq!(h.outbox.verifications().len(), 1);
    assert_eq!(h.outbox.verifications()[0].0, "ada@example.com");
    assert_eq!(
        h.app.get("/dashboard").header("location"),
        Some("/email/verify")
    );
    assert!(
        h.app
            .get("/email/verify")
            .text()
            .starts_with("page=verify-email")
    );
    let link = h.outbox.last_verification_path();
    let res = h.app.get(&link);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    let dashboard = h.app.get("/dashboard");
    assert_eq!(dashboard.status(), 200);
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::Verified { .. })),
        1
    );
    // A second click: home, no second event.
    assert_eq!(h.app.get(&link).header("location"), Some("/dashboard"));
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::Verified { .. })),
        1
    );
}

#[test]
fn the_verification_link_can_be_sent_again() {
    let h = harness_verifying();
    register(&h.app, "ada@example.com");
    let res = h.app.post_form("/email/verification-notification", &[]);
    assert_eq!(res.status(), 303);
    assert!(
        h.app
            .get("/email/verify")
            .text()
            .contains("status=A new verification link has been sent")
    );
    assert_eq!(h.outbox.verifications().len(), 2);
    let json = json_request(
        &h.app,
        Method::POST,
        "/email/verification-notification",
        &serde_json::json!({}),
    );
    assert_eq!(json.status(), 202);
    // Core's six a minute per user.
    let refused = (0..8).any(|_| {
        json_request(
            &h.app,
            Method::POST,
            "/email/verification-notification",
            &serde_json::json!({}),
        )
        .status()
            == 429
    });
    assert!(refused);
}

// ---- password confirmation ----------------------------------------------------------------------------------------

#[test]
fn password_confirmation_guards_a_page_and_returns_to_it() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let res = h.app.get("/secret");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/user/confirm-password"));
    assert_eq!(
        h.app.get("/user/confirmed-password-status").json(),
        serde_json::json!({ "confirmed": false })
    );
    assert!(
        h.app
            .get("/user/confirm-password")
            .text()
            .starts_with("page=confirm-password")
    );
    let wrong = confirm_password(&h.app, "wrong-password");
    assert_eq!(wrong.status(), 303);
    assert!(
        h.app
            .get("/user/confirm-password")
            .text()
            .contains("The provided password was incorrect.")
    );
    let res = confirm_password(&h.app, "analytical-engine");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/secret"));
    assert_eq!(h.app.get("/secret").status(), 200);
    assert_eq!(
        h.app.get("/user/confirmed-password-status").json(),
        serde_json::json!({ "confirmed": true })
    );
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::PasswordConfirmed { .. })),
        1
    );
    let json = json_request(
        &h.app,
        Method::POST,
        "/user/confirm-password",
        &serde_json::json!({ "password": "analytical-engine" }),
    );
    assert_eq!(json.status(), 201);
}

#[test]
fn the_confirmation_routes_need_a_signed_in_user() {
    let h = harness();
    assert_eq!(
        h.app.get("/user/confirm-password").header("location"),
        Some("/login")
    );
    assert_eq!(
        confirm_password(&h.app, "x").header("location"),
        Some("/login")
    );
    let status = h.app.request(
        Method::GET,
        "/user/confirmed-password-status",
        json_headers(),
        "".into(),
    );
    assert_eq!(status.status(), 401);
}

// ---- profile and password updates ---------------------------------------------------------------------------------

#[test]
fn users_can_update_their_profile() {
    let h = harness();
    let id = create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    confirm_password(&h.app, "analytical-engine");
    let res = form_request(
        &h.app,
        Method::PUT,
        "/user/profile-information",
        &[("name", "Ada King"), ("email", "ada@example.com")],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(user(&h.app, id).name, "Ada King");
    assert!(h.outbox.verifications().is_empty());
    let updated: Vec<_> = h
        .events
        .events()
        .into_iter()
        .filter(|e| matches!(e, TemperEvent::ProfileUpdated { .. }))
        .collect();
    assert_eq!(updated.len(), 1);
    assert!(matches!(
        updated[0],
        TemperEvent::ProfileUpdated {
            email_changed: false,
            ..
        }
    ));
    // Invalid input never reaches the action.
    let bad = json_request(
        &h.app,
        Method::PUT,
        "/user/profile-information",
        &serde_json::json!({ "name": "", "email": "nope" }),
    );
    assert_eq!(bad.status(), 422);
    assert_eq!(user(&h.app, id).name, "Ada King");
    let ok = json_request(
        &h.app,
        Method::PUT,
        "/user/profile-information",
        &serde_json::json!({ "name": "Ada", "email": "ada@example.com" }),
    );
    assert_eq!(ok.status(), 200);
}

#[test]
fn a_new_address_is_unverified_and_gets_a_link() {
    let h = harness_verifying();
    register(&h.app, "ada@example.com");
    let link = h.outbox.last_verification_path();
    h.app.get(&link);
    assert!(user(&h.app, 1).email_verified_at.is_some());
    confirm_password(&h.app, "analytical-engine");
    let res = form_request(
        &h.app,
        Method::PUT,
        "/user/profile-information",
        &[("name", "Ada"), ("email", "Ada@Lovelace.example")],
    );
    assert_eq!(res.status(), 303);
    let ada = user(&h.app, 1);
    assert_eq!(ada.email, "ada@lovelace.example");
    assert!(ada.email_verified_at.is_none());
    let sent = h.outbox.verifications();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].0, "ada@lovelace.example");
    assert_eq!(
        h.app.get("/dashboard").header("location"),
        Some("/email/verify")
    );
}

#[test]
fn users_can_change_their_password_and_stay_signed_in() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let wrong = json_request(
        &h.app,
        Method::PUT,
        "/user/password",
        &serde_json::json!({
            "current_password": "wrong", "password": "a-new-password", "password_confirmation": "a-new-password"
        }),
    );
    assert_eq!(wrong.status(), 422);
    assert_eq!(
        wrong.json()["errors"]["current_password"][0],
        "The provided password does not match your current password."
    );
    assert_eq!(h.password_updates.calls.load(Ordering::SeqCst), 0);
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
    assert_eq!(h.app.get("/dashboard").status(), 200, "still signed in");
    assert_eq!(h.password_updates.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::PasswordUpdated { .. })),
        1
    );
    h.app.post_form("/logout", &[]);
    assert_eq!(
        log_in(&h.app, "ada@example.com", "a-new-password").header("location"),
        Some("/dashboard")
    );
}

#[test]
fn wrong_current_passwords_are_budgeted_on_the_field() {
    // Core's five confirmations a minute per user; the sixth is refused before the check, on the form's field.
    // Budgets count in fixed windows: a run that crosses a window's end is repeated on a fresh app.
    for attempt in 0..3 {
        let h = harness();
        create_user(&h.app, "ada@example.com", "analytical-engine");
        log_in(&h.app, "ada@example.com", "analytical-engine");
        let body = serde_json::json!({
            "current_password": "wrong", "password": "a-new-password", "password_confirmation": "a-new-password"
        });
        let answers: Vec<_> = (0..6)
            .map(|_| json_request(&h.app, Method::PUT, "/user/password", &body))
            .collect();
        let statuses: Vec<u16> = answers.iter().map(|a| a.status()).collect();
        if statuses != [422, 422, 422, 422, 422, 429] && attempt < 2 {
            continue;
        }
        assert_eq!(statuses, [422, 422, 422, 422, 422, 429]);
        let limited = answers.last().unwrap();
        assert!(
            limited.json()["errors"]["current_password"].is_array(),
            "{}",
            limited.text()
        );
        return;
    }
}

// ---- listeners, responses and the pipeline ------------------------------------------------------------------------

#[test]
fn listeners_never_change_the_answer() {
    let recorder = EventRecorder::new();
    let outbox = Outbox::default();
    let mail = outbox.clone();
    let rec = recorder.clone();
    let app = TestApp::new(move |b| {
        base(b, &mail).temper(
            Temper::<User>::new()
                .views(views())
                .listen(|_, _| async { Err(Error::internal("a failing listener")) })
                .listen(|_, event: TemperEvent| async move {
                    if event.name() == "login" {
                        panic!("a panicking listener");
                    }
                    Ok(())
                })
                .listen(rec.listener()),
        )
    });
    create_user(&app, "ada@example.com", "analytical-engine");
    let res = log_in(&app, "ada@example.com", "analytical-engine");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    assert_eq!(app.get("/dashboard").status(), 200);
    // The listeners after the failing ones still ran.
    assert_eq!(login_events(&recorder), 1);
}

struct Goodbye;

impl TemperResponses for Goodbye {
    fn logout(&self, ctx: &TemperCtx) -> smeltery_core::Result<smeltery_core::Response> {
        use smeltery_core::http::{IntoResponse, Redirect};
        ctx.session().flash("status", "bye");
        Ok(Redirect::to("/goodbye").into_response())
    }
}

#[test]
fn an_app_answers_one_outcome_its_own_way() {
    let h = harness_with(|t| t.responses(Goodbye), |b| b);
    create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    let res = h.app.post_form("/logout", &[]);
    assert_eq!(res.header("location"), Some("/goodbye"));
    // The other outcomes keep their defaults.
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
}

#[test]
fn a_pipeline_step_can_refuse_or_answer_a_login() {
    let h = harness_with(
        |t| {
            t.login_pipeline(|_ctx, user: User| async move {
                if user.email.starts_with("banned") {
                    return Err(Error::validation("email", "This account is closed."));
                }
                Ok(PipelineStep::Continue)
            })
            .login_pipeline(|_ctx, user: User| async move {
                if user.email.starts_with("teapot") {
                    use smeltery_core::http::{IntoResponse, StatusCode};
                    return Ok(PipelineStep::Respond(
                        StatusCode::IM_A_TEAPOT.into_response(),
                    ));
                }
                Ok(PipelineStep::Continue)
            })
        },
        |b| b,
    );
    create_user(&h.app, "banned@example.com", "analytical-engine");
    create_user(&h.app, "teapot@example.com", "analytical-engine");
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let res = json_request(
        &h.app,
        Method::POST,
        "/login",
        &serde_json::json!({ "email": "banned@example.com", "password": "analytical-engine" }),
    );
    assert_eq!(res.status(), 422);
    assert_eq!(res.json()["errors"]["email"][0], "This account is closed.");
    assert_eq!(h.app.get("/dashboard").status(), 303);
    let res = log_in(&h.app, "teapot@example.com", "analytical-engine");
    assert_eq!(res.status(), 418);
    assert_eq!(h.app.get("/dashboard").status(), 303);
    assert_eq!(login_events(&h.events), 0);
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
    assert_eq!(login_events(&h.events), 1);
}

/// A social-login callback stand-in: signs user 1 in through the service, knowing only core types.
async fn callback(
    app: App,
    auth: Auth,
    session: Session,
    headers: HeaderMap,
) -> smeltery_core::Result<smeltery_core::Response> {
    let completion = app
        .login_completion()
        .ok_or_else(|| Error::internal("no login pipeline"))?;
    let user: AuthUser = app.find_user(1).await?.ok_or_else(Error::not_found)?;
    completion
        .complete(&app, &auth, &session, &headers, user, false)
        .await
}

#[test]
fn the_login_pipeline_is_a_service_for_other_sign_in_paths() {
    let h = harness_with(
        |t| t,
        |b| {
            b.routes(|r| {
                r.get("/auth/callback", callback);
            })
        },
    );
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let res = h.app.get("/auth/callback");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    assert_eq!(h.app.get("/dashboard").status(), 200);
    assert_eq!(login_events(&h.events), 1);
}

#[test]
fn create_social_refuses_by_default() {
    let h = harness_with(
        |t| t,
        |b| {
            b.routes(|r| {
                r.get("/social", |ctx: TemperCtx| async move {
                    let user = SocialUser::new("github", "7", " Ada@Example.com ").verified(true);
                    match CreateNewUser.create_social(&ctx, user).await {
                        Ok(_) => "created".to_owned(),
                        Err(e) => e.to_string(),
                    }
                });
            })
        },
    );
    let answer = h.app.get("/social").text();
    assert!(answer.contains("create_social"), "{answer}");
    let social = SocialUser::new("github", "7", " Ada@Example.com ").name("Ada");
    assert_eq!(social.email, "ada@example.com");
    assert!(!social.email_verified);
    assert_eq!(social.name.as_deref(), Some("Ada"));
}

// ---- set-up options -----------------------------------------------------------------------------------------------

#[test]
fn routes_false_registers_no_route_but_keeps_the_pipeline() {
    let outbox = Outbox::default();
    let mail = outbox.clone();
    let app = TestApp::new(move |b| base(b, &mail).temper(Temper::<User>::new().routes(false)));
    assert_eq!(app.get("/login").status(), 404);
    assert_eq!(app.post_form("/login", &[]).status(), 404);
    assert!(app.app().login_completion().is_some());
}

#[test]
fn without_route_leaves_one_route_to_the_app() {
    let outbox = Outbox::default();
    let mail = outbox.clone();
    let app = TestApp::new(move |b| {
        base(b, &mail)
            // No login view needed: the page is the app's.
            .temper(
                Temper::<User>::new()
                    .without_route("login")
                    .views(TemperViews::new().confirm_password(|_| "confirm")),
            )
            .routes(|r| {
                r.get("/login", || async { "the app's own login page" })
                    .name("login")
                    .middleware("guest");
            })
    });
    assert_eq!(app.get("/login").text(), "the app's own login page");
    create_user(&app, "ada@example.com", "analytical-engine");
    assert_eq!(
        log_in(&app, "ada@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
}

#[test]
fn views_false_registers_no_page_and_answers_json() {
    let outbox = Outbox::default();
    let mail = outbox.clone();
    let app = TestApp::new(move |b| {
        base(b, &mail).temper(
            Temper::<User>::new()
                .registration(CreateNewUser)
                .reset_passwords(ResetUserPassword::default())
                .email_verification()
                .views(false),
        )
    });
    // No page: 405 where the path has a form route, 404 elsewhere.
    for path in [
        "/login",
        "/register",
        "/forgot-password",
        "/reset-password/x",
        "/user/confirm-password",
        "/email/verify",
    ] {
        assert!(matches!(app.get(path).status(), 404 | 405), "{path}");
    }
    for name in [
        "login",
        "register",
        "password.request",
        "password.reset",
        "password.confirm",
        "verification.notice",
    ] {
        assert!(app.app().url(name, &[("token", "t")]).is_err(), "{name}");
    }
    let res = json_request(
        &app,
        Method::POST,
        "/register",
        &serde_json::json!({
            "name": "Ada", "email": "ada@example.com",
            "password": "analytical-engine", "password_confirmation": "analytical-engine"
        }),
    );
    assert_eq!(res.status(), 201);
    assert_eq!(
        json_request(&app, Method::POST, "/logout", &serde_json::json!({})).status(),
        204
    );
    let res = json_request(
        &app,
        Method::POST,
        "/login",
        &serde_json::json!({ "email": "ada@example.com", "password": "analytical-engine" }),
    );
    assert_eq!(res.status(), 200);
    // The reset link falls back to the plain path when the page route is not there.
    json_request(&app, Method::POST, "/logout", &serde_json::json!({}));
    json_request(
        &app,
        Method::POST,
        "/forgot-password",
        &serde_json::json!({ "email": "ada@example.com" }),
    );
    assert!(outbox.last_reset_path().starts_with("/reset-password/"));
}

#[test]
fn a_prefix_moves_every_route_and_keeps_the_names() {
    let h = harness_with(|t| t.prefix("/auth"), |b| b);
    create_user(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(h.app.get("/login").status(), 404);
    assert_eq!(h.app.get("/auth/login").status(), 200);
    assert_eq!(h.app.app().url("login.store", &[]).unwrap(), "/auth/login");
    // `auth` sends guests to the route named `login`.
    assert_eq!(
        h.app.get("/dashboard").header("location"),
        Some("/auth/login")
    );
    let res = log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(res.header("location"), Some("/dashboard"));
    h.app.post_form("/auth/logout", &[]);
    h.app
        .post_form("/auth/forgot-password", &[("email", "ada@example.com")]);
    assert!(
        h.outbox
            .last_reset_path()
            .starts_with("/auth/reset-password/")
    );
}

#[test]
fn home_sends_users_elsewhere() {
    let h = harness_with(|t| t.home("/welcome"), |b| b);
    let res = register(&h.app, "ada@example.com");
    assert_eq!(res.header("location"), Some("/welcome"));
    h.app.post_form("/logout", &[]);
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").header("location"),
        Some("/welcome")
    );
}

#[test]
fn limits_change_the_throttles() {
    let h = harness_with(
        |t| t.limits(smeltery_temper::Limits::new().login("3,1").forms("2,5")),
        |b| b,
    );
    let routes = h.app.app().routes();
    let login = routes
        .iter()
        .find(|r| r.name.as_deref() == Some("login.store"))
        .unwrap();
    assert_eq!(login.middleware, ["guest", "throttle:3,1"]);
    let register = routes
        .iter()
        .find(|r| r.name.as_deref() == Some("register.store"))
        .unwrap();
    assert_eq!(register.middleware, ["guest", "throttle:2,5"]);
    // Fixed windows: 7 requests at 3 a minute see a refusal even when a window ends during the test.
    let statuses: Vec<u16> = (0..7)
        .map(|_| {
            json_request(
                &h.app,
                Method::POST,
                "/login",
                &serde_json::json!({ "email": "x@example.com", "password": "x" }),
            )
            .status()
        })
        .collect();
    assert!(statuses.contains(&429), "{statuses:?}");
    assert!(
        statuses.iter().all(|s| *s == 422 || *s == 429),
        "{statuses:?}"
    );
}

// ---- boot errors --------------------------------------------------------------------------------------------------

async fn build(builder: impl FnOnce(AppBuilder) -> AppBuilder) -> String {
    let mut settings = smeltery_core::config::Settings::from_env();
    settings.env = "testing".into();
    match builder(AppBuilder::new(settings)).build().await {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    }
}

#[tokio::test]
async fn a_missing_view_stops_the_app_and_names_the_method() {
    let error = build(|b| {
        b.temper(
            Temper::<User>::new()
                .registration(CreateNewUser)
                .views(TemperViews::new().login(|_| "l").confirm_password(|_| "c")),
        )
    })
    .await;
    assert!(error.contains("TemperViews::register"), "{error}");
    let error = build(|b| b.temper(Temper::<User>::new())).await;
    assert!(error.contains("TemperViews::login"), "{error}");
    // Without routes or views, nothing is missing.
    assert_eq!(
        build(|b| b.temper(Temper::<User>::new().routes(false))).await,
        ""
    );
    assert_eq!(
        build(|b| b.temper(Temper::<User>::new().views(false))).await,
        ""
    );
}

mod other {
    //! Another model.
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "admins")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub email: String,
        pub password: String,
        pub remember_token: Option<String>,
    }

    impl ActiveModelBehavior for ActiveModel {}

    impl smeltery_core::auth::Authenticatable for Model {
        fn auth_id(&self) -> i64 {
            self.id
        }
        fn password_hash(&self) -> &str {
            &self.password
        }
        fn remember_token(&self) -> Option<&str> {
            self.remember_token.as_deref()
        }
    }
}

#[tokio::test]
async fn set_up_mistakes_stop_the_app() {
    let error = build(|b| {
        b.temper(Temper::<User>::new().views(false))
            .auth::<other::Model>()
    })
    .await;
    assert!(error.contains("two different user models"), "{error}");
    // Before `.temper(…)` too (core sees both calls).
    let error = build(|b| {
        b.auth::<other::Model>()
            .temper(Temper::<User>::new().views(false))
    })
    .await;
    assert!(error.contains("two different user models"), "{error}");
    let error = build(|b| b.temper(Temper::<User>::new().views(false).without_route("logn"))).await;
    assert!(error.contains("without_route(\"logn\")"), "{error}");
    for prefix in ["auth", "/auth/", "/{x}"] {
        let error = build(|b| b.temper(Temper::<User>::new().views(false).prefix(prefix))).await;
        assert!(error.contains("prefix"), "{prefix}: {error}");
    }
    let error = build(|b| {
        b.temper(
            Temper::<User>::new()
                .views(false)
                .limits(smeltery_temper::Limits::new().login("lots")),
        )
    })
    .await;
    assert!(error.contains("throttle:lots"), "{error}");
    // A duplicate route of the app's own is a build error too.
    let error = build(|b| {
        b.temper(Temper::<User>::new().views(false)).routes(|r| {
            r.post("/login", || async { "mine" });
        })
    })
    .await;
    assert!(error.contains("declared twice"), "{error}");
}

// ---- review round 2 -----------------------------------------------------------------------------------------------

/// Core requires verification; the verification routes are the app's own; Temper's feature is off.
fn harness_with_own_verification_routes() -> Harness {
    harness_with(
        |_| {
            Temper::<User>::new()
                .registration(CreateNewUser)
                .update_profile_information(UpdateProfile)
                .views(views())
                .listen(t_listener_placeholder())
        },
        |b| {
            b.verify_email::<User>().routes(|r| {
                r.get("/email/verify", || async { "the app's notice" })
                    .name("verification.notice")
                    .middleware("auth");
                r.get(
                    "/email/verify/{id}/{hash}",
                    |request: smeltery_core::auth::EmailVerificationRequest| async move {
                        request.fulfill().await?;
                        Ok::<_, Error>("verified")
                    },
                )
                .name("verification.verify")
                .middleware("auth");
            })
        },
    )
}

fn t_listener_placeholder() -> impl Fn(
    App,
    TemperEvent,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = smeltery_core::Result<()>> + Send>,
> + Send
+ Sync
+ 'static {
    |_, _| Box::pin(async { Ok(()) })
}

#[test]
fn a_changed_address_is_unverified_even_without_temper_s_verification_feature() {
    let h = harness_with_own_verification_routes();
    // L4: registration mails the link through the app's own verification routes.
    register(&h.app, "ada@example.com");
    assert_eq!(h.outbox.verifications().len(), 1);
    assert_eq!(
        h.app.get(&h.outbox.last_verification_path()).text(),
        "verified"
    );
    assert_eq!(h.app.get("/dashboard").status(), 200);
    // M1: a new address is not verified, and the new address gets a link.
    confirm_password(&h.app, "analytical-engine");
    let res = form_request(
        &h.app,
        Method::PUT,
        "/user/profile-information",
        &[("name", "Ada"), ("email", "someone-else@example.com")],
    );
    assert_eq!(res.status(), 303);
    assert!(user(&h.app, 1).email_verified_at.is_none());
    assert_eq!(
        h.app.get("/dashboard").header("location"),
        Some("/email/verify")
    );
    let sent = h.outbox.verifications();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].0, "someone-else@example.com");
}

#[test]
fn a_listener_that_panics_before_its_future_changes_nothing() {
    let recorder = EventRecorder::new();
    let rec = recorder.clone();
    let h = harness_with(
        move |t| {
            t.listen(|_, event: TemperEvent| {
                assert!(event.user_id().is_some(), "a synchronous panic");
                async { Ok(()) }
            })
            .listen(rec.listener())
        },
        |b| b,
    );
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let res = log_in(&h.app, "ada@example.com", "wrong-password");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
    assert_eq!(
        recorder.count(|e| matches!(e, TemperEvent::Failed { .. })),
        1
    );
}

#[test]
fn unknown_addresses_and_wrong_passwords_get_identical_answers() {
    let h = harness();
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let browser = |email: &str| {
        h.app.clear_cookies();
        let res = log_in(&h.app, email, "wrong-password");
        let page = h.app.get("/login").text().replace(email, "<typed>");
        (
            res.status(),
            res.header("location").map(str::to_owned),
            page,
        )
    };
    assert_eq!(browser("ada@example.com"), browser("nobody@example.com"));
    let json = |email: &str| {
        let res = json_request(
            &h.app,
            Method::POST,
            "/login",
            &serde_json::json!({ "email": email, "password": "wrong-password" }),
        );
        (res.status(), res.text())
    };
    assert_eq!(json("ada@example.com"), json("nobody@example.com"));
    assert_eq!(
        h.events.count(|e| matches!(e, TemperEvent::Failed { .. })),
        4
    );
}

struct BrokenFailedLogin;

impl TemperResponses for BrokenFailedLogin {
    fn failed_login(
        &self,
        _ctx: &TemperCtx,
        _email: &str,
    ) -> smeltery_core::Result<smeltery_core::Response> {
        Err(Error::internal("a failing override"))
    }
}

#[test]
fn the_failed_event_fires_even_when_the_answer_fails() {
    let h = harness_with(|t| t.responses(BrokenFailedLogin), |b| b);
    let res = log_in(&h.app, "nobody@example.com", "wrong-password");
    assert_eq!(res.status(), 500);
    assert_eq!(
        h.events.count(|e| matches!(e, TemperEvent::Failed { .. })),
        1
    );
}

#[test]
fn the_failed_login_hash_is_keyed_by_the_app() {
    let h = harness();
    log_in(&h.app, "Nobody@example.com", "wrong-password");
    let TemperEvent::Failed { email_hash, .. } = h.events.events().pop().unwrap() else {
        panic!()
    };
    assert_ne!(
        email_hash,
        smeltery_core::crypto::sha256_hex("nobody@example.com")
    );
    assert_eq!(email_hash.len(), 43, "an HMAC-SHA256, base64url");
    log_in(&h.app, "nobody@example.com", "x");
    let TemperEvent::Failed {
        email_hash: again, ..
    } = h.events.events().pop().unwrap()
    else {
        panic!()
    };
    assert_eq!(email_hash, again, "stable per app for correlation");
}

#[test]
fn an_address_change_without_verification_just_updates_the_profile() {
    let h = harness();
    register(&h.app, "ada@example.com");
    confirm_password(&h.app, "analytical-engine");
    let res = form_request(
        &h.app,
        Method::PUT,
        "/user/profile-information",
        &[("name", "Ada"), ("email", "ada@lovelace.example")],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(user(&h.app, 1).email, "ada@lovelace.example");
    assert!(h.outbox.verifications().is_empty());
    assert_eq!(h.app.get("/dashboard").status(), 200);
    assert_eq!(
        h.events.count(|e| matches!(
            e,
            TemperEvent::ProfileUpdated {
                email_changed: true,
                ..
            }
        )),
        1
    );
}

// ---- login policy (core builder 2) --------------------------------------------------------------------------------

async fn suspended_first(
    _app: App,
    user: User,
) -> smeltery_core::Result<smeltery_core::auth::LoginDecision> {
    Ok(if user.email.starts_with("suspended") {
        smeltery_core::auth::LoginDecision::refuse(
            smeltery_core::http::StatusCode::FORBIDDEN,
            "This account is suspended.",
        )
    } else {
        smeltery_core::auth::LoginDecision::Allow
    })
}

#[test]
fn a_login_policy_refuses_every_way_in_before_the_steps() {
    let steps = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&steps);
    let h = harness_with(
        move |t| {
            t.login_policy(suspended_first)
                .login_pipeline(move |_ctx, _user: User| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    async { Ok(PipelineStep::Continue) }
                })
        },
        |b| {
            b.routes(|r| {
                r.get("/auth/callback", callback);
            })
        },
    );
    let suspended = create_user(&h.app, "suspended@example.com", "analytical-engine");
    assert_eq!(suspended, 1, "the callback signs in user 1");
    create_user(&h.app, "ada@example.com", "analytical-engine");
    // JSON: the policy's status and message, nobody signed in, no step run.
    let res = json_request(
        &h.app,
        Method::POST,
        "/login",
        &serde_json::json!({ "email": "suspended@example.com", "password": "analytical-engine" }),
    );
    assert_eq!(res.status(), 403, "{}", res.text());
    assert_eq!(
        res.json()["errors"]["email"][0],
        "This account is suspended."
    );
    assert_eq!(h.app.get("/dashboard").status(), 303);
    // A browser is sent back.
    let res = log_in(&h.app, "suspended@example.com", "analytical-engine");
    assert_eq!(res.status(), 303);
    assert_ne!(res.header("location"), Some("/dashboard"));
    assert_eq!(h.app.get("/dashboard").status(), 303);
    // A social login through core's LoginCompletion meets it too.
    assert_eq!(h.app.get("/auth/callback").status(), 303);
    assert_eq!(h.app.get("/dashboard").status(), 303);
    // The same rule answers a session-free caller (a token endpoint).
    let app = h.app.app().clone();
    let refused = h.app.block_on(async move {
        let user = app.find_user(suspended).await.unwrap().unwrap();
        app.check_login(&user).await
    });
    assert_eq!(refused.unwrap_err().status(), 403);
    assert_eq!(steps.load(Ordering::SeqCst), 0);
    assert_eq!(login_events(&h.events), 0);
    // Others sign in through the steps.
    assert_eq!(
        log_in(&h.app, "ada@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
    assert_eq!(steps.load(Ordering::SeqCst), 1);
}

#[test]
fn a_core_login_policy_gates_temper_logins_too() {
    struct Nobody;
    impl smeltery_core::auth::LoginPolicy for Nobody {
        fn check<'a>(
            &'a self,
            _app: &'a App,
            _user: &'a AuthUser,
        ) -> smeltery_core::BoxFuture<'a, smeltery_core::Result<smeltery_core::auth::LoginDecision>>
        {
            Box::pin(async {
                Ok(smeltery_core::auth::LoginDecision::refuse(
                    smeltery_core::http::StatusCode::FORBIDDEN,
                    "Sign-ins are closed.",
                ))
            })
        }
    }
    let h = harness_with(|t| t, |b: AppBuilder| b.login_policy(Nobody));
    create_user(&h.app, "ada@example.com", "analytical-engine");
    let res = json_request(
        &h.app,
        Method::POST,
        "/login",
        &serde_json::json!({ "email": "ada@example.com", "password": "analytical-engine" }),
    );
    assert_eq!(res.status(), 403, "{}", res.text());
    assert_eq!(h.app.get("/dashboard").status(), 303);
}

#[test]
fn an_approval_policy_refuses_the_first_sign_in_after_registration() {
    let h = harness_with(
        |t| {
            t.login_policy(|_app, user: User| async move {
                Ok(if user.email.ends_with("@pending.example") {
                    smeltery_core::auth::LoginDecision::refuse(
                        smeltery_core::http::StatusCode::FORBIDDEN,
                        "Your account awaits approval.",
                    )
                } else {
                    smeltery_core::auth::LoginDecision::Allow
                })
            })
        },
        |b| b,
    );
    let res = h.app.post_json(
        "/register",
        &serde_json::json!({
            "name": "Ada Lovelace",
            "email": "ada@pending.example",
            "password": "analytical-engine",
            "password_confirmation": "analytical-engine",
        }),
    );
    assert_eq!(res.status(), 403, "{}", res.text());
    assert_eq!(
        res.json()["errors"]["email"][0],
        "Your account awaits approval."
    );
    // The account exists, nobody is signed in, `Registered` fired, no `Login`.
    let app = h.app.app().clone();
    let found = h.app.block_on(async move {
        smeltery_core::auth::find_by_email::<User>(&app, "ada@pending.example")
            .await
            .unwrap()
    });
    assert!(found.is_some());
    assert_eq!(h.app.get("/dashboard").status(), 303);
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::Registered { .. })),
        1
    );
    assert_eq!(login_events(&h.events), 0);
    // An allowed address registers and is signed in as before.
    let res = register(&h.app, "bob@example.com");
    assert_eq!(res.status(), 303);
    assert_eq!(
        h.events
            .count(|e| matches!(e, TemperEvent::Registered { .. })),
        2
    );
}

#[test]
fn end_credentials_signs_a_temper_session_out_on_its_next_request() {
    let h = harness();
    let ada = create_user(&h.app, "ada@example.com", "analytical-engine");
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(h.app.get("/dashboard").status(), 200);
    h.app
        .block_on(smeltery_core::auth::end_credentials(h.app.app(), ada))
        .unwrap();
    assert_eq!(h.app.get("/dashboard").status(), 303);
    // Signing in again works (no policy refuses here).
    log_in(&h.app, "ada@example.com", "analytical-engine");
    assert_eq!(h.app.get("/dashboard").status(), 200);
}

/// W3-03 (sweep 3): a reset link mailed to the old address stops working when the address changes.
#[test]
fn an_address_change_ends_the_pending_reset_link() {
    let h = harness_verifying();
    create_user(&h.app, "old@example.com", "analytical-engine");
    h.app
        .post_form("/forgot-password", &[("email", "old@example.com")]);
    let link = h.outbox.last_reset_path();
    log_in(&h.app, "old@example.com", "analytical-engine");
    confirm_password(&h.app, "analytical-engine");
    let res = form_request(
        &h.app,
        Method::PUT,
        "/user/profile-information",
        &[("name", "Ada"), ("email", "new@example.com")],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(h.app.post_form("/logout", &[]).status(), 303);
    // Whoever holds the old mailbox types the new address: the token is gone.
    h.app.post_form(
        link.split('?').next().unwrap(),
        &[
            ("email", "new@example.com"),
            ("password", "someone-elses-password"),
            ("password_confirmation", "someone-elses-password"),
        ],
    );
    assert_eq!(
        log_in(&h.app, "new@example.com", "someone-elses-password").header("location"),
        Some("/login"),
        "the old link must not reset the account"
    );
    assert_eq!(
        log_in(&h.app, "new@example.com", "analytical-engine").header("location"),
        Some("/dashboard")
    );
}

/// W3-04 (sweep 3): address changes mail at most three verification links an hour per user (the address is typed
/// by the user, so it may be anyone's).
#[test]
fn address_changes_mail_at_most_three_links_an_hour() {
    let h = harness_verifying();
    register(&h.app, "ada@example.com");
    assert_eq!(h.outbox.verifications().len(), 1);
    confirm_password(&h.app, "analytical-engine");
    for (i, email) in [
        "victim@example.com",
        "ada@example.com",
        "victim@example.com",
        "ada@example.com",
    ]
    .into_iter()
    .enumerate()
    {
        let res = form_request(
            &h.app,
            Method::PUT,
            "/user/profile-information",
            &[("name", "Ada"), ("email", email)],
        );
        assert_eq!(res.status(), 303, "change {i}");
        assert_eq!(user(&h.app, 1).email, email, "the change is saved");
    }
    assert_eq!(
        h.outbox.verifications().len(),
        4,
        "registration + three changes; the fourth change mails nothing"
    );
}
