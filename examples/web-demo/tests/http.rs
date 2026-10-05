//! HTTP tests: requests against the app in memory, no server needed. Every `TestApp` starts with a fresh, migrated
//! database and keeps its cookies between requests, so sessions and logins carry over.

use smeltery::db::seed::Seeder;
use smeltery::mail::{Mailer, ResetPassword};
use smeltery::sparks::testing::TestSpark;
use smeltery::testing::TestApp;

use web_demo::database::seeders::database_seeder::DatabaseSeeder;

/// A fresh app with the demo user (`demo@example.com` / `password`).
fn app_with_demo_user() -> TestApp {
    let app = TestApp::new(web_demo::build);
    let db = app.db();
    app.block_on(DatabaseSeeder.run(&db))
        .expect("seeding the demo user");
    app
}

#[test]
fn home_page_works() {
    let app = TestApp::new(web_demo::build);
    let res = app.get("/");
    assert_eq!(res.status(), 200);
    assert!(res.text().contains("Web Demo"));
}

#[test]
fn the_counter_spark_counts() {
    let app = TestApp::new(web_demo::build);
    let html = app.get("/").text();
    assert!(html.contains("wire:name=\"counter\""));
    let mut counter =
        TestSpark::from_html(&html, "counter").expect("the home page shows the counter");
    assert_eq!(counter.data()["count"], 0);
    assert_eq!(
        counter
            .call("increment", smeltery::json!([]))
            .send(&app)
            .status(),
        200
    );
    assert_eq!(counter.data()["count"], 1);
    counter.set("step", 5);
    assert_eq!(
        counter
            .call("increment", smeltery::json!([]))
            .send(&app)
            .status(),
        200
    );
    assert_eq!(counter.data()["count"], 6);
}

#[test]
fn health_check_works() {
    let app = TestApp::new(web_demo::build);
    let res = app.get("/api/health");
    assert_eq!(res.status(), 200);
    assert!(res.text().contains("ok"));
}

#[test]
fn guests_are_sent_from_the_dashboard_to_the_login_page() {
    let app = TestApp::new(web_demo::build);
    let res = app.get("/dashboard");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
}

#[test]
fn new_users_can_register_and_are_logged_in() {
    let app = TestApp::new(web_demo::build);
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
    assert_eq!(res.header("location"), Some("/dashboard"));
    let dashboard = app.get("/dashboard");
    assert_eq!(dashboard.status(), 200);
    assert!(dashboard.text().contains("Ada Lovelace"));
}

#[test]
fn the_demo_user_can_log_in() {
    let app = app_with_demo_user();
    let res = app.post_form(
        "/login",
        &[("email", "demo@example.com"), ("password", "password")],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/dashboard"));
    assert!(app.get("/dashboard").text().contains("Demo User"));
}

#[test]
fn a_wrong_password_shows_an_error() {
    let app = app_with_demo_user();
    let res = app.post_form(
        "/login",
        &[
            ("email", "demo@example.com"),
            ("password", "wrong-password"),
        ],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
    assert!(
        app.get("/login")
            .text()
            .contains("These credentials do not match our records.")
    );
    assert_eq!(app.get("/dashboard").status(), 303);
}

#[test]
fn forgot_password_mails_a_reset_link() {
    let app = app_with_demo_user();
    let res = app.post_form("/forgot-password", &[("email", "demo@example.com")]);
    assert_eq!(res.status(), 303);
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
fn users_can_log_out() {
    let app = app_with_demo_user();
    app.post_form(
        "/login",
        &[("email", "demo@example.com"), ("password", "password")],
    );
    assert_eq!(app.get("/dashboard").status(), 200);
    let res = app.post_form("/logout", &[]);
    assert_eq!(res.status(), 303);
    assert_eq!(app.get("/dashboard").status(), 303);
}
