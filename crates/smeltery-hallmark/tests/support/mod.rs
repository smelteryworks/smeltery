//! The test app shared by the token suites: a `users` table, the tokens table, API and web routes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code
)]

use std::time::Duration;

use serde::Deserialize;
use smeltery_core::auth::{self, Auth, AuthEvent, Authenticated, passwords};
use smeltery_core::db::Record;
use smeltery_core::db::migration::{Migration, Migrator, Schema};
use smeltery_core::db::prelude::Set;
use smeltery_core::http::{Form, StatusCode};
use smeltery_core::pubsub::PubSub;
use smeltery_core::testing::TestApp;
use smeltery_core::{App, AppBuilder, Result};
use smeltery_hallmark::{CurrentToken, Hallmark, HallmarkExt as _, NewToken, Tokens};

pub mod user {
    //! The `User` model (table `users`).
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub email: String,
        pub password: String,
        pub remember_token: Option<String>,
        pub credentials_epoch: Option<i64>,
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
        fn credentials_epoch(&self) -> Option<i64> {
            self.credentials_epoch
        }
    }
}

pub use user::Model as User;

struct CreateTables;

impl Migration for CreateTables {
    fn name(&self) -> &'static str {
        "2026_10_05_000001_create_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("users", |t| {
                t.id();
                t.string("email").unique();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
                t.big_integer("credentials_epoch").nullable();
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
        smeltery_hallmark::migrations::up(schema).await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        smeltery_hallmark::migrations::down(schema).await?;
        schema.drop_if_exists("password_reset_tokens").await?;
        schema.drop_if_exists("users").await
    }
}

async fn me(who: Authenticated, app: App) -> Result<String> {
    let user: User = who.user(&app).await?.unwrap();
    Ok(format!("{} {}", who.key(), user.email))
}

async fn key(who: Authenticated) -> String {
    who.key()
}

async fn maybe(token: Option<CurrentToken>) -> String {
    token.map_or_else(|| "none".to_owned(), |t| format!("{} {}", t.id(), t.name()))
}

async fn twice(who: Option<Authenticated>, token: Option<CurrentToken>) -> String {
    format!(
        "{} {}",
        who.map_or_else(|| "none".to_owned(), |w| w.key()),
        token.map_or_else(|| "none".to_owned(), |t| t.id().to_string())
    )
}

#[derive(Deserialize)]
struct TokenRequest {
    email: String,
    password: String,
    device_name: String,
    code: Option<String>,
}

async fn issue(
    app: App,
    client: smeltery_core::http::ClientInfo,
    smeltery_core::http::Json(form): smeltery_core::http::Json<TokenRequest>,
) -> Result<smeltery_core::Response> {
    let issued = smeltery_hallmark::issue_for_credentials(
        &app,
        &client,
        &form.email,
        &form.password,
        &form.device_name,
        form.code.as_deref(),
        &["*"],
    )
    .await?;
    Ok(issued.created())
}

async fn ok() -> &'static str {
    "ok"
}

async fn destroy(token: CurrentToken) -> Result<StatusCode> {
    token.revoke().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What an "update password" endpoint does: store a new hash, then tell the app's credentials.
async fn changed(who: Authenticated, app: App) -> Result<&'static str> {
    let hash = auth::hash_password("a brand new secret").await?;
    app.db()?
        .execute_with(
            "UPDATE users SET password = ? WHERE id = ?",
            [hash.into(), who.user_id.into()],
        )
        .await?;
    auth::password_changed(&app, who.user_id, Some(&who)).await?;
    Ok("changed")
}

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
}

async fn login(auth: Auth, Form(f): Form<Login>) -> Result<&'static str> {
    Ok(if auth.attempt(&f.email, &f.password, false).await? {
        "in"
    } else {
        "out"
    })
}

#[derive(Deserialize)]
struct Password {
    password: String,
}

async fn logout_others(auth: Auth, Form(f): Form<Password>) -> Result<String> {
    Ok(auth.logout_other_devices(&f.password).await?.to_string())
}

/// The app with `hallmark`'s settings.
pub fn build(hallmark: Hallmark) -> impl FnOnce(AppBuilder) -> AppBuilder {
    move |b: AppBuilder| {
        b.migrations(|m: &mut Migrator| {
            m.add(CreateTables);
        })
        .auth::<User>()
        .hallmark(hallmark)
        .api_routes(|r| {
            r.get("/me", me).middleware("auth:hallmark");
            r.get("/plain", key);
            r.get("/maybe", maybe);
            r.get("/twice", twice);
            r.post("/stateful", key).middleware("auth:hallmark");
            r.post("/tokens", issue);
            r.delete("/tokens/mine", smeltery_hallmark::revoke_current)
                .middleware("auth:hallmark");
            r.get("/orders", ok)
                .middleware("auth:hallmark")
                .middleware("abilities:orders:read");
            r.get("/both", ok)
                .middleware("auth:hallmark")
                .middleware("abilities:orders:read,orders:write");
            r.get("/either", ok)
                .middleware("auth:hallmark")
                .middleware("ability:orders:read,orders:write");
            r.get("/naked", ok).middleware("abilities:orders:read");
            r.delete("/tokens/current", destroy)
                .middleware("auth:hallmark");
            r.post("/password-changed", changed)
                .middleware("auth:hallmark");
        })
        .routes(|r| {
            r.get("/login", ok).name("login");
            r.post("/login", login);
            r.post("/logout-others", logout_others);
            r.get("/web/me", key).middleware("auth:hallmark");
            r.post("/web/post", ok).middleware("auth:hallmark");
            r.get("/web/abilities", ok)
                .middleware("auth:hallmark")
                .middleware("abilities:orders:read");
        })
    }
}

pub fn app() -> TestApp {
    TestApp::new(build(Hallmark::new()))
}

pub fn app_with(hallmark: Hallmark) -> TestApp {
    TestApp::new(build(hallmark))
}

pub fn create_user(app: &TestApp, email: &str, password: &str) -> User {
    let db = app.db();
    app.block_on(async {
        User::create(
            &db,
            user::ActiveModel {
                email: Set(email.into()),
                password: Set(auth::hash_password(password).await.unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    })
}

pub fn tokens(app: &TestApp) -> Tokens {
    Tokens::of(app.app()).unwrap()
}

pub fn create(app: &TestApp, user: &User, abilities: &[&str]) -> NewToken {
    app.block_on(tokens(app).create(user.id, "phone", abilities, None))
        .unwrap()
}

/// A GET with exactly this `Authorization` header (the app's sticky headers aside).
pub fn get_with(
    app: &TestApp,
    path: &str,
    authorization: &str,
) -> smeltery_core::testing::TestResponse {
    app.with_header("authorization", authorization);
    let res = app.get_json(path);
    app.without_header("authorization");
    res
}

/// Every `auth` event published while `f` runs.
pub fn events(app: &TestApp, f: impl FnOnce()) -> Vec<AuthEvent> {
    let mut sub = PubSub::of(app.app()).unwrap().subscribe(auth::EVENTS_TOPIC);
    f();
    let mut out = Vec::new();
    while let Ok(Ok(message)) =
        app.block_on(async { tokio::time::timeout(Duration::from_millis(20), sub.recv()).await })
    {
        out.push(serde_json::from_value(message.payload.clone()).unwrap());
    }
    out
}

/// Run the app's background tasks until `done` holds (the `last_used_at` writes), at most five seconds.
pub fn wait_until(app: &TestApp, mut done: impl FnMut(&TestApp) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !done(app) {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the background work"
        );
        app.block_on(tokio::task::yield_now());
    }
}

/// When token `id` of `user` was last used.
pub fn last_used(app: &TestApp, user: &User, id: i64) -> Option<sea_orm::prelude::DateTimeUtc> {
    app.block_on(tokens(app).find(user.id, id))
        .unwrap()
        .unwrap()
        .last_used_at
}

/// Run one SQL statement with values (SQLite placeholders).
pub fn sql(app: &TestApp, statement: &str, values: Vec<sea_orm::Value>) -> u64 {
    let db = app.db();
    app.block_on(db.execute_with(statement, values)).unwrap()
}

/// How many token rows there are.
pub fn token_rows(app: &TestApp) -> i64 {
    let db = app.db();
    let rows = app
        .block_on(db.query_with("SELECT COUNT(*) AS n FROM personal_access_tokens", []))
        .unwrap();
    rows[0].try_get("", "n").unwrap()
}

/// A reset link's token for `user`, then the reset.
pub fn reset_password(app: &TestApp, user: &User, new_password: &str) {
    let db = app.db();
    let token = app.block_on(passwords::create_token(&db, user.id)).unwrap();
    let changed = app
        .block_on(passwords::reset(
            app.app(),
            &user.email,
            &token,
            new_password,
        ))
        .unwrap();
    assert_eq!(changed, Some(user.id));
}
