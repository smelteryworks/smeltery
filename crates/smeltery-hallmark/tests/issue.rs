//! Issuing tokens for an email and password (`issue_for_credentials`), the second factor, the login budgets, and
//! `revoke_current`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::json;
use smeltery_core::auth::{
    AuthUser, LoginDecision, LoginPolicy, SecondFactor, SecondFactorVerdict,
};
use smeltery_core::http::{HeaderMap, Method};
use smeltery_core::testing::{TestApp, TestResponse};
use smeltery_core::{App, BoxFuture, Result};
use smeltery_hallmark::{CODE_INVALID, CODE_REQUIRED, FAILED, Hallmark};
use support::*;

fn issue(app: &TestApp, email: &str, password: &str, code: Option<&str>) -> TestResponse {
    let mut body = json!({"email": email, "password": password, "device_name": "Ada's phone"});
    if let Some(code) = code {
        body["code"] = json!(code);
    }
    app.with_header("accept", "application/json");
    let res = app.post_json("/api/tokens", &body);
    app.without_header("accept");
    res
}

#[test]
fn a_token_is_issued_for_matching_credentials() {
    let app = app();
    app.from_addr("192.0.2.1:4000".parse().unwrap());
    create_user(&app, "ada@example.com", "secret one");
    let res = issue(&app, "ada@example.com", "secret one", None);
    assert_eq!(res.status(), 201, "{}", res.text());
    assert_eq!(res.header("cache-control"), Some("no-store"));
    let body = res.json();
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["abilities"], json!(["*"]));
    assert!(body["expires_at"].is_string());
    let token = body["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("smt_") && token.len() == 68);
    let auth = format!("Bearer {token}");
    assert_eq!(get_with(&app, "/api/me", &auth).status(), 200);
    let listed = app.block_on(tokens(&app).list(1)).unwrap();
    assert_eq!(listed[0].name, "Ada's phone");
    // `revoke_current` signs this device out.
    app.with_bearer(&token);
    let res = app.request(
        Method::DELETE,
        "/api/tokens/mine",
        HeaderMap::new(),
        Default::default(),
    );
    assert_eq!(res.status(), 204);
    assert_eq!(app.get_json("/api/me").status(), 401);
}

#[test]
fn wrong_credentials_get_one_answer_and_no_token() {
    let app = app();
    app.from_addr("192.0.2.2:4000".parse().unwrap());
    create_user(&app, "ada@example.com", "secret one");
    for (email, password) in [
        ("ada@example.com", "wrong"),
        ("nobody@example.com", "secret one"),
    ] {
        let res = issue(&app, email, password, None);
        assert_eq!(res.status(), 422, "{email}");
        assert_eq!(res.json()["errors"]["email"][0], FAILED);
        assert!(res.json()["token"].is_null());
    }
    let mut body = json!({"email": "ada@example.com", "password": "secret one", "device_name": ""});
    app.with_header("accept", "application/json");
    let res = app.post_json("/api/tokens", &body);
    assert_eq!(res.status(), 422);
    assert!(res.json()["errors"]["device_name"].is_array());
    body["device_name"] = json!("a\nb");
    assert_eq!(app.post_json("/api/tokens", &body).status(), 422);
    assert_eq!(token_rows(&app), 0);
}

/// The issuing endpoint counts against the same budgets as the web login: five a minute for one address from one
/// client, whatever the endpoint.
#[test]
fn the_token_endpoint_shares_the_login_budgets() {
    let app = app();
    app.from_addr("192.0.2.3:4000".parse().unwrap());
    create_user(&app, "ada@example.com", "secret one");
    for i in 0..3 {
        assert_eq!(
            app.post_form(
                "/login",
                &[("email", "ada@example.com"), ("password", "wrong")]
            )
            .text(),
            "out",
            "{i}"
        );
    }
    for i in 0..2 {
        assert_eq!(
            issue(&app, "ada@example.com", "wrong", None).status(),
            422,
            "{i}"
        );
    }
    // The sixth attempt, right password or not, is refused before the password is checked.
    let res = issue(&app, "ada@example.com", "secret one", None);
    assert_eq!(res.status(), 429, "{}", res.text());
    assert_eq!(token_rows(&app), 0);
}

/// A second factor that needs `123456`, counting its checks; `spent` answers every check with a spent budget.
struct Fake {
    required: bool,
    spent: bool,
    checks: Arc<AtomicU32>,
}

impl SecondFactor for Fake {
    fn required<'a>(&'a self, _app: &'a App, _user: &'a AuthUser) -> BoxFuture<'a, Result<bool>> {
        let required = self.required;
        Box::pin(async move { Ok(required) })
    }

    fn verify<'a>(
        &'a self,
        _app: &'a App,
        _user: &'a AuthUser,
        code: &'a str,
    ) -> BoxFuture<'a, Result<SecondFactorVerdict>> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        let spent = self.spent;
        Box::pin(async move {
            Ok(if spent {
                SecondFactorVerdict::TooManyAttempts { retry_after: 42 }
            } else if code == "123456" {
                SecondFactorVerdict::Valid
            } else {
                SecondFactorVerdict::Invalid
            })
        })
    }
}

fn app_with_second_factor(required: bool) -> (TestApp, Arc<AtomicU32>) {
    let checks = Arc::new(AtomicU32::new(0));
    let fake = Fake {
        required,
        spent: false,
        checks: Arc::clone(&checks),
    };
    let app = TestApp::new(move |b| build(Hallmark::new())(b).second_factor(fake));
    app.from_addr("192.0.2.4:4000".parse().unwrap());
    (app, checks)
}

#[test]
fn a_required_second_factor_blocks_issuance() {
    let (app, checks) = app_with_second_factor(true);
    create_user(&app, "ada@example.com", "secret one");
    let res = issue(&app, "ada@example.com", "secret one", None);
    assert_eq!(res.status(), 422);
    assert_eq!(res.json()["errors"]["code"][0], CODE_REQUIRED);
    let res = issue(&app, "ada@example.com", "secret one", Some("000000"));
    assert_eq!(res.status(), 422);
    assert_eq!(res.json()["errors"]["code"][0], CODE_INVALID);
    assert_eq!(token_rows(&app), 0, "no token before the second factor");
    // A wrong password never reaches the second factor.
    let before = checks.load(Ordering::SeqCst);
    assert_eq!(
        issue(&app, "ada@example.com", "wrong", Some("123456")).status(),
        422
    );
    assert_eq!(checks.load(Ordering::SeqCst), before);
    assert_eq!(
        issue(&app, "ada@example.com", "secret one", Some("123456")).status(),
        201
    );
    assert_eq!(token_rows(&app), 1);
}

#[test]
fn a_second_factor_that_is_not_required_asks_nothing() {
    let (app, checks) = app_with_second_factor(false);
    create_user(&app, "ada@example.com", "secret one");
    assert_eq!(
        issue(&app, "ada@example.com", "secret one", None).status(),
        201
    );
    assert_eq!(checks.load(Ordering::SeqCst), 0);
}

/// Refuses the user with this address (a suspended account).
struct Suspended(&'static str);

impl LoginPolicy for Suspended {
    fn check<'a>(
        &'a self,
        _app: &'a App,
        user: &'a AuthUser,
    ) -> BoxFuture<'a, Result<LoginDecision>> {
        let email = user.downcast::<User>().map(|u| u.email);
        let refused = email.as_deref() == Some(self.0);
        Box::pin(async move {
            Ok(if refused {
                LoginDecision::refuse(
                    smeltery_core::http::StatusCode::FORBIDDEN,
                    "This account is suspended.",
                )
            } else {
                LoginDecision::Allow
            })
        })
    }
}

/// Review M-A: the app's login policy decides at the token endpoint too, before the second factor and any token.
#[test]
fn a_login_policy_refusing_a_suspended_user_blocks_issuance() {
    let checks = Arc::new(AtomicU32::new(0));
    let fake = Fake {
        required: true,
        spent: false,
        checks: Arc::clone(&checks),
    };
    let app = TestApp::new(move |b| {
        build(Hallmark::new())(b)
            .second_factor(fake)
            .login_policy(Suspended("bob@example.com"))
    });
    app.from_addr("192.0.2.5:4000".parse().unwrap());
    create_user(&app, "ada@example.com", "secret one");
    create_user(&app, "bob@example.com", "secret two");
    let res = issue(&app, "bob@example.com", "secret two", Some("123456"));
    assert_eq!(res.status(), 403, "{}", res.text());
    assert_eq!(
        res.json()["errors"]["email"][0],
        "This account is suspended."
    );
    assert_eq!(
        checks.load(Ordering::SeqCst),
        0,
        "refused before the second factor"
    );
    assert_eq!(token_rows(&app), 0);
    assert_eq!(
        issue(&app, "ada@example.com", "secret one", Some("123456")).status(),
        201
    );
}

/// A spent second-factor budget answers 429 with `Retry-After`, not "invalid".
#[test]
fn a_spent_second_factor_budget_answers_429() {
    let fake = Fake {
        required: true,
        spent: true,
        checks: Arc::new(AtomicU32::new(0)),
    };
    let app = TestApp::new(move |b| build(Hallmark::new())(b).second_factor(fake));
    app.from_addr("192.0.2.6:4000".parse().unwrap());
    create_user(&app, "ada@example.com", "secret one");
    let res = issue(&app, "ada@example.com", "secret one", Some("123456"));
    assert_eq!(res.status(), 429, "{}", res.text());
    assert_eq!(res.header("retry-after"), Some("42"));
    assert!(res.json()["errors"]["code"].is_array(), "{}", res.text());
    assert_eq!(token_rows(&app), 0);
}

/// A second factor that pauses inside `verify` until released: the moment between the login policy and the insert.
struct Gate {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl SecondFactor for Gate {
    fn required<'a>(&'a self, _app: &'a App, _user: &'a AuthUser) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(true) })
    }

    fn verify<'a>(
        &'a self,
        _app: &'a App,
        _user: &'a AuthUser,
        _code: &'a str,
    ) -> BoxFuture<'a, Result<SecondFactorVerdict>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(SecondFactorVerdict::Valid)
        })
    }
}

/// Sweep W4-01: a token issued while `end_credentials` runs (after the login policy allowed the user, before the
/// insert) never works: it is bound to the user row the policy judged, whose credentials epoch is the old one. Red
/// while the token was bound to a fresh read of the password hash alone.
#[test]
fn a_token_issued_while_credentials_end_never_works() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let gate = Gate {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let app = TestApp::new(move |b| build(Hallmark::new())(b).second_factor(gate));
    let ada = create_user(&app, "ada@example.com", "secret one");
    let parts = http::Request::new(()).into_parts().0;
    let client = smeltery_core::http::ClientInfo::from_parts(&parts);
    let a = app.app().clone();
    let issued = app.block_on(async {
        let issue = smeltery_hallmark::issue_for_credentials(
            &a,
            &client,
            "ada@example.com",
            "secret one",
            "phone",
            Some("123456"),
            &["*"],
        );
        let suspend = async {
            entered.notified().await;
            smeltery_core::auth::end_credentials(&a, ada.id)
                .await
                .unwrap();
            release.notify_one();
        };
        let (issued, ()) = tokio::join!(issue, suspend);
        issued
    });
    let token = issued.unwrap().plain_text().to_owned();
    assert_eq!(
        get_with(&app, "/api/me", &format!("Bearer {token}")).status(),
        401
    );
    assert_eq!(token_rows(&app), 0);
}
