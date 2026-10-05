//! HTTP tests: requests against the app in memory, no server and no Node.js needed. Every `TestApp` starts with a
//! fresh, migrated database and keeps its cookies between requests, so sessions and logins carry over.
//! `get_alloy` visits a page the way Inertia's client does and gets the page object as JSON.

use smeltery::alloy::testing::{AlloyAssertions as _, AlloyRequests as _, assert_page_file_exists};
use smeltery::db::Record as _;
use smeltery::db::seed::Seeder;
use smeltery::mail::{Mailer, ResetPassword, VerifyEmail};
use smeltery::temper::testing::{confirm_password, enable_two_factor, fake_clock, two_factor_code};
use smeltery::testing::TestApp;

use my_app::app::models::User;
use my_app::database::seeders::database_seeder::DatabaseSeeder;

/// A fresh app with the demo user (`demo@example.com` / `password`).
fn app_with_demo_user() -> TestApp {
    let app = TestApp::new(my_app::build);
    let db = app.db();
    app.block_on(DatabaseSeeder.run(&db))
        .expect("seeding the demo user");
    app
}

/// Logs the demo user in.
fn log_in(app: &TestApp) {
    let res = app.post_form(
        "/login",
        &[("email", "demo@example.com"), ("password", "password")],
    );
    assert_eq!(res.header("location"), Some("/dashboard"));
}

#[test]
fn home_page_works() {
    let app = TestApp::new(my_app::build);
    let res = app.get("/");
    assert_eq!(res.status(), 200);
    // The first visit is HTML from resources/views/app.mold.html with the page object inside.
    assert!(res.text().contains("<title>My App</title>"));
    res.assert_component("Welcome")
        .assert_prop("app.name", "My App")
        .assert_prop("version", smeltery::VERSION);
}

#[test]
fn every_page_has_its_file() {
    // Component names are strings the browser resolves to resources/js/pages/<name>.vue.
    for page in [
        "Welcome",
        "Dashboard",
        "auth/Login",
        "auth/Register",
        "auth/ForgotPassword",
        "auth/ResetPassword",
        "auth/VerifyEmail",
        "auth/ConfirmPassword",
        "auth/TwoFactorChallenge",
        "settings/Profile",
        "settings/Password",
        "settings/TwoFactor",
    ] {
        assert_page_file_exists(page);
    }
}

#[test]
fn the_forge_answers_a_partial_reload() {
    let app = TestApp::new(my_app::build);
    // `forge` is optional: a visit never computes it.
    app.get_alloy("/").assert_missing("forge");
    let res = app.reload_alloy("/", "Welcome", &["forge"]);
    assert_eq!(res.status(), 200);
    let temperature = res
        .prop("forge.temperature")
        .and_then(|t| t.as_u64())
        .expect("a reading");
    assert!((1_150..1_500).contains(&temperature), "{temperature}");
    // Only the prop that was asked for comes back.
    res.assert_missing("version");
}

#[test]
fn health_check_works() {
    let app = TestApp::new(my_app::build);
    let res = app.get("/api/health");
    assert_eq!(res.status(), 200);
    assert!(res.text().contains("ok"));
}

#[test]
fn guests_are_sent_from_the_dashboard_to_the_login_page() {
    let app = TestApp::new(my_app::build);
    let res = app.get_alloy("/dashboard");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
    app.get_alloy("/login")
        .assert_component("auth/Login")
        .assert_prop("auth.user", smeltery::json!(null));
}

#[test]
fn new_users_can_register_and_see_the_dashboard() {
    let app = TestApp::new(my_app::build);
    let res = app.post_alloy(
        "/register",
        &smeltery::json!({
            "name": "Ada Lovelace",
            "email": "ada@example.com",
            "password": "analytical-engine",
            "password_confirmation": "analytical-engine",
        }),
    );
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    let dashboard = app.get_alloy("/dashboard");
    dashboard
        .assert_component("Dashboard")
        .assert_prop("auth.user.name", "Ada Lovelace")
        // Deferred: not on the visit itself, the client asks for it right after.
        .assert_missing("activity")
        .assert_deferred("default", &["activity"]);
    // The shared user is an allow-list: no password hash, no remember token.
    let user = dashboard.prop("auth.user").expect("the signed-in user");
    let mut keys: Vec<&str> = user
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "email",
            "email_verified",
            "id",
            "name",
            "two_factor_enabled"
        ]
    );
    app.reload_alloy("/dashboard", "Dashboard", &["activity"])
        .assert_prop("activity.accounts", 1);
}

#[test]
fn a_bad_registration_shows_the_errors() {
    let app = TestApp::new(my_app::build);
    let res = app.post_alloy(
        "/register",
        &smeltery::json!({
            "name": "Ada",
            "email": "not-an-email",
            "password": "short",
            "password_confirmation": "short",
        }),
    );
    // A redirect back with the errors in the session, not 422 JSON.
    assert_eq!(res.status(), 303);
    let page = app.get_alloy("/register");
    let page = page.alloy_page();
    assert!(page["props"]["errors"]["email"].is_string(), "{page}");
    assert!(page["props"]["errors"]["password"].is_string(), "{page}");
    assert_eq!(app.get("/dashboard").status(), 303);
}

#[test]
fn the_demo_user_can_log_in() {
    let app = app_with_demo_user();
    log_in(&app);
    app.get_alloy("/dashboard")
        .assert_prop("auth.user.name", "Demo User");
}

#[test]
fn a_wrong_password_shows_an_error() {
    let app = app_with_demo_user();
    let res = app.post_alloy(
        "/login",
        &smeltery::json!({ "email": "demo@example.com", "password": "wrong-password", "remember": false }),
    );
    assert_eq!(res.status(), 303);
    app.get_alloy("/login").assert_prop(
        "errors.email",
        "These credentials do not match our records.",
    );
    assert_eq!(app.get("/dashboard").status(), 303);
}

#[test]
fn forgot_password_mails_a_reset_link() {
    let app = app_with_demo_user();
    let res = app.post_alloy(
        "/forgot-password",
        &smeltery::json!({ "email": "demo@example.com" }),
    );
    assert_eq!(res.status(), 303);
    // The status message arrives as the page's flash.
    let page = app.get_alloy("/forgot-password").alloy_page();
    assert!(page["flash"]["status"].is_string(), "{page}");
    // Tests run with APP_ENV=testing: mail goes to a fake mailbox instead of the log or SMTP.
    let mailbox = Mailer::of(app.app())
        .expect("mail is installed")
        .mailbox()
        .expect("the fake mailbox");
    mailbox.assert_sent::<ResetPassword>(|mail, email| {
        mail.email == "demo@example.com" && email.has_recipient("demo@example.com")
    });
}

#[test]
fn a_mailed_reset_link_sets_a_new_password() {
    let app = app_with_demo_user();
    app.post_form("/forgot-password", &[("email", "demo@example.com")]);
    let mailbox = Mailer::of(app.app())
        .expect("mail is installed")
        .mailbox()
        .expect("the fake mailbox");
    let (mail, _) = mailbox
        .sent_of::<ResetPassword>()
        .pop()
        .expect("the reset mail");
    let link = mail
        .url
        .find("/reset-password/")
        .and_then(|at| mail.url.get(at..))
        .expect("the link")
        .to_owned();
    assert_eq!(app.get(&link).status(), 200);
    let action = link.split('?').next().expect("the path");
    let res = app.post_alloy(
        action,
        &smeltery::json!({
            "email": "demo@example.com",
            "password": "a-new-password",
            "password_confirmation": "a-new-password",
        }),
    );
    assert_eq!(res.header("location"), Some("/login"));
    let res = app.post_alloy(
        "/login",
        &smeltery::json!({ "email": "demo@example.com", "password": "a-new-password", "remember": false }),
    );
    assert_eq!(res.header("location"), Some("/dashboard"));
}

#[test]
fn password_reset_requests_are_throttled() {
    let app = TestApp::new(my_app::build);
    // `throttle:6,1` allows 6 a minute per client and route; 13 requests fill one minute even across its end.
    let refused = (0..13).any(|_| {
        let res = app.post_alloy(
            "/forgot-password",
            &smeltery::json!({ "email": "nobody@example.com" }),
        );
        assert_eq!(res.status(), 303);
        let page = app.get_alloy("/forgot-password").alloy_page();
        page["props"]["errors"]["email"]
            .as_str()
            .is_some_and(|e| e.starts_with("Too many attempts"))
    });
    assert!(refused, "the forgot-password form is throttled");
}

#[test]
fn email_addresses_are_trimmed_and_lower_cased() {
    let app = TestApp::new(my_app::build);
    let register = |email: &str| {
        app.post_alloy(
            "/register",
            &smeltery::json!({
                "name": "Ada Lovelace",
                "email": email,
                "password": "analytical-engine",
                "password_confirmation": "analytical-engine",
            }),
        )
    };
    assert_eq!(
        register(" Ada@Example.COM ").header("location"),
        Some("/dashboard")
    );
    assert_eq!(
        app.post_alloy("/logout", &smeltery::json!({})).status(),
        303
    );
    // The same address in other letters is the same account: it logs in, and it cannot register again.
    let res = app.post_alloy(
        "/login",
        &smeltery::json!({ "email": "ADA@example.com", "password": "analytical-engine", "remember": false }),
    );
    assert_eq!(res.header("location"), Some("/dashboard"));
    assert_eq!(
        app.post_alloy("/logout", &smeltery::json!({})).status(),
        303
    );
    register("ada@EXAMPLE.com");
    app.get_alloy("/register")
        .assert_prop("errors.email", "The email has already been taken.");
}

#[test]
fn logging_out_clears_the_history() {
    let app = app_with_demo_user();
    log_in(&app);
    assert_eq!(app.get_alloy("/dashboard").status(), 200);
    let res = app.post_alloy("/logout", &smeltery::json!({}));
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/"));
    // The next page tells the browser to forget the history state, so the back button cannot show the dashboard's
    // props; only once.
    let page = app.get_alloy("/").alloy_page();
    assert_eq!(page["clearHistory"], true, "{page}");
    let page = app.get_alloy("/").alloy_page();
    assert!(page.get("clearHistory").is_none(), "{page}");
    assert_eq!(app.get("/dashboard").status(), 303);
}

/// Registers Ada and returns the app's fake mailbox.
fn register_ada(app: &TestApp) -> smeltery::mail::Mailbox {
    let res = app.post_form(
        "/register",
        &[
            ("name", "Ada Lovelace"),
            ("email", "ada@example.com"),
            ("password", "analytical-engine"),
            ("password_confirmation", "analytical-engine"),
        ],
    );
    assert_eq!(res.status(), 303);
    Mailer::of(app.app())
        .expect("mail is installed")
        .mailbox()
        .expect("the fake mailbox")
}

/// `build` with the line that `bootstrap/app.rs` has commented out: email verification on.
fn require_verification(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    my_app::build(app).verify_email::<User>()
}

#[test]
fn email_verification_follows_bootstrap() {
    let app = TestApp::new(my_app::build);
    let mailbox = register_ada(&app);
    let dashboard = app.get_alloy("/dashboard");
    if mailbox.sent_of::<VerifyEmail>().is_empty() {
        // Off (`.verify_email` is commented out in `bootstrap/app.rs`): `verified` lets everyone through.
        assert_eq!(dashboard.status(), 200);
        let res = app.get("/email/verify");
        assert_eq!(res.status(), 303);
        assert_eq!(res.header("location"), Some("/dashboard"));
    } else {
        // On: the dashboard waits for the link in the mail.
        assert_eq!(dashboard.status(), 303);
        assert_eq!(dashboard.header("location"), Some("/email/verify"));
    }
}

#[test]
fn with_verification_on_the_mailed_link_opens_the_dashboard() {
    let app = TestApp::new(require_verification);
    let mailbox = register_ada(&app);
    let res = app.get("/dashboard");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/email/verify"));
    app.get_alloy("/email/verify")
        .assert_component("auth/VerifyEmail")
        .assert_prop("auth.user.email_verified", false);
    let (mail, email) = mailbox
        .sent_of::<VerifyEmail>()
        .pop()
        .expect("the verification mail");
    assert!(email.has_recipient("ada@example.com"));
    let path = mail
        .url
        .find("/email/verify/")
        .and_then(|at| mail.url.get(at..))
        .expect("the link");
    let res = app.get(path);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    app.get_alloy("/dashboard")
        .assert_prop("auth.user.email_verified", true);
}

#[test]
fn with_verification_on_the_link_can_be_sent_again() {
    let app = TestApp::new(require_verification);
    let mailbox = register_ada(&app);
    let res = app.post_alloy("/email/verification-notification", &smeltery::json!({}));
    assert_eq!(res.status(), 303);
    let page = app.get_alloy("/email/verify").alloy_page();
    assert_eq!(
        page["flash"]["status"],
        "A new verification link has been sent to your e-mail address."
    );
    assert_eq!(mailbox.sent_of::<VerifyEmail>().len(), 2);
}
#[test]
fn a_guest_lands_on_the_page_they_asked_for_after_logging_in() {
    let app = app_with_demo_user();
    let res = app.get("/dashboard");
    assert_eq!(res.header("location"), Some("/login"));
    let res = app.post_form(
        "/login",
        &[("email", "demo@example.com"), ("password", "password")],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
}

#[test]
fn a_verification_link_opened_while_logged_out_works_after_logging_in() {
    let app = TestApp::new(require_verification);
    let mailbox = register_ada(&app);
    let (mail, _) = mailbox
        .sent_of::<VerifyEmail>()
        .pop()
        .expect("the verification mail");
    let link = mail
        .url
        .find("/email/verify/")
        .and_then(|at| mail.url.get(at..))
        .expect("the link")
        .to_owned();
    assert_eq!(app.post_form("/logout", &[]).status(), 303);
    // Logged out, the link leads to the login page; after logging in, back to the link.
    assert_eq!(app.get(&link).header("location"), Some("/login"));
    let res = app.post_form(
        "/login",
        &[
            ("email", "ada@example.com"),
            ("password", "analytical-engine"),
        ],
    );
    assert_eq!(res.header("location"), Some(link.as_str()));
    assert_eq!(app.get(&link).header("location"), Some("/dashboard"));
    assert_eq!(app.get("/dashboard").status(), 200);
}

/// A fixed time for two-factor codes (`fake_clock`): the next 30-second step starts 20 seconds later.
const T: i64 = 1_700_000_010;

/// The demo user's id.
fn demo_user_id(app: &TestApp) -> i64 {
    let db = app.db();
    app.block_on(User::all(&db))
        .expect("the users")
        .into_iter()
        .find(|u| u.email == "demo@example.com")
        .expect("the demo user")
        .id
}

/// `POST /login` the way the page sends it; the answer's `location`.
fn log_in_as(app: &TestApp, password: &str) -> Option<String> {
    app.post_alloy(
        "/login",
        &smeltery::json!({ "email": "demo@example.com", "password": password, "remember": false }),
    )
    .header("location")
    .map(str::to_owned)
}

#[test]
fn users_can_update_their_profile() {
    let app = app_with_demo_user();
    log_in(&app);
    // The address decides where password reset links go: the page and the form ask for the password first.
    let res = app.get_alloy("/settings/profile");
    assert_eq!(res.header("location"), Some("/user/confirm-password"));
    let res = app.post_form(
        "/user/profile-information",
        &[
            ("_method", "PUT"),
            ("name", "Mallory"),
            ("email", "mallory@example.com"),
        ],
    );
    assert_eq!(res.header("location"), Some("/user/confirm-password"));
    let res = confirm_password(&app, "password");
    assert_eq!(res.header("location"), Some("/settings/profile"));
    let page = app.get_alloy("/settings/profile");
    assert_eq!(page.header("cache-control"), Some("no-store"));
    page.assert_component("settings/Profile")
        .assert_prop("auth.user.email", "demo@example.com");
    // The page sends PUT; an HTML form would send POST with `_method=PUT`, the same route.
    let res = app.post_form(
        "/user/profile-information",
        &[
            ("_method", "PUT"),
            ("name", "Ada Lovelace"),
            ("email", " Ada@Example.com "),
        ],
    );
    assert_eq!(res.status(), 303);
    let page = app.get_alloy("/settings/profile");
    assert_eq!(
        page.alloy_page()["flash"]["status"],
        "Your profile has been updated."
    );
    page.assert_prop("auth.user.name", "Ada Lovelace")
        .assert_prop("auth.user.email", "ada@example.com");
}

#[test]
fn users_can_change_their_password_and_stay_signed_in() {
    let app = app_with_demo_user();
    log_in(&app);
    let change = |current: &str| {
        app.post_form(
            "/user/password",
            &[
                ("_method", "PUT"),
                ("current_password", current),
                ("password", "a-new-password"),
                ("password_confirmation", "a-new-password"),
            ],
        )
    };
    assert_eq!(change("wrong-password").status(), 303);
    app.get_alloy("/settings/password")
        .assert_component("settings/Password")
        .assert_prop(
            "errors.current_password",
            "The provided password does not match your current password.",
        );
    assert_eq!(change("password").status(), 303);
    assert_eq!(
        app.get_alloy("/dashboard").status(),
        200,
        "this device stays signed in"
    );
    assert_eq!(
        app.post_alloy("/logout", &smeltery::json!({})).status(),
        303
    );
    assert_eq!(log_in_as(&app, "password").as_deref(), Some("/login"));
    assert_eq!(
        log_in_as(&app, "a-new-password").as_deref(),
        Some("/dashboard")
    );
}

#[test]
fn two_factor_can_be_enabled_confirmed_and_used_to_log_in() {
    let app = app_with_demo_user();
    fake_clock(app.app(), T);
    let id = demo_user_id(&app);
    log_in(&app);
    assert_eq!(confirm_password(&app, "password").status(), 303);
    let res = app.post_alloy("/user/two-factor-authentication", &smeltery::json!({}));
    assert_eq!(res.status(), 303);
    let page = app.get_alloy("/settings/two-factor");
    assert_eq!(page.header("cache-control"), Some("no-store"));
    page.assert_component("settings/TwoFactor")
        .assert_prop("two_factor.enabled", true)
        .assert_prop("two_factor.confirmed", false);
    let codes = page.prop("recovery_codes").expect("the recovery codes");
    assert_eq!(
        codes.as_array().map(Vec::len),
        Some(8),
        "shown once, after enabling"
    );
    // The page fetches the QR code while enrolling; it is never a prop.
    page.assert_missing("qr_code_url");
    let mut accept = smeltery::http::HeaderMap::new();
    accept.insert(
        smeltery::http::header::ACCEPT,
        smeltery::http::HeaderValue::from_static("application/json"),
    );
    let qr = app.request(
        smeltery::http::Method::GET,
        "/user/two-factor-qr-code",
        accept,
        "".into(),
    );
    assert_eq!(qr.status(), 200);
    assert!(
        qr.json()["url"]
            .as_str()
            .is_some_and(|u| u.starts_with("data:image/svg+xml;base64,"))
    );
    let code = two_factor_code::<User>(&app, id);
    let res = app.post_alloy(
        "/user/confirmed-two-factor-authentication",
        &smeltery::json!({ "code": code }),
    );
    assert_eq!(res.status(), 303);
    app.get_alloy("/settings/two-factor")
        .assert_prop("two_factor.confirmed", true)
        .assert_prop("auth.user.two_factor_enabled", true)
        .assert_prop("recovery_codes", smeltery::json!([]));
    assert_eq!(
        app.post_alloy("/logout", &smeltery::json!({})).status(),
        303
    );
    // The password alone does not sign in: the challenge asks for a code.
    assert_eq!(
        log_in_as(&app, "password").as_deref(),
        Some("/two-factor-challenge")
    );
    assert_eq!(app.get("/dashboard").status(), 303);
    app.get_alloy("/two-factor-challenge")
        .assert_component("auth/TwoFactorChallenge");
    // The code that confirmed the enrolment is used up: the app shows the next one 30 seconds later.
    fake_clock(app.app(), T + 30);
    let code = two_factor_code::<User>(&app, id);
    let res = app.post_alloy("/two-factor-challenge", &smeltery::json!({ "code": code }));
    assert_eq!(res.header("location"), Some("/dashboard"));
    app.get_alloy("/dashboard")
        .assert_prop("auth.user.name", "Demo User");
}

#[test]
fn a_recovery_code_logs_in_once() {
    let app = app_with_demo_user();
    fake_clock(app.app(), T);
    let codes = enable_two_factor::<User>(&app, demo_user_id(&app));
    let code = codes.first().expect("a recovery code").clone();
    assert_eq!(
        log_in_as(&app, "password").as_deref(),
        Some("/two-factor-challenge")
    );
    let res = app.post_alloy(
        "/two-factor-challenge",
        &smeltery::json!({ "recovery_code": code }),
    );
    assert_eq!(res.header("location"), Some("/dashboard"));
    assert_eq!(
        app.post_alloy("/logout", &smeltery::json!({})).status(),
        303
    );
    log_in_as(&app, "password");
    let res = app.post_alloy(
        "/two-factor-challenge",
        &smeltery::json!({ "recovery_code": code }),
    );
    assert_eq!(res.header("location"), Some("/two-factor-challenge"));
    app.get_alloy("/two-factor-challenge").assert_prop(
        "errors.recovery_code",
        "The provided two factor authentication code was invalid.",
    );
    assert_eq!(app.get("/dashboard").status(), 303);
}

#[test]
fn settings_need_a_recent_password_confirmation() {
    let app = app_with_demo_user();
    log_in(&app);
    assert_eq!(app.get_alloy("/settings/password").status(), 200);
    assert_eq!(
        app.get_alloy("/settings/profile").header("location"),
        Some("/user/confirm-password")
    );
    let res = app.get_alloy("/settings/two-factor");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/user/confirm-password"));
    app.get_alloy("/user/confirm-password")
        .assert_component("auth/ConfirmPassword");
    confirm_password(&app, "wrong-password");
    app.get_alloy("/user/confirm-password")
        .assert_prop("errors.password", "The provided password was incorrect.");
    let res = confirm_password(&app, "password");
    assert_eq!(res.header("location"), Some("/settings/two-factor"));
    assert_eq!(app.get_alloy("/settings/two-factor").status(), 200);
    // Guests go to the login page.
    assert_eq!(
        app.post_alloy("/logout", &smeltery::json!({})).status(),
        303
    );
    assert_eq!(
        app.get("/settings/profile").header("location"),
        Some("/login")
    );
}
#[test]
fn ending_a_users_credentials_signs_them_out_everywhere() {
    let app = app_with_demo_user();
    log_in(&app);
    assert_eq!(app.get_alloy("/dashboard").status(), 200);
    let id = demo_user_id(&app);
    app.block_on(smeltery::auth::end_credentials(app.app(), id))
        .expect("ending the credentials");
    let res = app.get_alloy("/dashboard");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
}
