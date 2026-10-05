//! The authentication seams: guards and the principal, the `auth:` and other middleware families, `Authenticated`,
//! `throttle:` and `verified` reading the principal, the public `RateLimiter`, `App::encrypt`, `App::find_user`
//! and `TestApp`'s sticky headers.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use smeltery_core::auth::{self, Authenticated, Credential, Guard, GuardSet, Principal, WEB_GUARD};
use smeltery_core::cache::{RateLimit, RateLimiter};
use smeltery_core::db::migration::{Migration, Migrator, Schema};
use smeltery_core::http::request::Parts;
use smeltery_core::http::{HeaderMap, HeaderValue, Method, header};
use smeltery_core::middleware::{BoxedMiddleware, Next, Request};
use smeltery_core::testing::TestApp;
use smeltery_core::{App, AppBuilder, BoxFuture, Error, Result};

mod user {
    //! The `User` model (table `users`).
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub email: String,
        pub email_verified_at: Option<DateTimeUtc>,
        pub password: String,
        pub remember_token: Option<String>,
        pub created_at: Option<DateTimeUtc>,
        pub updated_at: Option<DateTimeUtc>,
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

    impl smeltery_core::auth::MustVerifyEmail for Model {
        fn email(&self) -> &str {
            &self.email
        }
        fn email_verified_at(&self) -> Option<DateTimeUtc> {
            self.email_verified_at
        }
    }
}

use user::Model as User;

struct CreateUsers;

impl Migration for CreateUsers {
    fn name(&self) -> &'static str {
        "2026_10_05_000001_create_users"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("users", |t| {
                t.id();
                t.string("email").unique();
                t.datetime("email_verified_at").nullable();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("users").await
    }
}

/// Accepts `X-Test-User: <id>` (a test double for a bearer guard) and counts its calls.
struct HeaderGuard {
    calls: Arc<AtomicUsize>,
}

impl Guard for HeaderGuard {
    fn name(&self) -> &'static str {
        "header"
    }

    fn stateless(&self) -> bool {
        true
    }

    fn authenticate<'a>(
        &'a self,
        _app: &'a App,
        parts: &'a mut Parts,
    ) -> BoxFuture<'a, Result<Option<Principal>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let id = parts
                .headers
                .get("x-test-user")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            Ok(id.map(|id| Principal::new(id, "header", Credential::token(id * 10, ["read"]))))
        })
    }
}

async fn who(who: Authenticated) -> String {
    format!("{} {} {}", who.guard, who.user_id, who.key())
}

async fn maybe(who: Option<Authenticated>) -> String {
    who.map_or_else(|| "guest".to_owned(), |w| w.user_id.to_string())
}

async fn ok() -> &'static str {
    "ok"
}

async fn email(app: App, who: Authenticated) -> Result<String> {
    let user = who.user::<User>(&app).await?;
    Ok(user.map_or_else(|| "no user".to_owned(), |u| u.email))
}

async fn login() -> &'static str {
    "login page"
}

fn build(calls: Arc<AtomicUsize>) -> impl FnOnce(AppBuilder) -> AppBuilder {
    move |b: AppBuilder| {
        b.migrations(|m: &mut Migrator| {
            m.add(CreateUsers);
        })
        .auth::<User>()
        .guard(HeaderGuard { calls })
        .routes(|r| {
            r.get("/login", login).name("login");
            r.get("/web/who", who).middleware("auth:web");
            r.get("/web/either", who).middleware("auth:header,web");
            r.get("/web/plain", who).middleware("auth");
            r.get("/web/header-only", who).middleware("auth:header");
            r.get("/web/maybe", maybe);
            r.get("/web/limited", ok).middleware("throttle:2,1");
        })
        .api_routes(|r| {
            r.get("/who", who).middleware("auth:header");
            r.get("/email", email).middleware("auth:header");
            r.get("/maybe", maybe);
            r.get("/limited", ok)
                .middleware("auth:header")
                .middleware("throttle:2,1");
            r.get("/verified", ok)
                .middleware("auth:header")
                .middleware("verified");
        })
    }
}

fn app() -> (TestApp, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (TestApp::new(build(Arc::clone(&calls))), calls)
}

fn create_user(app: &TestApp, email: &str) -> User {
    use smeltery_core::db::Record;
    use smeltery_core::db::prelude::Set;
    let db = app.db();
    app.block_on(async {
        User::create(
            &db,
            user::ActiveModel {
                email: Set(email.into()),
                password: Set(auth::hash_password("secret").await.unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    })
}

fn get_as(
    app: &TestApp,
    path: &str,
    user: Option<i64>,
    json: bool,
) -> smeltery_core::testing::TestResponse {
    let mut headers = HeaderMap::new();
    if let Some(id) = user {
        headers.insert(
            "x-test-user",
            HeaderValue::from_str(&id.to_string()).unwrap(),
        );
    }
    if json {
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    }
    app.request(Method::GET, path, headers, Default::default())
}

#[test]
fn a_stateless_guard_authenticates_api_routes() {
    let (app, _) = app();
    let res = get_as(&app, "/api/who", Some(7), false);
    assert_eq!(res.status(), 200);
    assert_eq!(res.text(), "header 7 header:token:70");

    let res = get_as(&app, "/api/who", None, false);
    assert_eq!(res.status(), 401);
    assert_eq!(res.header("www-authenticate"), Some("Bearer"));
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert_eq!(res.json()["error"], "Unauthenticated.");
    assert!(app.app().has_stateless_guard());
}

#[test]
fn the_principal_loads_its_user_once() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com");
    let res = get_as(&app, "/api/email", Some(ada.id), false);
    assert_eq!(res.text(), "ada@example.com");
    // A principal of a deleted user has no user.
    let res = get_as(&app, "/api/email", Some(ada.id + 100), false);
    assert_eq!(res.text(), "no user");
}

#[test]
fn stateless_guards_never_run_on_web_routes() {
    let (app, calls) = app();
    let ada = create_user(&app, "ada@example.com");
    // A bearer-style header is no session: refused on a web route even with a valid value.
    let res = get_as(&app, "/web/header-only", Some(ada.id), true);
    assert_eq!(res.status(), 401);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the guard never ran");
    // `Option<Authenticated>` on a web route reads the session only.
    assert_eq!(
        get_as(&app, "/web/maybe", Some(ada.id), false).text(),
        "guest"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn guards_run_in_order_and_the_session_is_the_web_guard() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com");
    // Guests on web routes: `auth:web` is the plain `auth` (303 to the login route). A list that also names a
    // stateless guard answers the same on a web route (review L1): there the session is the only credential, so a
    // signed-out browser goes to the login page; only JSON clients get a 401, without `WWW-Authenticate`.
    for path in ["/web/who", "/web/plain", "/web/either"] {
        let res = app.get(path);
        assert_eq!(res.status(), 303, "{path}");
        assert_eq!(res.header("location"), Some("/login"));
    }
    assert_eq!(get_as(&app, "/web/who", None, true).status(), 401);
    let res = get_as(&app, "/web/either", None, true);
    assert_eq!(res.status(), 401);
    assert_eq!(res.header("www-authenticate"), None);

    app.acting_as(ada.id);
    for path in ["/web/who", "/web/plain", "/web/either"] {
        let res = app.get(path);
        assert_eq!(res.status(), 200, "{path}");
        assert!(
            res.text()
                .starts_with(&format!("{WEB_GUARD} {} web:session:", ada.id)),
            "{}",
            res.text()
        );
    }
    // The session key is the 24-hex binding of the session.
    let key = app.get("/web/who").text();
    let binding = key.rsplit("session:").next().unwrap();
    assert_eq!(binding.len(), 24);
    assert!(binding.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(app.get("/web/maybe").text(), ada.id.to_string());
}

#[test]
fn authenticated_finds_stateless_principals_on_api_routes_without_middleware() {
    let (app, _) = app();
    assert_eq!(get_as(&app, "/api/maybe", None, false).text(), "guest");
    assert_eq!(get_as(&app, "/api/maybe", Some(3), false).text(), "3");
}

#[test]
fn throttle_counts_per_principal_user() {
    let (app, _) = app();
    for _ in 0..2 {
        assert_eq!(get_as(&app, "/api/limited", Some(1), false).status(), 200);
    }
    assert_eq!(get_as(&app, "/api/limited", Some(1), false).status(), 429);
    // Same address, another user: a budget of its own.
    assert_eq!(get_as(&app, "/api/limited", Some(2), false).status(), 200);
}

#[test]
fn verified_reads_the_principals_user() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = TestApp::new(|b| build(calls)(b).verify_email::<User>());
    let ada = create_user(&app, "ada@example.com");
    let res = get_as(&app, "/api/verified", Some(ada.id), false);
    assert_eq!(res.status(), 403, "unverified, JSON on an API route");
    assert_eq!(res.json()["error"], "Your email address is not verified.");
    let db = app.db();
    app.block_on(db.execute(&format!(
        "UPDATE users SET email_verified_at = '2026-10-05 00:00:00' WHERE id = {}",
        ada.id
    )))
    .unwrap();
    assert_eq!(
        get_as(&app, "/api/verified", Some(ada.id), false).status(),
        200
    );
    // Without a verification requirement every principal with a user passes.
    let (plain, _) = self::app();
    let bob = create_user(&plain, "bob@example.com");
    assert_eq!(
        get_as(&plain, "/api/verified", Some(bob.id), false).status(),
        200
    );
}

fn try_build(f: impl FnOnce(AppBuilder) -> AppBuilder) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut settings = smeltery_core::config::Settings::from_env();
    settings.env = "testing".into();
    settings.database_url = String::new();
    runtime
        .block_on(f(AppBuilder::new(settings)).build())
        .map(|_| ())
}

fn header_guard() -> HeaderGuard {
    HeaderGuard {
        calls: Arc::new(AtomicUsize::new(0)),
    }
}

/// Review L2: the public `authenticate` refuses web requests too, like the `auth:` family.
async fn authenticate_here(app: App, req: Request) -> String {
    let (mut parts, _) = req.into_parts();
    let found = auth::authenticate(&app, &mut parts, GuardSet::Stateless)
        .await
        .unwrap();
    found.map_or_else(|| "none".to_owned(), |p| p.user_id.to_string())
}

#[test]
fn authenticate_answers_none_on_web_requests() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = TestApp::new(|b| {
        build(Arc::clone(&calls))(b)
            .routes(|r| {
                r.get("/web/authenticate", authenticate_here);
            })
            .api_routes(|r| {
                r.get("/authenticate", authenticate_here);
            })
    });
    assert_eq!(
        get_as(&app, "/web/authenticate", Some(4), false).text(),
        "none"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no guard ran");
    assert_eq!(
        get_as(&app, "/api/authenticate", Some(4), false).text(),
        "4"
    );
}

#[test]
fn a_principal_takes_only_its_own_user() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com");
    let user = app.block_on(app.app().find_user(ada.id)).unwrap().unwrap();
    let mine = Principal::new(ada.id, "header", Credential::token(1, ["*"]));
    let loaded = mine.with_user(user.clone()).unwrap();
    assert_eq!(
        app.block_on(loaded.auth_user(app.app()))
            .unwrap()
            .unwrap()
            .id(),
        ada.id
    );
    let other = Principal::new(ada.id + 1, "header", Credential::token(1, ["*"]));
    assert!(other.with_user(user).is_err());
}

/// Review N4: a token whose user row is gone gets 401 from `verified`, not "not verified".
#[test]
fn verified_answers_401_when_the_principals_user_is_gone() {
    let (app, _) = app();
    let res = get_as(&app, "/api/verified", Some(999), false);
    assert_eq!(res.status(), 401);
    assert_eq!(res.json()["error"], "Unauthenticated.");
}

#[test]
fn bad_guards_and_families_fail_the_build() {
    let err = |f: fn(AppBuilder) -> AppBuilder| try_build(f).unwrap_err().to_string();
    assert!(
        err(|b| b.routes(|r| {
            r.get("/", ok).middleware("auth:nobody");
        }))
        .contains("no guard named `nobody`")
    );
    assert!(err(|b| b.guard(header_guard()).guard(header_guard())).contains("two guards"));
    assert!(
        err(|b| b.middleware_family("throttle", |_, _| Ok(BoxedMiddleware::new(pass))))
            .contains("prefix `throttle:`")
    );
    assert!(
        err(|b| b.middleware_family("auth", |_, _| Ok(BoxedMiddleware::new(pass))))
            .contains("prefix `auth:`")
    );
    assert!(
        err(|b| b.middleware_family("a:b", |_, _| Ok(BoxedMiddleware::new(pass))))
            .contains("invalid")
    );
    assert!(
        err(|b| b
            .middleware("role:admin", pass)
            .middleware_family("role", |_, _| Ok(BoxedMiddleware::new(pass))))
        .contains("clashes")
    );
    assert!(
        err(|b| b
            .middleware_family("role", |args, _| if args == "admin" {
                Ok(BoxedMiddleware::new(pass))
            } else {
                Err(Error::internal(format!("unknown role `{args}`")))
            })
            .routes(|r| {
                r.get("/", ok).middleware("role:nobody");
            }))
        .contains("unknown role `nobody`")
    );
    // `throttle:` keeps its own message.
    assert!(
        err(|b| b.routes(|r| {
            r.get("/", ok).middleware("throttle:five");
        }))
        .contains("throttle:<max>,<minutes>")
    );
    // `.auth::<User>()` twice registers `web` once.
    assert!(
        try_build(|b| b.auth::<User>().auth::<User>().routes(|r| {
            r.get("/", ok).middleware("auth:web");
        }))
        .is_ok()
    );
    struct Bad;
    impl Guard for Bad {
        fn name(&self) -> &'static str {
            "Bad Name"
        }
        fn stateless(&self) -> bool {
            true
        }
        fn authenticate<'a>(
            &'a self,
            _app: &'a App,
            _parts: &'a mut Parts,
        ) -> BoxFuture<'a, Result<Option<Principal>>> {
            Box::pin(async { Ok(None) })
        }
    }
    assert!(err(|b| b.guard(Bad)).contains("guard name"));
    // Review L3: `web` is core's session guard; another guard cannot take the name, before or after `.auth`.
    struct FakeWeb;
    impl Guard for FakeWeb {
        fn name(&self) -> &'static str {
            "web"
        }
        fn stateless(&self) -> bool {
            true
        }
        fn authenticate<'a>(
            &'a self,
            _app: &'a App,
            _parts: &'a mut Parts,
        ) -> BoxFuture<'a, Result<Option<Principal>>> {
            Box::pin(async { Ok(None) })
        }
    }
    assert!(err(|b| b.guard(FakeWeb).auth::<User>()).contains("`web` belongs to core"));
    assert!(err(|b| b.auth::<User>().guard(FakeWeb)).contains("`web` belongs to core"));
}

async fn pass(req: Request, next: Next) -> smeltery_core::Response {
    next.run(req).await
}

#[test]
fn a_middleware_family_builds_per_route_from_its_arguments() {
    let app = TestApp::new(|b| {
        b.middleware_family("tag", |args, route| {
            let value = HeaderValue::from_str(&format!("{args} @ {route}"))
                .map_err(|_| Error::internal("bad tag"))?;
            Ok(BoxedMiddleware::new(move |req: Request, next: Next| {
                let value = value.clone();
                async move {
                    let mut res = next.run(req).await;
                    res.headers_mut().insert("x-tag", value);
                    res
                }
            }))
        })
        .api_routes(|r| {
            r.get("/a", ok).middleware("tag:one");
            r.post("/b", ok).middleware("tag:two,three");
        })
    });
    assert_eq!(
        app.get("/api/a").header("x-tag"),
        Some("one @ GET|HEAD /api/a")
    );
    let res = app.post_json("/api/b", &serde_json::json!({}));
    assert_eq!(res.header("x-tag"), Some("two,three @ POST /api/b"));
    // `route:list` shows the alias as written.
    let route = app
        .app()
        .routes()
        .iter()
        .find(|r| r.path == "/api/b")
        .unwrap();
    assert_eq!(route.middleware, ["tag:two,three"]);
}

#[test]
fn authenticate_runs_only_the_stateless_guards() {
    let (app, calls) = app();
    let mut req = http::Request::builder()
        .uri("/x")
        .header("x-test-user", "9")
        .body(())
        .unwrap();
    let (mut parts, ()) = std::mem::replace(&mut req, http::Request::new(())).into_parts();
    let found = app
        .block_on(auth::authenticate(
            app.app(),
            &mut parts,
            GuardSet::Stateless,
        ))
        .unwrap()
        .unwrap();
    assert_eq!((found.user_id, found.guard), (9, "header"));
    assert!(found.can("read") && !found.can("write"));
    assert!(
        parts.extensions.get::<Principal>().is_none(),
        "nothing stored"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn sticky_test_headers_are_sent_until_removed() {
    let (app, _) = app();
    app.with_header("x-test-user", "5");
    assert_eq!(app.get("/api/who").status(), 200);
    // A header of the request itself wins.
    assert_eq!(
        get_as(&app, "/api/who", Some(6), false).text(),
        "header 6 header:token:60"
    );
    app.without_header("x-test-user");
    assert_eq!(app.get("/api/who").status(), 401);

    let echo = TestApp::new(|b| {
        b.api_routes(|r| {
            r.get("/auth", |headers: HeaderMap| async move {
                headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("none")
                    .to_owned()
            });
        })
    });
    echo.with_bearer("smt_example");
    assert_eq!(echo.get("/api/auth").text(), "Bearer smt_example");
}

// ---- RateLimiter -------------------------------------------------------------------------

#[test]
fn the_rate_limiter_counts_atomically_per_name_and_key() {
    let app = TestApp::new(|b| b);
    let limiter = Arc::new(RateLimiter::new("codes", 3, Duration::from_secs(60)));
    let passed = app.block_on(async {
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..20 {
            let (app, limiter) = (app.app().clone(), Arc::clone(&limiter));
            tasks.spawn(async move { limiter.hit(&app, "user:1").await.unwrap().allowed() });
        }
        let mut passed = 0;
        while let Some(ok) = tasks.join_next().await {
            passed += usize::from(ok.unwrap());
        }
        passed
    });
    assert_eq!(passed, 3);
    let limited = app.block_on(limiter.hit(app.app(), "user:1")).unwrap();
    assert!(
        matches!(limited, RateLimit::Limited { retry_after, .. } if (1..=60).contains(&retry_after))
    );
    // Another key, another name: their own counts.
    assert!(matches!(
        app.block_on(limiter.hit(app.app(), "user:2")).unwrap(),
        RateLimit::Allowed { remaining: 2, .. }
    ));
    let other = RateLimiter::new("other", 3, Duration::from_secs(60));
    assert!(
        app.block_on(other.hit(app.app(), "user:1"))
            .unwrap()
            .allowed()
    );
    // Clearing gives the key its budget back.
    app.block_on(limiter.clear(app.app(), "user:1")).unwrap();
    assert!(
        app.block_on(limiter.hit(app.app(), "user:1"))
            .unwrap()
            .allowed()
    );
    assert_eq!(limiter.max(), 3);
    assert_eq!(limiter.window(), Duration::from_secs(60));
}

/// Review M2: without a cache store, limiters built per call shared nothing (a fresh budget each time).
#[test]
fn same_name_limiters_share_their_counts_without_a_store() {
    let null = TestApp::new(|mut b| {
        b.settings_mut().cache_store = "null".to_owned();
        b
    });
    let hit = || {
        null.block_on(RateLimiter::new("x", 1, Duration::from_secs(60)).hit(null.app(), "k"))
            .unwrap()
            .allowed()
    };
    assert!(hit());
    assert!(
        !hit(),
        "a second limiter value with the same name counts on"
    );
    // Another max or window is another limiter.
    assert!(
        null.block_on(RateLimiter::new("x", 2, Duration::from_secs(60)).hit(null.app(), "k"))
            .unwrap()
            .allowed()
    );
}

#[test]
fn the_rate_limiter_fails_closed_and_counts_in_memory_without_a_store() {
    let broken = TestApp::new(|mut b| {
        b.settings_mut().cache_store = "database".to_owned();
        b.settings_mut().database_url = String::new();
        b
    });
    let limiter = RateLimiter::new("x", 3, Duration::from_secs(60));
    assert!(broken.block_on(limiter.hit(broken.app(), "k")).is_err());

    let null = TestApp::new(|mut b| {
        b.settings_mut().cache_store = "null".to_owned();
        b
    });
    let limiter = RateLimiter::new("x", 1, Duration::from_secs(60));
    assert!(
        null.block_on(limiter.hit(null.app(), "k"))
            .unwrap()
            .allowed()
    );
    assert!(
        !null
            .block_on(limiter.hit(null.app(), "k"))
            .unwrap()
            .allowed()
    );
    null.block_on(limiter.clear(null.app(), "k")).unwrap();
    assert!(
        null.block_on(limiter.hit(null.app(), "k"))
            .unwrap()
            .allowed()
    );
    // `max` 0 refuses everything.
    let none = RateLimiter::new("none", 0, Duration::from_secs(60));
    assert!(!null.block_on(none.hit(null.app(), "k")).unwrap().allowed());
}

// ---- encrypt / find_user ----------------------------------------------------------------

#[test]
fn encryption_is_bound_to_purpose_aad_and_key() {
    let app = TestApp::new(|b| b);
    let a = app.app();
    let sealed = a.encrypt("two-factor", b"7", b"secret").unwrap();
    assert!(!sealed.contains("secret"));
    assert_ne!(
        sealed,
        a.encrypt("two-factor", b"7", b"secret").unwrap(),
        "fresh nonce"
    );
    assert_eq!(
        a.decrypt("two-factor", b"7", &sealed).unwrap().unwrap(),
        b"secret"
    );
    assert_eq!(
        a.decrypt("two-factor", b"8", &sealed).unwrap(),
        None,
        "another aad"
    );
    assert_eq!(
        a.decrypt("pubsub", b"7", &sealed).unwrap(),
        None,
        "another purpose"
    );
    assert_eq!(a.decrypt("two-factor", b"7", "garbage").unwrap(), None);
    assert_eq!(a.decrypt("two-factor", b"7", "").unwrap(), None);
    let mut edited = sealed.into_bytes();
    let i = edited.len() / 2;
    edited[i] = if edited[i] == b'A' { b'B' } else { b'A' };
    assert_eq!(
        a.decrypt("two-factor", b"7", &String::from_utf8(edited).unwrap())
            .unwrap(),
        None
    );
    let other = TestApp::new(|mut b| {
        b.settings_mut().key = "another-key-0123456789abcdef0123456".into();
        b
    });
    let sealed = a.encrypt("two-factor", b"7", b"secret").unwrap();
    assert_eq!(
        other.app().decrypt("two-factor", b"7", &sealed).unwrap(),
        None
    );
}

#[test]
fn find_user_returns_an_erased_user() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com");
    let found = app.block_on(app.app().find_user(ada.id)).unwrap().unwrap();
    assert_eq!(found.id(), ada.id);
    assert_eq!(found.downcast::<User>().unwrap().email, "ada@example.com");
    assert_eq!(found.binding("session").unwrap().len(), 64);
    assert_ne!(
        found.binding("session").unwrap(),
        found.binding("hallmark.token").unwrap()
    );
    assert!(found.binding("a|b").is_err());
    assert!(!format!("{found:?}").contains("argon2"));
    assert!(
        app.block_on(app.app().find_user(ada.id + 1))
            .unwrap()
            .is_none()
    );
    let bare = TestApp::new(|b| b);
    assert!(
        bare.block_on(bare.app().find_user(1)).is_err(),
        "no user model"
    );
}

#[test]
fn peek_reads_a_limiter_without_counting() {
    for store in ["array", "null"] {
        let app = TestApp::new(|mut b| {
            b.settings_mut().cache_store = store.to_owned();
            b
        });
        let limiter = RateLimiter::new("peek", 2, Duration::from_secs(60));
        let peek = || app.block_on(limiter.peek(app.app(), "k")).unwrap();
        assert!(
            matches!(peek(), RateLimit::Allowed { remaining: 2, .. }),
            "{store}"
        );
        assert!(
            matches!(peek(), RateLimit::Allowed { remaining: 2, .. }),
            "{store}: no hit counted"
        );
        assert!(app.block_on(limiter.hit(app.app(), "k")).unwrap().allowed());
        assert!(
            matches!(peek(), RateLimit::Allowed { remaining: 1, .. }),
            "{store}"
        );
        assert!(app.block_on(limiter.hit(app.app(), "k")).unwrap().allowed());
        assert!(
            matches!(peek(), RateLimit::Limited { retry_after, .. } if (1..=60).contains(&retry_after)),
            "{store}"
        );
        assert!(matches!(
            app.block_on(limiter.peek(app.app(), "other")).unwrap(),
            RateLimit::Allowed { remaining: 2, .. }
        ));
    }
    let broken = TestApp::new(|mut b| {
        b.settings_mut().cache_store = "database".to_owned();
        b.settings_mut().database_url = String::new();
        b
    });
    let limiter = RateLimiter::new("peek", 2, Duration::from_secs(60));
    assert!(
        broken.block_on(limiter.peek(broken.app(), "k")).is_err(),
        "fails closed"
    );
}

#[test]
fn the_bearer_refusal_is_public_and_the_same_as_the_families() {
    let (app, _) = app();
    let family = get_as(&app, "/api/who", None, false);
    let public = auth::unauthenticated_bearer();
    assert_eq!(public.status().as_u16(), family.status());
    for name in ["www-authenticate", "cache-control", "content-type"] {
        assert_eq!(
            public.headers().get(name).and_then(|v| v.to_str().ok()),
            family.header(name),
            "{name}"
        );
    }
}

// ---- the web stack as a seam (first-party requests on API routes) --------------------------------

/// A stateless guard (bearer: `X-Test-User`) that calls a request first-party when it carries `X-First-Party`.
struct SpaGuard;

impl Guard for SpaGuard {
    fn name(&self) -> &'static str {
        "spa"
    }

    fn stateless(&self) -> bool {
        true
    }

    fn first_party(&self, _app: &App, parts: &Parts) -> bool {
        parts.headers.contains_key("x-first-party")
    }

    fn authenticate<'a>(
        &'a self,
        _app: &'a App,
        parts: &'a mut Parts,
    ) -> BoxFuture<'a, Result<Option<Principal>>> {
        Box::pin(async move {
            let id = parts
                .headers
                .get("x-test-user")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            Ok(id.map(|id| Principal::new(id, "spa", Credential::token(id, ["*"]))))
        })
    }
}

async fn token_of(session: smeltery_core::session::Session) -> String {
    session.token()
}

async fn session_page(session: smeltery_core::session::Session) -> String {
    let visits = session.get::<u32>("visits").unwrap_or(0) + 1;
    session.insert("visits", visits);
    visits.to_string()
}

fn spa_app() -> TestApp {
    TestApp::new(|b| {
        b.migrations(|m: &mut Migrator| {
            m.add(CreateUsers);
        })
        .auth::<User>()
        .guard(SpaGuard)
        .middleware("web-stack", |req: Request, next: Next| async move {
            let app = req.extensions().get::<App>().cloned().expect("the app");
            smeltery_core::session::run_web_stack(app, req, next).await
        })
        .routes(|r| {
            r.get("/token", token_of);
        })
        .api_routes(|r| {
            r.get("/spa", who).middleware("auth:spa");
            r.post("/spa", who).middleware("auth:spa");
            r.get("/visits", session_page).middleware("web-stack");
            r.post("/visits", session_page).middleware("web-stack");
        })
    })
    .with_csrf()
}

fn spa_request(
    app: &TestApp,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> smeltery_core::testing::TestResponse {
    let mut map = HeaderMap::new();
    for (k, v) in headers {
        map.insert(
            http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    app.request(method, path, map, Default::default())
}

#[test]
fn first_party_api_requests_run_the_web_stack_with_its_csrf_check() {
    let app = spa_app();
    let ada = create_user(&app, "ada@example.com");
    app.acting_as(ada.id);
    let token = app.get("/token").text();
    let fp = [("x-first-party", "1")];

    // A first-party read: the session authenticates it.
    let res = spa_request(&app, Method::GET, "/api/spa", &fp);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert!(
        res.text()
            .starts_with(&format!("web {} web:session:", ada.id)),
        "{}",
        res.text()
    );

    // A first-party write without the CSRF token: 419, never the handler.
    let res = spa_request(&app, Method::POST, "/api/spa", &fp);
    assert_eq!(res.status(), 419, "{}", res.text());
    // With it: through.
    let res = spa_request(
        &app,
        Method::POST,
        "/api/spa",
        &[("x-first-party", "1"), ("x-csrf-token", &token)],
    );
    assert_eq!(res.status(), 200, "{}", res.text());

    // A bearer on a first-party write still needs the CSRF token: the check runs before any guard.
    app.clear_cookies();
    let res = spa_request(
        &app,
        Method::POST,
        "/api/spa",
        &[("x-first-party", "1"), ("x-test-user", "9")],
    );
    assert_eq!(res.status(), 419);
    // A first-party read with only a bearer: the guard runs inside the stack.
    let res = spa_request(
        &app,
        Method::GET,
        "/api/spa",
        &[("x-first-party", "1"), ("x-test-user", "9")],
    );
    assert_eq!(res.text(), "spa 9 spa:token:9");
}

#[test]
fn other_api_requests_never_read_the_session() {
    let app = spa_app();
    let ada = create_user(&app, "ada@example.com");
    app.acting_as(ada.id);
    let _ = app.get("/token");
    // Not first-party: the session cookie is ignored; only a bearer counts, and no CSRF check applies to it.
    let res = spa_request(&app, Method::POST, "/api/spa", &[]);
    assert_eq!(res.status(), 401);
    assert_eq!(res.header("www-authenticate"), Some("Bearer"));
    let res = spa_request(&app, Method::POST, "/api/spa", &[("x-test-user", "9")]);
    assert_eq!(res.text(), "spa 9 spa:token:9");
}

#[test]
fn run_web_stack_gives_an_api_route_the_web_session_and_csrf() {
    let app = spa_app();
    assert_eq!(app.get("/api/visits").text(), "1");
    assert_eq!(app.get("/api/visits").text(), "2", "the session is kept");
    let res = spa_request(&app, Method::POST, "/api/visits", &[]);
    assert_eq!(res.status(), 419, "CSRF on state-changing methods");
}

#[test]
fn credential_keys_have_one_format() {
    let principal = Principal::new(1, "hallmark", Credential::token(12, ["*"]));
    assert_eq!(
        principal.key(),
        auth::credential_key("hallmark", "token", "12")
    );
    assert_eq!(
        auth::credential_key("web", "session", "ab"),
        "web:session:ab"
    );
}

struct CacheTables;

impl Migration for CacheTables {
    fn name(&self) -> &'static str {
        "2026_10_05_000002_create_cache_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        smeltery_core::cache::migrations::up(schema).await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        smeltery_core::cache::migrations::down(schema).await
    }
}

/// Review FN2: `peek` on the database store reads the counter that real hits wrote.
#[test]
fn peek_reads_the_database_store_after_real_hits() {
    let app = TestApp::new(|mut b| {
        b.settings_mut().cache_store = "database".to_owned();
        b.migrations(|m: &mut Migrator| {
            m.add(CacheTables);
        })
    });
    assert_eq!(app.app().cache().store_name(), "database");
    let limiter = RateLimiter::new("peek-db", 3, Duration::from_secs(60));
    assert!(matches!(
        app.block_on(limiter.peek(app.app(), "k")).unwrap(),
        RateLimit::Allowed { remaining: 3, .. }
    ));
    for _ in 0..2 {
        assert!(app.block_on(limiter.hit(app.app(), "k")).unwrap().allowed());
    }
    assert!(matches!(
        app.block_on(limiter.peek(app.app(), "k")).unwrap(),
        RateLimit::Allowed { remaining: 1, .. }
    ));
    assert!(app.block_on(limiter.hit(app.app(), "k")).unwrap().allowed());
    assert!(matches!(
        app.block_on(limiter.peek(app.app(), "k")).unwrap(),
        RateLimit::Limited { .. }
    ));
    assert!(
        matches!(
            app.block_on(limiter.peek(app.app(), "k")).unwrap(),
            RateLimit::Limited { .. }
        ),
        "peeking counts nothing"
    );
}
