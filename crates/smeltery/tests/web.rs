//! Sessions, CSRF, validation, flash messages and authentication through the facade, the way a
//! generated app uses them.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use serde::Deserialize;
use smeltery::auth::{self, Auth, passwords};
use smeltery::db::migration::{Migration, Migrator, Schema};
use smeltery::http::{HeaderMap, HeaderValue, Method, header};
use smeltery::prelude::*;
use smeltery::testing::{TestApp, TestResponse};

mod user {
    //! The `User` model (table `users`).
    use smeltery::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub email: String,
        pub password: String,
        pub remember_token: Option<String>,
        pub created_at: Option<DateTimeUtc>,
        pub updated_at: Option<DateTimeUtc>,
    }

    impl ActiveModelBehavior for ActiveModel {}

    impl smeltery::auth::Authenticatable for Model {
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

use user::Model as User;

struct CreateTables;

impl Migration for CreateTables {
    fn name(&self) -> &'static str {
        "2026_10_03_000001_create_auth_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("users", |t| {
                t.id();
                t.string("name");
                t.string("email").unique();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
                t.timestamps();
            })
            .await?;
        schema
            .create("password_reset_tokens", |t| {
                t.foreign_id("user_id")
                    .unique()
                    .constrained("users")
                    .cascade_on_delete();
                t.string("token");
                t.datetime("created_at").nullable();
            })
            .await?;
        schema
            .create("sessions", |t| {
                t.string("id").unique();
                t.text("payload");
                t.big_integer("last_activity").index();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("sessions").await?;
        schema.drop_if_exists("password_reset_tokens").await?;
        schema.drop_if_exists("users").await
    }
}

// ---- handlers -------------------------------------------------------------------------

async fn put(session: Session, Path(value): Path<String>) -> &'static str {
    session.insert("value", value);
    "stored"
}

async fn read(session: Session) -> String {
    session
        .get::<String>("value")
        .unwrap_or_else(|| "none".into())
}

async fn flash(session: Session) -> Redirect {
    session.flash("status", "Saved.");
    Redirect::to("/read-flash")
}

async fn read_flash(session: Session) -> String {
    session
        .get::<String>("status")
        .unwrap_or_else(|| "none".into())
}

async fn session_id(session: Session) -> String {
    session.id()
}

async fn regenerate(session: Session) -> &'static str {
    session.regenerate();
    "ok"
}

async fn invalidate(session: Session) -> &'static str {
    session.invalidate();
    "ok"
}

async fn token(session: Session) -> String {
    session.token()
}

async fn submit() -> &'static str {
    "accepted"
}

#[derive(Debug, Deserialize, smeltery::Validate)]
struct Everything {
    #[validate(required, max = 10)]
    name: String,
    #[validate(required, email)]
    email: String,
    #[validate(url)]
    site: Option<String>,
    #[validate(min = 3)]
    code: Option<String>,
    #[validate(integer, between(1, 120))]
    age: Option<i64>,
    #[validate(numeric)]
    price: Option<String>,
    #[validate(alpha)]
    a: Option<String>,
    #[validate(alpha_num)]
    an: Option<String>,
    #[validate(alpha_dash)]
    ad: Option<String>,
    #[validate(in_list("red", "blue"))]
    color: Option<String>,
    #[validate(required, min = 8, confirmed)]
    password: String,
    #[validate(same = "email")]
    email_again: Option<String>,
    #[validate(accepted)]
    terms: Option<String>,
    #[validate(required, message = "Tell us your city.")]
    city: Option<String>,
    #[validate(max = 2)]
    tags: Option<Vec<String>>,
}

async fn everything(Valid(form): Valid<Everything>) -> String {
    format!("ok {:?} {:?}", form.age, form.site)
}

#[derive(Debug, Deserialize, smeltery::Validate)]
struct Signup {
    #[validate(required, email)]
    email: String,
    #[validate(required)]
    name: String,
    #[validate(required)]
    password: String,
}

async fn signup(session: Session, Valid(form): Valid<Signup>) -> Redirect {
    session.flash("status", format!("Welcome {}", form.name));
    Redirect::to("/signup")
}

#[derive(Mold)]
#[mold("signup", dir = "tests/app/resources/views")]
struct SignupPage {}

async fn signup_page() -> SignupPage {
    SignupPage {}
}

#[derive(Debug, Deserialize, smeltery::Validate)]
struct UserForm {
    #[validate(required, unique(table = "users", column = "email", except_id))]
    email: String,
    #[validate(exists(table = "users", column = "id"))]
    manager_id: Option<i64>,
}

async fn user_form(Valid(_form): Valid<UserForm>) -> &'static str {
    "ok"
}

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
    remember: Option<String>,
}

async fn login(auth: Auth, Form(f): Form<Login>) -> Result<&'static str> {
    Ok(
        if auth
            .attempt(&f.email, &f.password, f.remember.is_some())
            .await?
        {
            "in"
        } else {
            "out"
        },
    )
}

async fn login_page() -> &'static str {
    "login page"
}

async fn me(auth: Auth) -> Result<String> {
    let user = auth.user::<User>().await?.expect("signed in");
    Ok(format!("{} #{}", user.name, auth.id().unwrap_or_default()))
}

async fn logout(auth: Auth) -> Result<&'static str> {
    auth.logout().await?;
    Ok("bye")
}

async fn api_submit() -> &'static str {
    "api ok"
}

fn build(app: AppBuilder) -> AppBuilder {
    let mut app = app
        .migrations(|m: &mut Migrator| {
            m.add(CreateTables);
        })
        .auth::<User>()
        .routes(|r| {
            r.get("/put/{value}", put);
            r.get("/read", read);
            r.get("/flash", flash);
            r.get("/read-flash", read_flash);
            r.get("/id", session_id);
            r.get("/regenerate", regenerate);
            r.get("/invalidate", invalidate);
            r.get("/token", token);
            r.post("/submit", submit);
            r.post("/everything", everything);
            r.get("/signup", signup_page);
            r.post("/signup", signup);
            r.post("/users", user_form);
            r.post("/users/{user}", user_form);
            r.get("/login", login_page).name("login");
            r.post("/login", login);
            r.get("/me", me).middleware("auth");
            r.get("/register", login_page).middleware("guest");
            r.post("/logout", logout);
        })
        .api_routes(|r| {
            r.post("/submit", api_submit);
        });
    app.settings_mut().root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
    app
}

fn form(
    app: &TestApp,
    path: &str,
    fields: &[(&str, &str)],
    extra: &[(&str, &str)],
) -> TestResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    for (k, v) in extra {
        headers.insert(
            header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    let body = serde_urlencoded::to_string(fields).unwrap();
    app.request(Method::POST, path, headers, body.into())
}

fn create_user(app: &TestApp, email: &str, password: &str) -> User {
    let db = app.db();
    app.block_on(async {
        let hash = auth::hash_password(password).await.unwrap();
        User::create(
            &db,
            user::ActiveModel {
                name: Set("Ada".into()),
                email: Set(email.into()),
                password: Set(hash),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    })
}

const SESSION: &str = "smeltery_session";
const REMEMBER: &str = "remember_smeltery_session";

// ---- sessions -------------------------------------------------------------------------

fn session_round_trip(app: &TestApp) {
    assert_eq!(app.get("/read").text(), "none");
    assert_eq!(app.get("/put/hello").text(), "stored");
    assert_eq!(app.get("/read").text(), "hello");
    // Tampering with the cookie gives a fresh, empty session.
    app.set_cookie(SESSION, "not-encrypted");
    assert_eq!(app.get("/read").text(), "none");
}

#[test]
fn cookie_session_round_trip_and_cookie_flags() {
    let app = TestApp::new(build);
    let res = app.get("/put/x");
    let set = res.header("set-cookie").unwrap().to_owned();
    assert!(set.starts_with("smeltery_session="), "{set}");
    for flag in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=7200"] {
        assert!(set.contains(flag), "{flag} in {set}");
    }
    assert!(!set.contains("Secure"));
    assert!(!set.contains("\"value\""), "the payload is encrypted");
    app.clear_cookies();
    session_round_trip(&app);
}

#[test]
fn database_session_round_trip() {
    let app = TestApp::new(|b| {
        let mut b = build(b);
        b.settings_mut().session_driver = "database".into();
        b
    });
    session_round_trip(&app);
    app.get("/put/stored-in-db");
    let db = app.db();
    let rows = app
        .block_on(db.execute("UPDATE sessions SET last_activity = last_activity"))
        .unwrap();
    assert!(rows >= 1, "the session lives in the table");
    let id = app.get("/id").text();
    assert_eq!(app.get("/id").text(), id, "stable id");
    // The cookie holds only the (encrypted) id, not the data.
    assert!(app.cookie(SESSION).unwrap().len() < 200);
    assert_eq!(app.get("/read").text(), "stored-in-db");
    // Regenerating moves the data to a new row and deletes the old one.
    app.get("/regenerate");
    assert_ne!(app.get("/id").text(), id);
    assert_eq!(app.get("/read").text(), "stored-in-db");
}

fn session_files(root: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(root.join("storage/framework/sessions"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

fn file_sessions(root: &std::path::Path, lifetime: Option<Duration>) -> TestApp {
    let root = root.to_path_buf();
    TestApp::new(move |b| {
        let mut b = build(b);
        b.settings_mut().session_driver = "file".into();
        b.settings_mut().root = root;
        if let Some(lifetime) = lifetime {
            b.settings_mut().session_lifetime = lifetime;
        }
        b
    })
}

#[test]
fn file_session_round_trip() {
    let root = tempfile::tempdir().unwrap();
    let app = file_sessions(root.path(), None);
    session_round_trip(&app);
    app.clear_cookies();
    app.get("/put/stored-in-a-file");
    let id = app.get("/id").text();
    assert_eq!(app.get("/id").text(), id, "stable id");
    // One file per session, named by its id, holding the data; the cookie holds only the id.
    let files = session_files(root.path());
    assert!(files.contains(&id), "{files:?}");
    let text =
        std::fs::read_to_string(root.path().join("storage/framework/sessions").join(&id)).unwrap();
    assert!(text.contains("stored-in-a-file"), "{text}");
    assert!(app.cookie(SESSION).unwrap().len() < 200);
    assert_eq!(app.get("/read").text(), "stored-in-a-file");

    // Regenerating moves the data to a new file and deletes the old one.
    app.get("/regenerate");
    let new_id = app.get("/id").text();
    assert_ne!(new_id, id);
    assert_eq!(app.get("/read").text(), "stored-in-a-file");
    let files = session_files(root.path());
    assert!(!files.contains(&id) && files.contains(&new_id), "{files:?}");

    // Invalidating drops the data and the old file.
    app.get("/invalidate");
    let last_id = app.get("/id").text();
    assert_eq!(app.get("/read").text(), "none");
    let files = session_files(root.path());
    assert!(
        !files.contains(&new_id) && files.contains(&last_id),
        "{files:?}"
    );

    // A cookie for a session whose file is gone gives a fresh session.
    app.get("/put/again");
    std::fs::remove_file(
        root.path()
            .join("storage/framework/sessions")
            .join(&last_id),
    )
    .unwrap();
    assert_eq!(app.get("/read").text(), "none");
    assert_ne!(app.get("/id").text(), last_id);
    assert!(
        session_files(root.path())
            .iter()
            .all(|f| !f.ends_with(".tmp")),
        "no temp file is left behind"
    );
}

#[test]
fn file_sessions_sign_in_and_out() {
    let root = tempfile::tempdir().unwrap();
    let app = file_sessions(root.path(), None).with_csrf();
    create_user(&app, "ada@example.com", "secret-password");
    let token = app.get("/token").text();
    let before = app.get("/id").text();
    let res = form(
        &app,
        "/login",
        &[
            ("_token", &token),
            ("email", "ada@example.com"),
            ("password", "secret-password"),
        ],
        &[],
    );
    assert_eq!(res.text(), "in");
    assert_eq!(app.get("/me").status(), 200);
    let signed_in = app.get("/id").text();
    assert_ne!(signed_in, before, "login regenerates the session id");
    assert_eq!(session_files(root.path()), std::slice::from_ref(&signed_in));
    let token = app.get("/token").text();
    assert_eq!(
        form(&app, "/logout", &[("_token", &token)], &[]).text(),
        "bye"
    );
    assert!(!session_files(root.path()).contains(&signed_in));
    assert_ne!(app.get("/me").status(), 200);
}

#[test]
fn expired_sessions_start_fresh_in_every_driver() {
    for driver in ["cookie", "database"] {
        let app = TestApp::new(move |b| {
            let mut b = build(b);
            b.settings_mut().session_driver = driver.into();
            b.settings_mut().session_lifetime = Duration::ZERO;
            b
        });
        app.get("/put/x");
        assert_eq!(app.get("/read").text(), "none", "{driver}");
    }
    let root = tempfile::tempdir().unwrap();
    let app = file_sessions(root.path(), Some(Duration::ZERO));
    app.get("/put/x");
    assert_eq!(app.get("/read").text(), "none", "file");
}

#[test]
fn flash_lives_exactly_one_request() {
    let app = TestApp::new(build);
    let res = app.get("/flash");
    assert_eq!(res.status(), 303);
    assert_eq!(app.get("/read-flash").text(), "Saved.");
    assert_eq!(app.get("/read-flash").text(), "none");
}

#[test]
fn regenerate_keeps_data_and_invalidate_clears_it() {
    let app = TestApp::new(build);
    app.get("/put/kept");
    let id = app.get("/id").text();
    assert_eq!(id.len(), 40);
    app.get("/regenerate");
    let new_id = app.get("/id").text();
    assert_ne!(new_id, id);
    assert_eq!(app.get("/read").text(), "kept");
    app.get("/invalidate");
    assert_ne!(app.get("/id").text(), new_id);
    assert_eq!(app.get("/read").text(), "none");
}

// ---- CSRF -----------------------------------------------------------------------------

#[test]
fn csrf_is_checked_on_web_routes_only() {
    let app = TestApp::new(build).with_csrf();
    let res = form(&app, "/submit", &[("a", "b")], &[]);
    assert_eq!(res.status(), 419);
    assert!(res.text().contains("Page Expired"));
    let res = form(&app, "/submit", &[], &[("accept", "application/json")]);
    assert_eq!(res.status(), 419);
    assert_eq!(res.json()["error"], "CSRF token mismatch");
    let token = app.get("/token").text();
    let wrong = form(&app, "/submit", &[("_token", "wrong")], &[]);
    assert_eq!(wrong.status(), 419);

    // A form field, a header, or a multipart field.
    assert_eq!(
        form(&app, "/submit", &[("_token", &token)], &[]).text(),
        "accepted"
    );
    assert_eq!(
        form(&app, "/submit", &[], &[("x-csrf-token", &token)]).text(),
        "accepted"
    );
    let body = format!(
        "--b0\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nHi\r\n\
         --b0\r\nContent-Disposition: form-data; name=\"_token\"\r\n\r\n{token}\r\n--b0--\r\n"
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("multipart/form-data; boundary=b0"),
    );
    let res = app.request(Method::POST, "/submit", headers, body.into());
    assert_eq!(res.text(), "accepted");

    // API routes are exempt, and the testing environment skips the check by default.
    assert_eq!(
        app.post_json("/api/submit", &serde_json::json!({})).text(),
        "api ok"
    );
    let relaxed = TestApp::new(build);
    assert_eq!(form(&relaxed, "/submit", &[], &[]).text(), "accepted");
}

// ---- validation -----------------------------------------------------------------------

#[test]
fn every_rule_reports_its_message_as_json() {
    let app = TestApp::new(build);
    let res = app.post_json(
        "/everything",
        &serde_json::json!({
            "name": "abcdefghijkl",
            "email": "nope",
            "site": "ftp://x",
            "code": "ab",
            "age": 130,
            "price": "abc",
            "a": "ab1",
            "an": "a-b",
            "ad": "a b",
            "color": "green",
            "password": "short1",
            "password_confirmation": "other",
            "email_again": "x@y.z",
            "terms": "no",
            "tags": ["a", "b", "c"],
        }),
    );
    assert_eq!(res.status(), 422);
    let body = res.json();
    assert_eq!(body["message"], "The given data was invalid.");
    let expect = serde_json::json!({
        "name": ["The name field must not be greater than 10 characters."],
        "email": ["The email field must be a valid email address."],
        "site": ["The site field must be a valid URL."],
        "code": ["The code field must be at least 3 characters."],
        "age": ["The age field must be between 1 and 120."],
        "price": ["The price field must be a number."],
        "a": ["The a field must only contain letters."],
        "an": ["The an field must only contain letters and numbers."],
        "ad": ["The ad field must only contain letters, numbers, dashes, and underscores."],
        "color": ["The selected color is invalid."],
        "password": [
            "The password field must be at least 8 characters.",
            "The password field confirmation does not match."
        ],
        "email_again": ["The email again field must match email."],
        "terms": ["The terms field must be accepted."],
        "city": ["Tell us your city."],
        "tags": ["The tags field must not have more than 2 items."],
    });
    assert_eq!(body["errors"], expect);

    let ok = app.post_json(
        "/everything",
        &serde_json::json!({
            "name": "Ada", "email": "ada@example.com", "password": "long enough",
            "password_confirmation": "long enough", "city": "London", "age": 36,
            "site": "https://example.com", "terms": "yes", "color": "red",
        }),
    );
    assert_eq!(ok.text(), "ok Some(36) Some(\"https://example.com\")");
}

#[test]
fn empty_strings_are_absent_and_missing_fields_are_all_reported() {
    let app = TestApp::new(build);
    let fields = [
        ("name", "Ada"),
        ("email", "ada@example.com"),
        ("password", "long enough"),
        ("password_confirmation", "long enough"),
        ("city", "Paris"),
        ("age", ""),
        ("site", ""),
    ];
    assert_eq!(
        form(&app, "/everything", &fields, &[]).text(),
        "ok None None"
    );

    // Nothing deserializes: every required field is reported at once.
    let res = form(
        &app,
        "/everything",
        &[("name", ""), ("age", "x")],
        &[("accept", "application/json")],
    );
    assert_eq!(res.status(), 422);
    let errors = &res.json()["errors"];
    assert_eq!(errors["name"][0], "The name field is required.");
    assert_eq!(errors["email"][0], "The email field is required.");
    assert_eq!(errors["password"][0], "The password field is required.");
    assert_eq!(errors["city"][0], "Tell us your city.");
}

#[test]
fn web_failures_redirect_back_with_errors_and_old_input() {
    let app = TestApp::new(build);
    let res = form(
        &app,
        "/signup",
        &[
            ("email", "not-an-email"),
            ("name", ""),
            ("password", "secret"),
        ],
        &[
            ("referer", "http://localhost/signup"),
            ("host", "localhost"),
        ],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/signup"));

    let page = app.get("/signup").text();
    assert!(
        page.contains(r#"<input name="email" value="not-an-email">"#),
        "{page}"
    );
    assert!(page.contains("The email field must be a valid email address."));
    assert!(page.contains("The name field is required."));
    assert!(
        page.contains(r#"<input name="password" value="">"#),
        "passwords are not flashed"
    );
    // Errors and old input live for one request.
    let again = app.get("/signup").text();
    assert!(!again.contains("The name field is required."), "{again}");

    // Success: a flash message for the next page.
    let ok = form(
        &app,
        "/signup",
        &[
            ("email", "ada@example.com"),
            ("name", "Ada"),
            ("password", "x"),
        ],
        &[],
    );
    assert_eq!(ok.status(), 303);
    assert!(app.get("/signup").text().contains("<p>[Welcome Ada]</p>"));
    assert!(app.get("/signup").text().contains("<p>[]</p>"));
}

#[test]
fn database_rules_unique_and_exists() {
    let app = TestApp::new(build);
    let ada = create_user(&app, "ada@example.com", "password");
    let json = [("accept", "application/json")];
    let res = form(&app, "/users", &[("email", "ada@example.com")], &json);
    assert_eq!(
        res.json()["errors"]["email"][0],
        "The email has already been taken."
    );
    assert_eq!(
        form(&app, "/users", &[("email", "new@example.com")], &json).text(),
        "ok"
    );
    // `except_id` ignores the row of the route's key (an edit form).
    let own = format!("/users/{}", ada.id);
    assert_eq!(
        form(&app, &own, &[("email", "ada@example.com")], &json).text(),
        "ok"
    );
    let res = form(
        &app,
        "/users",
        &[("email", "b@example.com"), ("manager_id", "999")],
        &json,
    );
    assert_eq!(
        res.json()["errors"]["manager_id"][0],
        "The selected manager id is invalid."
    );
    let manager = ada.id.to_string();
    let res = form(
        &app,
        "/users",
        &[("email", "b@example.com"), ("manager_id", &manager)],
        &json,
    );
    assert_eq!(res.text(), "ok");
}

// ---- auth -----------------------------------------------------------------------------

#[test]
fn attempt_login_logout_and_middleware() {
    let app = TestApp::new(build);
    create_user(&app, "ada@example.com", "correct horse");

    // Guests: `auth` redirects to the login route, or answers 401 to JSON clients.
    let res = app.get("/me");
    assert_eq!(
        (res.status(), res.header("location")),
        (303, Some("/login"))
    );
    assert_eq!(app.get_json("/me").status(), 401);
    assert_eq!(app.get("/register").text(), "login page");

    let wrong = form(
        &app,
        "/login",
        &[("email", "ada@example.com"), ("password", "nope")],
        &[],
    );
    assert_eq!(wrong.text(), "out");
    let unknown = form(
        &app,
        "/login",
        &[("email", "who@example.com"), ("password", "x")],
        &[],
    );
    assert_eq!(unknown.text(), "out");
    let before = app.get("/id").text();
    let ok = form(
        &app,
        "/login",
        &[("email", "ada@example.com"), ("password", "correct horse")],
        &[],
    );
    assert_eq!(ok.text(), "in");
    assert_ne!(
        app.get("/id").text(),
        before,
        "login regenerates the session id"
    );
    assert!(app.get("/me").text().starts_with("Ada #"));
    // `guest` sends signed-in users to AUTH_HOME.
    let res = app.get("/register");
    assert_eq!(
        (res.status(), res.header("location")),
        (303, Some("/dashboard"))
    );
    assert!(app.get("/signup").text().contains("signed in"));

    assert_eq!(form(&app, "/logout", &[], &[]).text(), "bye");
    assert_eq!(app.get("/me").status(), 303);
}

#[test]
fn remember_me_signs_in_without_a_session() {
    let app = TestApp::new(build);
    let ada = create_user(&app, "ada@example.com", "correct horse");
    let res = form(
        &app,
        "/login",
        &[
            ("email", "ada@example.com"),
            ("password", "correct horse"),
            ("remember", "on"),
        ],
        &[],
    );
    assert_eq!(res.text(), "in");
    let set: Vec<&str> = res
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    let remember = set.iter().find(|c| c.starts_with(REMEMBER)).unwrap();
    assert!(remember.contains("Max-Age=157680000") && remember.contains("HttpOnly"));
    let stored = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap()
        .remember_token
        .unwrap();
    assert_eq!(stored.len(), 64, "a SHA-256 of the token, not the token");

    // A browser that lost its session cookie is signed in from the remember cookie.
    app.set_cookie(SESSION, "gone");
    assert!(app.get("/me").text().starts_with("Ada #"));

    // A forged remember cookie does nothing and is removed.
    let saved = app.cookie(REMEMBER).unwrap();
    app.set_cookie(REMEMBER, "forged");
    app.set_cookie(SESSION, "gone");
    assert_eq!(app.get("/me").status(), 303);
    assert!(app.cookie(REMEMBER).is_none());

    // Logout replaces the token with a new random one, so the old cookie no longer works.
    app.set_cookie(REMEMBER, &saved);
    app.set_cookie(SESSION, "gone");
    assert!(app.get("/me").text().starts_with("Ada"));
    form(&app, "/logout", &[], &[]);
    assert!(app.cookie(REMEMBER).is_none());
    let cycled = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap()
        .remember_token
        .unwrap();
    assert_eq!(cycled.len(), 64);
    assert_ne!(cycled, stored);
    app.set_cookie(REMEMBER, &saved);
    assert_eq!(app.get("/me").status(), 303);
}

#[test]
fn failed_logins_are_throttled() {
    let app = TestApp::new(build);
    create_user(&app, "ada@example.com", "correct horse");
    for _ in 0..5 {
        let res = form(
            &app,
            "/login",
            &[("email", "ada@example.com"), ("password", "x")],
            &[],
        );
        assert_eq!(res.text(), "out");
    }
    // JSON clients get 429 with the message on `email`.
    let res = form(
        &app,
        "/login",
        &[("email", "ADA@example.com"), ("password", "correct horse")],
        &[("accept", "application/json")],
    );
    assert_eq!(res.status(), 429);
    let message = res.json()["errors"]["email"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.starts_with("Too many login attempts. Please try again in "),
        "{message}"
    );
    // Web clients are sent back with the message flashed on `email` and their input.
    let res = form(
        &app,
        "/login",
        &[("email", "ada@example.com"), ("password", "correct horse")],
        &[("referer", "/signup")],
    );
    assert_eq!(
        (res.status(), res.header("location")),
        (303, Some("/signup"))
    );
    let page = app.get("/signup").text();
    assert!(page.contains("Too many login attempts."), "{page}");
    assert!(page.contains(r#"value="ada@example.com""#), "{page}");
    // Another email is not affected.
    create_user(&app, "bob@example.com", "pw");
    let res = form(
        &app,
        "/login",
        &[("email", "bob@example.com"), ("password", "pw")],
        &[],
    );
    assert_eq!(res.text(), "in");
}

#[test]
fn acting_as_signs_in_for_following_requests() {
    let app = TestApp::new(build);
    let ada = create_user(&app, "ada@example.com", "pw");
    app.acting_as(ada.id);
    assert_eq!(app.get("/me").text(), format!("Ada #{}", ada.id));
}

#[test]
fn password_reset_tokens() {
    let app = TestApp::new(build);
    let ada = create_user(&app, "ada@example.com", "old password");
    let db = app.db();

    // Unknown emails do nothing; known ones get a token row.
    app.block_on(passwords::send_reset_link(app.app(), "who@example.com"))
        .unwrap();
    app.block_on(passwords::send_reset_link(app.app(), "ada@example.com"))
        .unwrap();
    let token = app.block_on(passwords::create_token(&db, ada.id)).unwrap();
    assert_eq!(token.len(), 64);
    assert!(
        !app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            "wrong",
            "new password"
        ))
        .unwrap()
        .is_some()
    );
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            &token,
            "new password"
        ))
        .unwrap()
        .is_some()
    );
    assert!(
        !app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            &token,
            "again"
        ))
        .unwrap()
        .is_some(),
        "a token works once"
    );
    let res = form(
        &app,
        "/login",
        &[("email", "ada@example.com"), ("password", "new password")],
        &[],
    );
    assert_eq!(res.text(), "in");

    // Tokens expire after an hour.
    let token = app.block_on(passwords::create_token(&db, ada.id)).unwrap();
    let past = smeltery::db::prelude::ChronoUtc::now() - Duration::from_secs(61 * 60);
    let backend = db.conn().get_database_backend();
    app.block_on(db.conn().execute_raw(
        smeltery::db::prelude::sea_orm::Statement::from_sql_and_values(
            backend,
            "UPDATE password_reset_tokens SET created_at = ?",
            [smeltery::db::prelude::Value::from(past)],
        ),
    ))
    .unwrap();
    assert!(
        !app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            &token,
            "newer"
        ))
        .unwrap()
        .is_some()
    );
}

#[test]
fn hashes_are_argon2id() {
    let app = TestApp::new(build);
    let hash = app.block_on(auth::hash_password("pw")).unwrap();
    assert!(hash.starts_with("$argon2id$v=19$"));
    assert!(app.block_on(auth::verify_password("pw", &hash)).unwrap());
    assert!(!app.block_on(auth::verify_password("px", &hash)).unwrap());
}

use smeltery::db::prelude::{ConnectionTrait, Set};

#[test]
fn password_reset_links_are_mailed_when_mail_is_installed() {
    let app = TestApp::new(|b| build(b).mail());
    create_user(&app, "ada@example.com", "old password");
    let mailbox = Mailer::of(app.app()).unwrap().mailbox().unwrap();
    app.block_on(passwords::send_reset_link(app.app(), "who@example.com"))
        .unwrap();
    mailbox.assert_nothing_sent();
    app.block_on(passwords::send_reset_link(app.app(), "ada@example.com"))
        .unwrap();
    let (mail, email) = mailbox
        .sent_of::<smeltery::mail::ResetPassword>()
        .pop()
        .expect("the reset mail");
    assert!(email.has_recipient("ada@example.com"));
    // The link in the mail resets the password.
    let token = mail
        .url
        .split("/reset-password/")
        .nth(1)
        .and_then(|rest| rest.split('?').next())
        .unwrap()
        .to_owned();
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            &token,
            "new password"
        ))
        .unwrap()
        .is_some()
    );
}
