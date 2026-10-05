//! The test app: a `users` table, the app's actions and plain-text views, and notifiers that keep the links.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use smeltery::Validate;
use smeltery_core::auth::passwords::ResetNotifier;
use smeltery_core::auth::verification::VerificationNotifier;
use smeltery_core::auth::{Auth, hash_password};
use smeltery_core::db::Record;
use smeltery_core::db::migration::{Migration, Migrator, Schema};
use smeltery_core::db::prelude::Set;
use smeltery_core::session::Session;
use smeltery_core::testing::{TestApp, TestResponse};
use smeltery_core::{App, AppBuilder, BoxFuture, Result};
use smeltery_temper::testing::EventRecorder;
use smeltery_temper::{
    CreatesNewUsers, EmailChanged, PasswordInput, ResetsUserPasswords, Temper, TemperCtx,
    TemperExt as _, TemperViews, UpdatePasswordInput, UpdatesUserPasswords,
    UpdatesUserProfileInformation, ViewCtx,
};

pub mod user {
    //! The user model.
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub email: String,
        pub email_verified_at: Option<DateTimeUtc>,
        pub password: String,
        pub remember_token: Option<String>,
        pub credentials_epoch: Option<i64>,
        pub created_at: Option<DateTimeUtc>,
        pub updated_at: Option<DateTimeUtc>,
        #[serde(skip_serializing)]
        pub two_factor_secret: Option<String>,
        #[serde(skip_serializing)]
        pub two_factor_recovery_codes: Option<String>,
        pub two_factor_confirmed_at: Option<DateTimeUtc>,
        pub two_factor_last_step: Option<i64>,
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

    impl smeltery_temper::TwoFactorAuthenticatable for Model {
        fn two_factor_secret(&self) -> Option<&str> {
            self.two_factor_secret.as_deref()
        }
        fn two_factor_recovery_codes(&self) -> Option<&str> {
            self.two_factor_recovery_codes.as_deref()
        }
        fn two_factor_confirmed_at(&self) -> Option<DateTimeUtc> {
            self.two_factor_confirmed_at
        }
        fn two_factor_last_step(&self) -> Option<i64> {
            self.two_factor_last_step
        }
        fn two_factor_account(&self) -> String {
            self.email.clone()
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
                t.string("name");
                t.string("email").unique();
                t.datetime("email_verified_at").nullable();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
                t.big_integer("credentials_epoch").nullable();
                t.timestamps();
                t.text("two_factor_secret").nullable();
                t.text("two_factor_recovery_codes").nullable();
                t.datetime("two_factor_confirmed_at").nullable();
                t.big_integer("two_factor_last_step").nullable();
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
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("password_reset_tokens").await?;
        schema.drop_if_exists("users").await
    }
}

pub fn migrations(m: &mut Migrator) {
    m.add(CreateTables);
}

// ---- actions ------------------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, Validate)]
pub struct RegisterForm {
    #[validate(required, max = 255)]
    pub name: String,
    #[validate(required, email, max = 255, unique(table = "users", column = "email"))]
    #[serde(deserialize_with = "smeltery_core::auth::deserialize_email")]
    pub email: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: Option<String>,
}

pub struct CreateNewUser;

impl CreatesNewUsers<User> for CreateNewUser {
    type Input = RegisterForm;

    async fn create(&self, ctx: &TemperCtx, input: RegisterForm) -> Result<User> {
        User::create(
            &ctx.db()?,
            user::ActiveModel {
                name: Set(input.name),
                email: Set(input.email),
                password: Set(hash_password(&input.password).await?),
                ..Default::default()
            },
        )
        .await
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct ResetForm {
    #[validate(required, email)]
    #[serde(deserialize_with = "smeltery_core::auth::deserialize_email")]
    pub email: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: Option<String>,
}

impl PasswordInput for ResetForm {
    fn email(&self) -> &str {
        &self.email
    }
    fn password(&self) -> &str {
        &self.password
    }
}

/// Counts its calls (the users it was called for).
#[derive(Clone, Default)]
pub struct ResetUserPassword {
    pub calls: Arc<Mutex<Vec<i64>>>,
}

impl ResetsUserPasswords<User> for ResetUserPassword {
    type Input = ResetForm;

    async fn reset(&self, _ctx: &TemperCtx, user: &User, input: &ResetForm) -> Result<()> {
        assert!(!input.password.is_empty());
        self.calls.lock().unwrap().push(user.id);
        Ok(())
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct PasswordForm {
    #[validate(required)]
    pub current_password: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: Option<String>,
}

impl UpdatePasswordInput for PasswordForm {
    fn current_password(&self) -> &str {
        &self.current_password
    }
    fn password(&self) -> &str {
        &self.password
    }
}

#[derive(Clone, Default)]
pub struct UpdateUserPassword {
    pub calls: Arc<AtomicUsize>,
}

impl UpdatesUserPasswords<User> for UpdateUserPassword {
    type Input = PasswordForm;

    async fn update(&self, _ctx: &TemperCtx, _user: &User, _input: &PasswordForm) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct ProfileForm {
    #[validate(required, max = 255)]
    pub name: String,
    #[validate(required, email, max = 255)]
    #[serde(deserialize_with = "smeltery_core::auth::deserialize_email")]
    pub email: String,
}

pub struct UpdateProfile;

impl UpdatesUserProfileInformation<User> for UpdateProfile {
    type Input = ProfileForm;

    async fn update(
        &self,
        ctx: &TemperCtx,
        user: &User,
        input: ProfileForm,
    ) -> Result<EmailChanged> {
        let changed = input.email != user.email;
        user.update(&ctx.db()?, |m| {
            m.name = Set(input.name);
            m.email = Set(input.email);
        })
        .await?;
        Ok(if changed {
            EmailChanged::Yes
        } else {
            EmailChanged::No
        })
    }
}

// ---- views --------------------------------------------------------------------------------------------------------

/// A page as plain text: its name, the flashed errors, status and error, and the old `email` / `code` input.
fn show(name: &str, ctx: &ViewCtx) -> String {
    let session = ctx.session();
    format!(
        "page={name}|errors={}|status={}|error={}|old_email={}|old_code={}|token={}|email={}",
        serde_json::to_string(&session.errors()).unwrap(),
        ctx.status().unwrap_or_default(),
        session.get::<String>("error").unwrap_or_default(),
        session.old("email").unwrap_or_default(),
        session.old("code").unwrap_or_default(),
        ctx.token(),
        ctx.email(),
    )
}

pub fn views() -> TemperViews {
    TemperViews::new()
        .login(|ctx| show("login", &ctx))
        .register(|ctx| show("register", &ctx))
        .forgot_password(|ctx| show("forgot-password", &ctx))
        .reset_password(|ctx| show("reset-password", &ctx))
        .verify_email(|ctx| show("verify-email", &ctx))
        .confirm_password(|ctx| show("confirm-password", &ctx))
        .two_factor_challenge(|ctx| show("two-factor-challenge", &ctx))
}

// ---- mail ---------------------------------------------------------------------------------------------------------

/// The links the app sent: `(address, url)`.
#[derive(Clone, Default)]
pub struct Outbox {
    pub resets: Arc<Mutex<Vec<(String, String)>>>,
    pub verifications: Arc<Mutex<Vec<(String, String)>>>,
}

impl Outbox {
    pub fn resets(&self) -> Vec<(String, String)> {
        self.resets.lock().unwrap().clone()
    }

    pub fn verifications(&self) -> Vec<(String, String)> {
        self.verifications.lock().unwrap().clone()
    }

    /// The path (and query) of the last reset link.
    pub fn last_reset_path(&self) -> String {
        path_of(&self.resets().last().expect("a reset link").1)
    }

    /// The path (and query) of the last verification link.
    pub fn last_verification_path(&self) -> String {
        path_of(&self.verifications().last().expect("a verification link").1)
    }
}

/// `http://host/path?query` → `/path?query`.
pub fn path_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.find('/')
        .map_or("/".to_owned(), |at| rest[at..].to_owned())
}

struct Resets(Outbox);

impl ResetNotifier for Resets {
    fn send<'a>(
        &'a self,
        _app: &'a App,
        email: &'a str,
        url: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        self.0
            .resets
            .lock()
            .unwrap()
            .push((email.to_owned(), url.to_owned()));
        Box::pin(async { Ok(()) })
    }
}

struct Verifications(Outbox);

impl VerificationNotifier for Verifications {
    fn send<'a>(
        &'a self,
        _app: &'a App,
        email: &'a str,
        url: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        self.0
            .verifications
            .lock()
            .unwrap()
            .push((email.to_owned(), url.to_owned()));
        Box::pin(async { Ok(()) })
    }
}

// ---- the app ------------------------------------------------------------------------------------------------------

/// The app under test with what a test inspects.
pub struct Harness {
    pub app: TestApp,
    pub outbox: Outbox,
    pub events: EventRecorder,
    pub resets: ResetUserPassword,
    pub password_updates: UpdateUserPassword,
}

/// Every feature on, the plain-text views and the event recorder.
pub fn full_temper(
    events: &EventRecorder,
    resets: &ResetUserPassword,
    updates: &UpdateUserPassword,
) -> Temper<User> {
    Temper::<User>::new()
        .registration(CreateNewUser)
        .reset_passwords(resets.clone())
        .email_verification()
        .update_profile_information(UpdateProfile)
        .update_passwords(updates.clone())
        .views(views())
        .listen(events.listener())
}

/// The app's own routes around Temper: `home`, a dashboard behind `auth` + `verified`, a page behind
/// `password.confirm`, and helpers that show the session.
pub fn base(app: AppBuilder, outbox: &Outbox) -> AppBuilder {
    let resets: Arc<dyn ResetNotifier> = Arc::new(Resets(outbox.clone()));
    let verifications: Arc<dyn VerificationNotifier> = Arc::new(Verifications(outbox.clone()));
    app.migrations(migrations)
        .service(resets)
        .service(verifications)
        .routes(|r| {
            r.get("/", || async { "home" }).name("home");
            r.get("/dashboard", |auth: Auth| async move {
                format!("dashboard of {}", auth.id().unwrap_or_default())
            })
            .name("dashboard")
            .middleware("auth")
            .middleware("verified");
            r.get("/secret", || async { "secret" })
                .name("secret")
                .middleware("auth")
                .middleware("password.confirm");
            r.get("/settings-2fa", |auth: Auth, session: Session| async move {
                let status = smeltery_temper::two_factor::status::<User>(&auth, &session)
                    .await?
                    .unwrap_or_default();
                Ok::<_, smeltery_core::Error>(format!(
                    "enabled={}|confirmed={}|left={}|codes={}",
                    status.enabled,
                    status.confirmed,
                    status.recovery_codes_left,
                    status
                        .new_recovery_codes
                        .map_or_else(|| "none".to_owned(), |c| c.len().to_string())
                ))
            })
            .middleware("auth");
            r.get("/_session", |session: Session| async move {
                format!("{}|{}", session.id(), session.token())
            });
        })
}

/// The full app, `configure` applied to Temper and `extra` to the builder (after Temper).
pub fn harness_with(
    configure: impl FnOnce(Temper<User>) -> Temper<User>,
    extra: impl FnOnce(AppBuilder) -> AppBuilder,
) -> Harness {
    let outbox = Outbox::default();
    let events = EventRecorder::new();
    let resets = ResetUserPassword::default();
    let password_updates = UpdateUserPassword::default();
    let temper = configure(full_temper(&events, &resets, &password_updates));
    let mail = outbox.clone();
    let app = TestApp::new(move |app| extra(base(app, &mail).temper(temper)));
    Harness {
        app,
        outbox,
        events,
        resets,
        password_updates,
    }
}

/// The full app.
pub fn harness() -> Harness {
    harness_with(|t| t, |b| b)
}

/// The full app requiring verified addresses (core's `.verify_email::<User>()`).
pub fn harness_verifying() -> Harness {
    harness_with(|t| t, |b| b.verify_email::<User>())
}

/// A user with this address and password (not verified); its id.
pub fn create_user(app: &TestApp, email: &str, password: &str) -> i64 {
    let db = app.db();
    app.block_on(async {
        User::create(
            &db,
            user::ActiveModel {
                name: Set("Ada Lovelace".into()),
                email: Set(email.into()),
                password: Set(hash_password(password).await.unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    })
}

/// The user with this id.
pub fn user(app: &TestApp, id: i64) -> User {
    let db = app.db();
    app.block_on(async { User::find(&db, id).await.unwrap().expect("the user") })
}

pub fn register(app: &TestApp, email: &str) -> TestResponse {
    app.post_form(
        "/register",
        &[
            ("name", "Ada Lovelace"),
            ("email", email),
            ("password", "analytical-engine"),
            ("password_confirmation", "analytical-engine"),
        ],
    )
}

/// The session cookie's name in the test app.
pub fn session_cookie(app: &TestApp) -> String {
    app.app().settings().session_cookie.clone()
}

/// Accept JSON.
pub fn json_headers() -> smeltery_core::http::HeaderMap {
    let mut headers = smeltery_core::http::HeaderMap::new();
    headers.insert(
        smeltery_core::http::header::ACCEPT,
        smeltery_core::http::HeaderValue::from_static("application/json"),
    );
    headers
}

/// A JSON request from a JSON client (`Accept` and `Content-Type: application/json`).
pub fn json_request(
    app: &TestApp,
    method: smeltery_core::http::Method,
    path: &str,
    body: &serde_json::Value,
) -> TestResponse {
    let mut headers = json_headers();
    headers.insert(
        smeltery_core::http::header::CONTENT_TYPE,
        smeltery_core::http::HeaderValue::from_static("application/json"),
    );
    app.request(method, path, headers, body.to_string().into())
}

/// A form with any method (`PUT`, `DELETE`).
pub fn form_request(
    app: &TestApp,
    method: smeltery_core::http::Method,
    path: &str,
    fields: &[(&str, &str)],
) -> TestResponse {
    let mut headers = smeltery_core::http::HeaderMap::new();
    headers.insert(
        smeltery_core::http::header::CONTENT_TYPE,
        smeltery_core::http::HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    let body = serde_urlencoded_body(fields);
    app.request(method, path, headers, body.into())
}

fn serde_urlencoded_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
