//! The intended URL: the `auth` middleware remembers the page a guest was turned away from, and
//! `auth.intended(default)` sends them there after the login, never off the site.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde::Deserialize;
use smeltery::auth::{self, Auth};
use smeltery::db::migration::{Migration, Migrator, Schema};
use smeltery::db::prelude::Set;
use smeltery::http::{HeaderMap, HeaderValue, Method, Query};
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
        pub email: String,
        pub password: String,
        pub remember_token: Option<String>,
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

struct CreateUsers;

impl Migration for CreateUsers {
    fn name(&self) -> &'static str {
        "2026_10_05_000001_create_users_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("users", |t| {
                t.id();
                t.string("email").unique();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("users").await
    }
}

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
}

/// The login handler of a generated app.
async fn login(app: App, auth: Auth, Form(f): Form<Login>) -> Result<Response> {
    if auth.attempt(&f.email, &f.password, false).await? {
        return Ok(auth.intended(&app.settings().auth_home).into_response());
    }
    Ok("wrong".into_response())
}

async fn login_page() -> &'static str {
    "login page"
}

async fn logout(auth: Auth) -> Result<&'static str> {
    auth.logout().await?;
    Ok("bye")
}

async fn page(uri: smeltery::http::Uri) -> String {
    format!("page {uri}")
}

#[derive(Deserialize)]
struct Remember {
    path: String,
}

async fn remember(auth: Auth, Query(q): Query<Remember>) -> String {
    auth.set_intended(&q.path).to_string()
}

/// Puts a value into the session the way a bug in app code could.
async fn plant(session: Session, Query(q): Query<Remember>) -> &'static str {
    session.insert("_intended_url", q.path);
    "planted"
}

async fn go(auth: Auth) -> Redirect {
    auth.intended("/fallback")
}

fn build(app: AppBuilder) -> AppBuilder {
    app.migrations(|m: &mut Migrator| {
        m.add(CreateUsers);
    })
    .auth::<User>()
    .routes(|r| {
        r.get("/login", login_page).name("login");
        r.post("/login", login);
        r.post("/logout", logout);
        r.get("/account", page).middleware("auth");
        r.post("/account", page).middleware("auth");
        r.get("/remember", remember);
        r.get("/plant", plant);
        r.get("/go", go);
        // A catch-all behind `auth` sees request paths such as `//evil.example/x`.
        r.get("/{*rest}", page).middleware("auth");
    })
}

fn app() -> TestApp {
    let app = TestApp::new(build);
    let db = app.db();
    app.block_on(async {
        User::create(
            &db,
            user::ActiveModel {
                email: Set("ada@example.com".into()),
                password: Set(auth::hash_password("pw").await.unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    });
    app
}

fn sign_in(app: &TestApp) -> TestResponse {
    app.post_form(
        "/login",
        &[("email", "ada@example.com"), ("password", "pw")],
    )
}

fn location(res: &TestResponse) -> (u16, Option<&str>) {
    (res.status(), res.header("location"))
}

fn get_with(app: &TestApp, path: &str, headers: &[(&'static str, &'static str)]) -> TestResponse {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.append(*name, HeaderValue::from_static(value));
    }
    app.request(Method::GET, path, map, String::new().into())
}

#[test]
fn a_guest_returns_to_the_page_after_signing_in() {
    let app = app();
    assert_eq!(
        location(&app.get("/account?tab=2&x=%2F%2Fa")),
        (303, Some("/login"))
    );
    assert_eq!(
        location(&sign_in(&app)),
        (303, Some("/account?tab=2&x=%2F%2Fa"))
    );
    assert_eq!(
        app.get("/account?tab=2&x=%2F%2Fa").text(),
        "page /account?tab=2&x=%2F%2Fa"
    );
    // Used once: the next login goes to the default.
    app.post_form("/logout", &[]);
    assert_eq!(location(&sign_in(&app)), (303, Some("/dashboard")));
}

#[test]
fn without_a_remembered_page_the_default_is_used() {
    let app = app();
    assert_eq!(location(&sign_in(&app)), (303, Some("/dashboard")));
    assert_eq!(location(&app.get("/go")), (303, Some("/fallback")));
}

#[test]
fn the_latest_page_wins_and_logout_forgets_it() {
    let app = app();
    app.get("/account?first");
    app.get("/reports/7");
    assert_eq!(location(&sign_in(&app)), (303, Some("/reports/7")));

    app.post_form("/logout", &[]);
    app.get("/account?later");
    app.post_form("/logout", &[]);
    assert_eq!(location(&sign_in(&app)), (303, Some("/dashboard")));
}

#[test]
fn only_page_visits_are_remembered() {
    let app = app();
    // A JSON client gets 401; a form post, a prefetch, a background fetch and an event stream
    // are not pages to return to.
    assert_eq!(app.get_json("/account?json").status(), 401);
    assert_eq!(app.post_form("/account?post", &[]).status(), 303);
    for headers in [
        &[("purpose", "prefetch")][..],
        &[("sec-purpose", "prefetch;prerender")],
        &[("x-requested-with", "XMLHttpRequest")],
        &[("accept", "text/event-stream")],
    ] {
        assert_eq!(
            location(&get_with(&app, "/account?skip", headers)),
            (303, Some("/login"))
        );
    }
    let head = app.request(
        Method::HEAD,
        "/account?head",
        HeaderMap::new(),
        String::new().into(),
    );
    assert_eq!(head.status(), 303);
    assert_eq!(location(&sign_in(&app)), (303, Some("/dashboard")));

    // An Inertia visit is a page visit (its client also sends `X-Requested-With`).
    let app = self::app();
    get_with(
        &app,
        "/account?inertia",
        &[
            ("x-inertia", "true"),
            ("x-requested-with", "XMLHttpRequest"),
            ("accept", "text/html, application/xhtml+xml"),
        ],
    );
    assert_eq!(location(&sign_in(&app)), (303, Some("/account?inertia")));
}

#[test]
fn subresources_and_background_fetches_never_replace_the_page() {
    let navigate = &[
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-dest", "document"),
    ][..];
    let cases: [&[(&'static str, &'static str)]; 8] = [
        // An `<img>` on the login page pointing at a protected URL.
        &[("sec-fetch-mode", "no-cors"), ("sec-fetch-dest", "image")],
        &[("sec-fetch-mode", "no-cors"), ("sec-fetch-dest", "script")],
        &[("sec-fetch-mode", "no-cors"), ("sec-fetch-dest", "style")],
        // A frame is a navigation, but not of the page the user sees.
        &[("sec-fetch-mode", "navigate"), ("sec-fetch-dest", "iframe")],
        // `fetch()` / htmx without `X-Requested-With`.
        &[("sec-fetch-mode", "cors"), ("sec-fetch-dest", "empty")],
        &[("sec-fetch-mode", "same-origin")],
        &[("sec-fetch-dest", "image")],
        // `X-Inertia` on an image request is not an Inertia visit.
        &[
            ("x-inertia", "true"),
            ("sec-fetch-mode", "no-cors"),
            ("sec-fetch-dest", "image"),
        ],
    ];
    for headers in cases {
        let app = app();
        get_with(&app, "/account?page", navigate);
        assert_eq!(
            location(&get_with(&app, "/avatars/7", headers)),
            (303, Some("/login")),
            "{headers:?}"
        );
        assert_eq!(
            location(&sign_in(&app)),
            (303, Some("/account?page")),
            "{headers:?}"
        );
    }

    // A browser navigation is remembered, and so is an Inertia visit (a `cors` fetch).
    let app = app();
    get_with(&app, "/account?navigated", navigate);
    assert_eq!(location(&sign_in(&app)), (303, Some("/account?navigated")));
    let app = self::app();
    get_with(
        &app,
        "/account?inertia",
        &[
            ("x-inertia", "true"),
            ("sec-fetch-mode", "cors"),
            ("sec-fetch-dest", "empty"),
        ],
    );
    assert_eq!(location(&sign_in(&app)), (303, Some("/account?inertia")));
}

#[test]
fn unicode_lookalike_slashes_are_refused() {
    let app = app();
    // U+FF0F (fullwidth solidus) and U+2215 (division slash): not ASCII, so never remembered
    // (a `Location` cannot carry them, and request paths never hold them unencoded).
    for lookalike in ["/\u{FF0F}evil.example", "/\u{2215}evil.example"] {
        let query = serde_urlencoded::to_string([("path", lookalike)]).unwrap();
        assert_eq!(
            app.get(&format!("/remember?{query}")).text(),
            "false",
            "{lookalike:?}"
        );
        assert_eq!(location(&app.get("/go")), (303, Some("/fallback")));
        // Planted in the session by app code: ignored too.
        assert_eq!(app.get(&format!("/plant?{query}")).text(), "planted");
        assert_eq!(location(&app.get("/go")), (303, Some("/fallback")));
    }
    // The percent-encoded form is an ordinary path on this site.
    assert_eq!(
        app.get("/remember?path=%2F%25EF%25BC%258Fevil.example")
            .text(),
        "true"
    );
    assert_eq!(
        location(&app.get("/go")),
        (303, Some("/%EF%BC%8Fevil.example"))
    );
}

#[test]
fn the_host_header_never_reaches_the_redirect() {
    let app = app();
    get_with(&app, "/account", &[("host", "evil.example")]);
    get_with(
        &app,
        "/account",
        &[
            ("host", "evil.example"),
            ("x-forwarded-host", "evil.example"),
        ],
    );
    assert_eq!(location(&sign_in(&app)), (303, Some("/account")));
}

#[test]
fn off_site_paths_are_never_remembered() {
    let app = app();
    // Reaches the catch-all route behind `auth`.
    let res = app.get("//evil.example/x");
    assert_eq!(location(&res), (303, Some("/login")));
    assert_eq!(location(&sign_in(&app)), (303, Some("/dashboard")));

    // A refused visit also drops an older remembered page.
    app.post_form("/logout", &[]);
    app.get("/account");
    app.get("//evil.example/x");
    assert_eq!(location(&sign_in(&app)), (303, Some("/dashboard")));

    // Too long for a cookie session: not remembered.
    app.post_form("/logout", &[]);
    let long = format!("/account?q={}", "a".repeat(2100));
    assert_eq!(app.get(&long).status(), 303);
    assert_eq!(location(&sign_in(&app)), (303, Some("/dashboard")));
}

#[test]
fn set_intended_refuses_anything_but_a_local_path() {
    let app = app();
    let remember = |path: &str| {
        let query = serde_urlencoded::to_string([("path", path)]).unwrap();
        app.get(&format!("/remember?{query}")).text()
    };
    for bad in [
        "https://evil.example/",
        "//evil.example",
        "///evil.example",
        "/\\evil.example",
        "\\\\evil.example",
        "/\t/evil.example",
        "/\r\n/evil.example",
        "javascript:alert(1)",
        "evil.example",
        "",
    ] {
        assert_eq!(remember(bad), "false", "{bad:?}");
    }
    assert_eq!(location(&app.get("/go")), (303, Some("/fallback")));
    assert_eq!(remember("/reports?id=7"), "true");
    assert_eq!(location(&app.get("/go")), (303, Some("/reports?id=7")));
    assert_eq!(location(&app.get("/go")), (303, Some("/fallback")));
}

#[test]
fn a_bad_value_in_the_session_is_ignored() {
    let app = app();
    for bad in [
        "https://evil.example/",
        "//evil.example",
        "/\\evil.example",
        "/\t/evil.example",
    ] {
        let query = serde_urlencoded::to_string([("path", bad)]).unwrap();
        assert_eq!(app.get(&format!("/plant?{query}")).text(), "planted");
        assert_eq!(
            location(&app.get("/go")),
            (303, Some("/fallback")),
            "{bad:?}"
        );
    }
}
