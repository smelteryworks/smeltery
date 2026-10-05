//! Credentials without a session, password changes and what they end, credential listeners, auth events on
//! PubSub, password resets through the user model, password confirmation and the keys a sign-in owns.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::any::TypeId;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use smeltery_core::auth::{
    self, Auth, AuthEvent, AuthUser, Authenticated, CredentialChange, CredentialKind,
    CredentialListener, CredentialsChanged, LoginDecision, LoginPolicy, SecondFactor,
    SecondFactorVerdict, passwords,
};
use smeltery_core::db::Record;
use smeltery_core::db::migration::{Migration, Migrator, Schema};
use smeltery_core::db::prelude::Set;
use smeltery_core::http::{Form, HeaderMap, HeaderValue, Method, header};
use smeltery_core::pubsub::PubSub;
use smeltery_core::session::Session;
use smeltery_core::testing::{TestApp, TestResponse};
use smeltery_core::{App, AppBuilder, BoxFuture, Error, Result};

mod user {
    //! The `User` model (table `users`).
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "members")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub email: String,
        pub email_verified_at: Option<DateTimeUtc>,
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

struct CreateTables;

impl Migration for CreateTables {
    fn name(&self) -> &'static str {
        "2026_10_05_000001_create_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        // A table name other than `users`: resets go through the model, never a literal table.
        schema
            .create("members", |t| {
                t.id();
                t.string("email").unique();
                t.datetime("email_verified_at").nullable();
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
                    .constrained("members")
                    .cascade_on_delete();
                t.string("token");
                t.datetime("created_at").nullable();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("password_reset_tokens").await?;
        schema.drop_if_exists("members").await
    }
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

async fn me(who: Authenticated) -> String {
    who.key()
}

async fn logout(auth: Auth) -> Result<&'static str> {
    auth.logout().await?;
    Ok("bye")
}

#[derive(Deserialize)]
struct Password {
    password: String,
}

async fn logout_others(auth: Auth, Form(f): Form<Password>) -> Result<String> {
    Ok(auth.logout_other_devices(&f.password).await?.to_string())
}

async fn set_password(auth: Auth, Form(f): Form<Password>) -> Result<String> {
    Ok(auth.set_password(&f.password).await?.to_string())
}

async fn confirm(auth: Auth, Form(f): Form<Password>) -> Result<String> {
    Ok(auth.confirm_password(&f.password).await?.to_string())
}

async fn secret() -> &'static str {
    "secret"
}

async fn stash(session: Session) -> &'static str {
    session.insert("_temper.login", 7);
    session.insert("_auth.other", true);
    session.insert("kept", "yes");
    "stashed"
}

/// An API route (no web stack): who the session cookie says the visitor is, read with `session::peek` and
/// `Auth::peek`.
async fn peeked(app: App, headers: HeaderMap) -> Result<String> {
    let Some(session) = smeltery_core::session::peek(&app, &headers).await? else {
        return Ok("none".to_owned());
    };
    let binding = session.binding();
    Ok(match Auth::peek(&app, session, "unknown").await?.id() {
        Some(id) => format!("{id} {binding}"),
        None => "guest".to_owned(),
    })
}

async fn stashed(session: Session) -> String {
    format!(
        "{} {} {}",
        session.has("_temper.login"),
        session.has("_auth.other"),
        session.has("kept")
    )
}

/// Records every change it is told about; fails when `fail` is set.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<CredentialsChanged>>,
    fail: bool,
}

struct RecorderHandle(Arc<Recorder>);

impl CredentialListener for RecorderHandle {
    fn credentials_changed<'a>(
        &'a self,
        _app: &'a App,
        change: &'a CredentialsChanged,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.0.seen.lock().unwrap().push(change.clone());
            if self.0.fail {
                return Err(Error::internal("listener failed"));
            }
            Ok(())
        })
    }
}

fn build(recorder: Arc<Recorder>) -> impl FnOnce(AppBuilder) -> AppBuilder {
    move |b: AppBuilder| {
        b.migrations(|m: &mut Migrator| {
            m.add(CreateTables);
        })
        .auth::<User>()
        .credential_listener(RecorderHandle(recorder))
        .routes(|r| {
            r.get("/login", login_page).name("login");
            r.post("/login", login);
            r.get("/me", me).middleware("auth");
            r.post("/logout", logout);
            r.post("/logout-others", logout_others);
            r.post("/password", set_password);
            r.post("/confirm", confirm);
            r.get("/secret", secret)
                .middleware("auth")
                .middleware("password.confirm");
            r.post("/secret", secret)
                .middleware("auth")
                .middleware("password.confirm");
            r.post("/stash", stash);
            r.get("/stash", stashed);
        })
        .api_routes_at("/raw", |r| {
            r.get("/peek", peeked);
        })
    }
}

fn app() -> (TestApp, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    (TestApp::new(build(Arc::clone(&recorder))), recorder)
}

/// An app whose budgets count in memory: their windows start with the first hit, so a budget test never straddles
/// a clock-aligned window boundary (the cache stores count per calendar minute).
fn memory_budget_app() -> TestApp {
    TestApp::new(|b| {
        let mut b = build(Arc::new(Recorder::default()))(b);
        b.settings_mut().cache_store = "null".to_owned();
        b
    })
}

fn create_user(app: &TestApp, email: &str, password: &str) -> User {
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

fn form(app: &TestApp, path: &str, fields: &[(&str, &str)]) -> TestResponse {
    app.post_form(path, fields)
}

fn sign_in(app: &TestApp, email: &str, password: &str) {
    assert_eq!(
        form(app, "/login", &[("email", email), ("password", password)]).text(),
        "in"
    );
}

fn session_cookie(app: &TestApp) -> String {
    app.app().settings().session_cookie.clone()
}

fn remember_cookie(app: &TestApp) -> String {
    format!("remember_{}", session_cookie(app))
}

/// Every `auth` event published while `f` runs.
fn events(app: &TestApp, f: impl FnOnce()) -> Vec<AuthEvent> {
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

// ---- verify_credentials / validate --------------------------------------------------------

#[test]
fn verify_credentials_checks_without_a_session_and_shares_the_login_budgets() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com", "correct horse");
    let found = app
        .block_on(auth::verify_credentials(
            app.app(),
            "192.0.2.7",
            " ADA@example.com",
            "correct horse",
        ))
        .unwrap()
        .unwrap();
    assert_eq!(found.id(), ada.id);
    assert!(
        app.block_on(auth::verify_credentials(
            app.app(),
            "192.0.2.7",
            "ada@example.com",
            "wrong"
        ))
        .unwrap()
        .is_none()
    );
    // Five a minute per address and client, for existing and unknown addresses alike; the sixth is refused
    // before any check.
    for email in ["ada@example.com", "nobody@example.com"] {
        let client = if email.starts_with("ada") {
            "198.51.100.1"
        } else {
            "198.51.100.2"
        };
        for _ in 0..5 {
            assert!(
                app.block_on(auth::verify_credentials(app.app(), client, email, "wrong"))
                    .unwrap()
                    .is_none()
            );
        }
        let err = app
            .block_on(auth::verify_credentials(
                app.app(),
                client,
                email,
                "correct horse",
            ))
            .unwrap_err();
        assert_eq!(err.status(), 429, "{email}");
    }
    // The same budget as `Auth::attempt`: the web login from that client is refused too.
    app.from_addr("198.51.100.1:5000".parse().unwrap());
    let res = form(
        &app,
        "/login",
        &[("email", "ada@example.com"), ("password", "correct horse")],
    );
    assert_eq!(res.status(), 303, "refused with the budget's message");
    assert_ne!(res.text(), "in");
}

// ---- set_password ---------------------------------------------------------------------------

#[test]
fn set_password_keeps_this_session_and_ends_every_other_credential() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "old password");
    // Device B with a remember-me cookie.
    let res = form(
        &app,
        "/login",
        &[
            ("email", "ada@example.com"),
            ("password", "old password"),
            ("remember", "on"),
        ],
    );
    assert_eq!(res.text(), "in");
    let b_session = app.cookie(&session_cookie(&app)).unwrap();
    let b_remember = app.cookie(&remember_cookie(&app)).unwrap();
    app.clear_cookies();
    // Device A changes the password.
    sign_in(&app, "ada@example.com", "old password");
    let key = app.get("/me").text();
    let published = events(&app, || {
        assert_eq!(
            form(&app, "/password", &[("password", "new password")]).text(),
            "true"
        );
    });
    assert_eq!(app.get("/me").status(), 200, "this device stays signed in");
    assert_eq!(app.get("/me").text(), key, "the same session");
    assert_eq!(
        published,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Every,
            except: Some(key.clone()),
        }],
        "published once"
    );
    let seen = recorder.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].user_id, seen[0].why, seen[0].except.clone()),
        (ada.id, CredentialChange::Changed, Some(key))
    );
    // B's session and B's remember-me cookie no longer sign in.
    app.clear_cookies();
    app.set_cookie(&session_cookie(&app), &b_session);
    assert_eq!(app.get("/me").status(), 303);
    app.clear_cookies();
    app.set_cookie(&remember_cookie(&app), &b_remember);
    assert_eq!(app.get("/me").status(), 303);
    app.clear_cookies();
    sign_in(&app, "ada@example.com", "new password");
    // A guest changes nothing.
    app.clear_cookies();
    assert_eq!(
        form(&app, "/password", &[("password", "x")]).text(),
        "false"
    );
}

// ---- logout_other_devices ---------------------------------------------------------------------

/// Before the fix a remember-me cookie of another device signed it in again after `logout_other_devices`: the
/// remember token was not replaced.
#[test]
fn logout_other_devices_also_ends_remember_me_cookies() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "correct horse");
    let res = form(
        &app,
        "/login",
        &[
            ("email", "ada@example.com"),
            ("password", "correct horse"),
            ("remember", "on"),
        ],
    );
    assert_eq!(res.text(), "in");
    let b_remember = app.cookie(&remember_cookie(&app)).unwrap();
    app.clear_cookies();
    sign_in(&app, "ada@example.com", "correct horse");
    let key = app.get("/me").text();
    let published = events(&app, || {
        assert_eq!(
            form(&app, "/logout-others", &[("password", "correct horse")]).text(),
            "true"
        );
    });
    assert_eq!(app.get("/me").status(), 200, "this device stays");
    assert_eq!(
        published,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Every,
            except: Some(key),
        }]
    );
    assert_eq!(
        recorder.seen.lock().unwrap()[0].why,
        CredentialChange::OtherDevicesLoggedOut
    );
    app.clear_cookies();
    app.set_cookie(&remember_cookie(&app), &b_remember);
    assert_eq!(
        app.get("/me").status(),
        303,
        "the other device's remember-me cookie is dead"
    );
    // A wrong password publishes nothing.
    app.clear_cookies();
    sign_in(&app, "ada@example.com", "correct horse");
    let published = events(&app, || {
        assert_eq!(
            form(&app, "/logout-others", &[("password", "nope")]).text(),
            "false"
        );
    });
    assert!(published.is_empty());
}

// ---- logout ---------------------------------------------------------------------------------

#[test]
fn logout_publishes_the_session_it_ended_and_clears_its_keys() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    let key = app.get("/me").text();
    assert!(key.starts_with("web:session:"));
    form(&app, "/stash", &[]);
    let published = events(&app, || {
        assert_eq!(form(&app, "/logout", &[]).text(), "bye");
    });
    assert_eq!(
        published,
        [AuthEvent::Revoked {
            user_id: ada.id,
            key
        }]
    );
    assert!(
        recorder.seen.lock().unwrap().is_empty(),
        "no credential changed"
    );
    assert_eq!(app.get("/stash").text(), "false false false");
    // A guest's logout publishes nothing.
    assert!(
        events(&app, || {
            form(&app, "/logout", &[]);
        })
        .is_empty()
    );
}

// ---- peek -------------------------------------------------------------------------------------

#[test]
fn peek_reads_the_sign_in_without_storing_anything() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com", "correct horse");
    let peek = |app: &TestApp| {
        let res = app.get("/raw/peek");
        assert_eq!(res.status(), 200);
        assert!(res.header("set-cookie").is_none(), "peek sets no cookie");
        res.text()
    };
    assert_eq!(peek(&app), "none", "no session cookie");
    sign_in(&app, "ada@example.com", "correct horse");
    let key = app.get("/me").text();
    let binding = key.strip_prefix("web:session:").unwrap();
    assert_eq!(peek(&app), format!("{} {binding}", ada.id));
    // After the logout the browser's session is another one, signed out.
    assert_eq!(form(&app, "/logout", &[]).text(), "bye");
    let after = peek(&app);
    assert!(after == "guest" || after == "none", "{after}");
    // A tampered cookie is no session.
    app.set_cookie(&session_cookie(&app), "garbage");
    assert_eq!(peek(&app), "none");
}

/// Review I-2: `peek` leaves the session store as it was (the file driver: content and modification time, which is
/// the session's last activity) and respects the idle lifetime without deleting anything.
#[test]
fn peek_leaves_the_session_store_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let app = TestApp::new(move |b| {
        let mut b = build(Arc::new(Recorder::default()))(b);
        b.settings_mut().session_driver = "file".to_owned();
        b.settings_mut().root = root;
        b
    });
    let ada = create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    let sessions = dir
        .path()
        .join("storage")
        .join("framework")
        .join("sessions");
    let files: Vec<std::path::PathBuf> = std::fs::read_dir(&sessions)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let file = &files[0];
    let set_age = |age: Duration| {
        let handle = std::fs::File::options().write(true).open(file).unwrap();
        handle
            .set_modified(std::time::SystemTime::now() - age)
            .unwrap();
        std::fs::metadata(file).unwrap().modified().unwrap()
    };
    let stamp = set_age(Duration::from_secs(60));
    let content = std::fs::read(file).unwrap();
    let res = app.get("/raw/peek");
    assert!(
        res.text().starts_with(&format!("{} ", ada.id)),
        "{}",
        res.text()
    );
    assert!(res.header("set-cookie").is_none());
    assert_eq!(
        std::fs::read(file).unwrap(),
        content,
        "the payload is unchanged"
    );
    assert_eq!(
        std::fs::metadata(file).unwrap().modified().unwrap(),
        stamp,
        "the last activity is unchanged"
    );
    assert_eq!(
        std::fs::read_dir(&sessions).unwrap().count(),
        1,
        "no new session file"
    );
    // Idle past the lifetime: no session, and nothing is deleted or written.
    let lifetime = app.app().settings().session_lifetime;
    let stamp = set_age(lifetime + Duration::from_secs(60));
    assert_eq!(app.get("/raw/peek").text(), "none");
    assert_eq!(std::fs::metadata(file).unwrap().modified().unwrap(), stamp);
    assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), 1);
}

#[test]
fn peek_sees_a_password_change_on_another_device() {
    let (app, _) = app();
    create_user(&app, "ada@example.com", "old password");
    sign_in(&app, "ada@example.com", "old password");
    let b = app.cookie(&session_cookie(&app)).unwrap();
    app.clear_cookies();
    sign_in(&app, "ada@example.com", "old password");
    assert_eq!(
        form(&app, "/password", &[("password", "new password")]).text(),
        "true"
    );
    app.clear_cookies();
    app.set_cookie(&session_cookie(&app), &b);
    assert_eq!(app.get("/raw/peek").text(), "guest");
}

// ---- reserved keys --------------------------------------------------------------------------

#[test]
fn a_sign_in_removes_the_keys_of_the_last_one() {
    let (app, _) = app();
    create_user(&app, "ada@example.com", "correct horse");
    form(&app, "/stash", &[]);
    assert_eq!(app.get("/stash").text(), "true true true");
    sign_in(&app, "ada@example.com", "correct horse");
    assert_eq!(app.get("/stash").text(), "false false true");
}

// ---- password confirmation -------------------------------------------------------------------

#[test]
fn password_confirmation_guards_routes_and_is_budgeted() {
    let app = memory_budget_app();
    create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    // Not confirmed: a page visit is remembered and sent to the confirmation page; JSON gets 423.
    let res = app.get("/secret?tab=1");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/user/confirm-password"));
    let res = app.get_json("/secret");
    assert_eq!(res.status(), 423);
    assert_eq!(res.json()["message"], "Password confirmation required.");
    assert_eq!(form(&app, "/secret", &[]).status(), 303);

    assert_eq!(
        form(&app, "/confirm", &[("password", "wrong")]).text(),
        "false"
    );
    assert_eq!(
        form(&app, "/confirm", &[("password", "correct horse")]).text(),
        "true"
    );
    assert_eq!(app.get("/secret").text(), "secret");
    // Five tries a minute per user, counted before the hash.
    for _ in 0..3 {
        form(&app, "/confirm", &[("password", "wrong")]);
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    let res = app.request(
        Method::POST,
        "/confirm",
        headers,
        "password=correct+horse".into(),
    );
    assert_eq!(res.status(), 429, "{}", res.text());
}

/// A confirmation belongs to one sign-in: a new sign-in or a logout ends it.
#[test]
fn a_confirmation_does_not_survive_a_new_sign_in() {
    let (app, _) = app();
    create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    form(&app, "/confirm", &[("password", "correct horse")]);
    assert_eq!(app.get("/secret").status(), 200);
    sign_in(&app, "ada@example.com", "correct horse");
    assert_eq!(app.get("/secret").status(), 303);
    form(&app, "/confirm", &[("password", "correct horse")]);
    form(&app, "/logout", &[]);
    sign_in(&app, "ada@example.com", "correct horse");
    assert_eq!(app.get("/secret").status(), 303);
}

#[test]
fn the_confirmation_lasts_the_password_timeout() {
    let recorder = Arc::new(Recorder::default());
    let app = TestApp::new(|b| {
        let mut b = build(recorder)(b);
        b.settings_mut().password_timeout = Duration::from_secs(1);
        b
    });
    create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    form(&app, "/confirm", &[("password", "correct horse")]);
    assert_eq!(app.get("/secret").status(), 200);
    std::thread::sleep(Duration::from_millis(2100));
    assert_eq!(app.get("/secret").status(), 303);
}

// ---- resets ---------------------------------------------------------------------------------

fn reset_app(verify: bool, named: bool) -> (TestApp, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let rec = Arc::clone(&recorder);
    let app = TestApp::new(move |b| {
        let mut b = build(rec)(b);
        if verify {
            b = b.verify_email::<User>();
        }
        if named {
            b = b.routes(|r| {
                r.get("/account/reset/{token}", secret)
                    .name("password.reset");
            });
        }
        b
    });
    (app, recorder)
}

#[test]
fn a_reset_returns_the_user_runs_the_listeners_and_publishes_once() {
    let (app, recorder) = reset_app(false, false);
    let ada = create_user(&app, "ada@example.com", "old password");
    sign_in(&app, "ada@example.com", "old password");
    let token = app
        .block_on(passwords::create_token(&app.db(), ada.id))
        .unwrap();
    let mut result = None;
    let published = events(&app, || {
        result = app
            .block_on(passwords::reset(
                app.app(),
                "ADA@example.com",
                &token,
                "new password",
            ))
            .unwrap();
    });
    assert_eq!(result, Some(ada.id));
    assert_eq!(
        published,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Every,
            except: None,
        }]
    );
    let seen = recorder.seen.lock().unwrap().clone();
    assert_eq!(
        (seen.len(), seen[0].why, seen[0].except.clone()),
        (1, CredentialChange::Reset, None)
    );
    assert_eq!(app.get("/me").status(), 303, "every session ended");
    // Used up; a wrong token or unknown address changes nothing and publishes nothing.
    let published = events(&app, || {
        for (email, token) in [
            ("ada@example.com", token.as_str()),
            ("ada@example.com", "wrong"),
            ("nobody@example.com", token.as_str()),
        ] {
            assert_eq!(
                app.block_on(passwords::reset(app.app(), email, token, "x"))
                    .unwrap(),
                None
            );
        }
    });
    assert!(published.is_empty());
    sign_in(&app, "ada@example.com", "new password");
    // The address is not marked verified when the app does not require verification... the column stays.
    let after = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap();
    assert!(after.email_verified_at.is_none());
}

#[test]
fn a_token_resets_once_even_in_parallel() {
    let (app, _) = reset_app(false, false);
    let ada = create_user(&app, "ada@example.com", "old password");
    let token = app
        .block_on(passwords::create_token(&app.db(), ada.id))
        .unwrap();
    let a = app.app().clone();
    let results = app.block_on(async move {
        let mut tasks = tokio::task::JoinSet::new();
        for i in 0..4 {
            let (a, token) = (a.clone(), token.clone());
            tasks.spawn(async move {
                passwords::reset(&a, "ada@example.com", &token, &format!("new {i}"))
                    .await
                    .unwrap()
            });
        }
        let mut done = Vec::new();
        while let Some(r) = tasks.join_next().await {
            done.push(r.unwrap());
        }
        done
    });
    assert_eq!(
        results.iter().filter(|r| r.is_some()).count(),
        1,
        "{results:?}"
    );
}

/// An app with mail captured: the reset links `send_reset_link` hands out.
fn mailing_app(verify: bool, recorders: Vec<Arc<Recorder>>) -> (TestApp, Arc<Mutex<Vec<String>>>) {
    let links = Arc::new(Mutex::new(Vec::new()));
    let notifier: Arc<dyn passwords::ResetNotifier> = Arc::new(Links(Arc::clone(&links)));
    let app = TestApp::new(move |b| {
        let mut b = build(Arc::new(Recorder::default()))(b).service(notifier);
        for recorder in recorders {
            b = b.credential_listener(RecorderHandle(recorder));
        }
        if verify {
            b = b.verify_email::<User>();
        }
        b
    });
    (app, links)
}

/// The token of the last mailed link.
fn mailed_token(app: &TestApp, links: &Mutex<Vec<String>>, email: &str) -> String {
    app.block_on(passwords::send_reset_link(app.app(), email))
        .unwrap();
    let url = links.lock().unwrap().pop().expect("a link");
    url.split("/reset-password/")
        .nth(1)
        .and_then(|rest| rest.split('?').next())
        .unwrap()
        .to_owned()
}

/// A mailed link proves control of the address it went to: the reset marks that address verified and tells the
/// listeners it was unverified (review BL2). A token not mailed (`create_token`) verifies nothing (review BL3).
#[test]
fn a_reset_marks_the_address_verified_when_the_app_requires_it() {
    let recorder = Arc::new(Recorder::default());
    let (app, links) = mailing_app(true, vec![Arc::clone(&recorder)]);
    let ada = create_user(&app, "ada@example.com", "old password");
    let token = mailed_token(&app, &links, "ada@example.com");
    assert_eq!(token.len(), 64);
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            &token,
            "new"
        ))
        .unwrap()
        .is_some()
    );
    let after = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap();
    assert!(after.email_verified_at.is_some());
    let seen = recorder.seen.lock().unwrap().clone();
    assert_eq!(
        (seen[0].why, seen[0].was_unverified),
        (CredentialChange::Reset, true)
    );
    // An address verified already: the reset verifies nothing new.
    let carol = create_user(&app, "carol@example.com", "old password");
    app.block_on(app.db().execute(&format!(
        "UPDATE members SET email_verified_at = '2026-10-05 00:00:00' WHERE id = {}",
        carol.id
    )))
    .unwrap();
    let token = mailed_token(&app, &links, "carol@example.com");
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "carol@example.com",
            &token,
            "new"
        ))
        .unwrap()
        .is_some()
    );
    assert!(!recorder.seen.lock().unwrap()[1].was_unverified);

    let bob = create_user(&app, "bob@example.com", "old password");
    let token = app
        .block_on(passwords::create_token(&app.db(), bob.id))
        .unwrap();
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "bob@example.com",
            &token,
            "new"
        ))
        .unwrap()
        .is_some()
    );
    let bob = app
        .block_on(User::find(&app.db(), bob.id))
        .unwrap()
        .unwrap();
    assert!(
        bob.email_verified_at.is_none(),
        "an unmailed token verifies nothing"
    );
}

/// Review BL3: a token mailed to the old address does not verify an address the account has since changed to.
#[test]
fn a_reset_verifies_only_the_address_the_link_was_mailed_to() {
    let recorder = Arc::new(Recorder::default());
    let (app, links) = mailing_app(true, vec![Arc::clone(&recorder)]);
    let ada = create_user(&app, "old@example.com", "old password");
    let token = mailed_token(&app, &links, "old@example.com");
    // The account's address changes (unverified) before the link is used.
    app.block_on(app.db().execute(&format!(
        "UPDATE members SET email = 'new@example.com' WHERE id = {}",
        ada.id
    )))
    .unwrap();
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "new@example.com",
            &token,
            "new"
        ))
        .unwrap()
        .is_some(),
        "the password reset still works"
    );
    let after = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap();
    assert!(
        after.email_verified_at.is_none(),
        "new@ never received the link"
    );
    assert!(!recorder.seen.lock().unwrap()[0].was_unverified);
}

/// Review BM1: a failing listener neither skips the other listeners nor the revocation event; the first error is
/// still returned.
#[test]
fn a_failing_listener_never_skips_the_others_or_the_event() {
    let failing = Arc::new(Recorder {
        fail: true,
        ..Default::default()
    });
    let second = Arc::new(Recorder::default());
    let (app, links) = mailing_app(false, vec![Arc::clone(&failing), Arc::clone(&second)]);
    let ada = create_user(&app, "ada@example.com", "old password");
    let token = mailed_token(&app, &links, "ada@example.com");
    let mut result = None;
    let published = events(&app, || {
        result = Some(app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            &token,
            "new",
        )));
    });
    assert!(result.unwrap().is_err(), "the first error is returned");
    assert_eq!(failing.seen.lock().unwrap().len(), 1);
    assert_eq!(
        second.seen.lock().unwrap().len(),
        1,
        "the second listener still ran"
    );
    assert_eq!(
        published,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Every,
            except: None,
        }],
        "published once"
    );
    sign_in(&app, "ada@example.com", "new");
}

#[test]
fn a_listener_error_fails_the_reset_after_the_password_changed() {
    let recorder = Arc::new(Recorder {
        fail: true,
        ..Default::default()
    });
    let app = TestApp::new(build(Arc::clone(&recorder)));
    let ada = create_user(&app, "ada@example.com", "old password");
    let token = app
        .block_on(passwords::create_token(&app.db(), ada.id))
        .unwrap();
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            &token,
            "new"
        ))
        .is_err()
    );
    sign_in(&app, "ada@example.com", "new");
}

/// Review BL1: `password_changed` keeps only a credential of the user whose password changed.
#[test]
fn password_changed_refuses_another_users_credential() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    let admin = auth::Principal::new(ada.id + 1, "test", auth::Credential::token(9, ["*"]));
    let published = events(&app, || {
        let err = app
            .block_on(auth::password_changed(app.app(), ada.id, Some(&admin)))
            .unwrap_err();
        assert!(
            err.to_string().contains("cannot keep a credential"),
            "{err}"
        );
    });
    assert!(published.is_empty());
    assert!(recorder.seen.lock().unwrap().is_empty());
}

/// Review BL4: `logout_other_devices` checks the password at most five times a minute per user.
#[test]
fn logout_other_devices_is_budgeted() {
    let app = memory_budget_app();
    create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    for _ in 0..5 {
        assert_eq!(
            form(&app, "/logout-others", &[("password", "nope")]).text(),
            "false"
        );
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    let res = app.request(
        Method::POST,
        "/logout-others",
        headers,
        "password=correct+horse".into(),
    );
    assert_eq!(res.status(), 429, "{}", res.text());
}

/// Keeps the reset links it is given.
struct Links(Arc<Mutex<Vec<String>>>);

impl passwords::ResetNotifier for Links {
    fn send<'a>(
        &'a self,
        _app: &'a App,
        _email: &'a str,
        url: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        self.0.lock().unwrap().push(url.to_owned());
        Box::pin(async { Ok(()) })
    }
}

/// The reset link follows the route named `password.reset`, else `/reset-password/{token}`, always on `APP_URL`.
#[test]
fn reset_links_follow_the_named_route() {
    for (named, prefix) in [(true, "/account/reset/"), (false, "/reset-password/")] {
        let links = Arc::new(Mutex::new(Vec::new()));
        let notifier: Arc<dyn passwords::ResetNotifier> = Arc::new(Links(Arc::clone(&links)));
        let app = TestApp::new(move |b| {
            let mut b = build(Arc::new(Recorder::default()))(b).service(notifier);
            b.settings_mut().url = "https://app.example".into();
            if named {
                b = b.routes(|r| {
                    r.get("/account/reset/{token}", secret)
                        .name("password.reset");
                });
            }
            b
        });
        create_user(&app, "ada@example.com", "old password");
        app.block_on(passwords::send_reset_link(app.app(), "ada@example.com"))
            .unwrap();
        let url = links.lock().unwrap().pop().expect("a link");
        let path = url.strip_prefix("https://app.example").unwrap();
        assert!(path.starts_with(prefix), "{path}");
        assert!(path.ends_with("?email=ada%40example.com"), "{path}");
        let token = &path[prefix.len()..path.len() - "?email=ada%40example.com".len()];
        assert_eq!(token.len(), 64);
        assert!(
            app.block_on(passwords::reset(app.app(), "ada@example.com", token, "new"))
                .unwrap()
                .is_some()
        );
    }
}

// ---- password_changed -----------------------------------------------------------------------

#[test]
fn password_changed_runs_listeners_ends_remember_me_and_publishes_once() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "old password");
    let res = form(
        &app,
        "/login",
        &[
            ("email", "ada@example.com"),
            ("password", "old password"),
            ("remember", "on"),
        ],
    );
    assert_eq!(res.text(), "in");
    let remember = app.cookie(&remember_cookie(&app)).unwrap();
    app.clear_cookies();
    // App code writes the password itself, then tells core.
    let db = app.db();
    let hash = app.block_on(auth::hash_password("new password")).unwrap();
    app.block_on(async {
        let user = User::find(&db, ada.id).await.unwrap().unwrap();
        user.update(&db, |m| m.password = Set(hash)).await.unwrap();
    });
    let token_principal = auth::Principal::new(ada.id, "test", auth::Credential::token(5, ["*"]));
    let published = events(&app, || {
        app.block_on(auth::password_changed(
            app.app(),
            ada.id,
            Some(&token_principal),
        ))
        .unwrap();
    });
    assert_eq!(
        published,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Every,
            except: Some("test:token:5".into()),
        }]
    );
    assert_eq!(
        recorder.seen.lock().unwrap()[0].why,
        CredentialChange::Changed
    );
    app.set_cookie(&remember_cookie(&app), &remember);
    assert_eq!(app.get("/me").status(), 303, "remember-me ended");
}

// ---- lookups, the model, the second factor ---------------------------------------------------

#[test]
fn lookups_and_the_registered_model() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    let found = app
        .block_on(auth::find_by_email::<User>(app.app(), " ADA@Example.com "))
        .unwrap()
        .unwrap();
    assert_eq!(found.id, ada.id);
    assert!(
        app.block_on(auth::find_by_email::<User>(app.app(), "ádá@example.com"))
            .unwrap()
            .is_none()
    );
    assert_eq!(app.app().auth_model(), Some(TypeId::of::<User>()));
    assert_eq!(TestApp::new(|b| b).app().auth_model(), None);
    use smeltery_core::db::prelude::IdenStatic;
    assert_eq!(
        auth::model_column::<User>("email").unwrap().as_str(),
        "email"
    );
    assert!(auth::model_column::<User>("nope").is_err());
}

struct FixedCode;

impl SecondFactor for FixedCode {
    fn required<'a>(&'a self, _app: &'a App, user: &'a AuthUser) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move { Ok(user.id() == 1) })
    }

    fn verify<'a>(
        &'a self,
        _app: &'a App,
        _user: &'a AuthUser,
        code: &'a str,
    ) -> BoxFuture<'a, Result<SecondFactorVerdict>> {
        Box::pin(async move {
            Ok(match code {
                "999999" => SecondFactorVerdict::TooManyAttempts { retry_after: 42 },
                c if smeltery_core::crypto::constant_time_eq(c, "123456") => {
                    SecondFactorVerdict::Valid
                }
                _ => SecondFactorVerdict::Invalid,
            })
        })
    }
}

#[test]
fn a_second_factor_is_found_once_registered() {
    let bare = TestApp::new(|b| b);
    assert!(bare.app().second_factor().is_none());
    let (plain, _) = app();
    drop(plain);
    let app = TestApp::new(|b| build(Arc::new(Recorder::default()))(b).second_factor(FixedCode));
    let ada = create_user(&app, "ada@example.com", "pw");
    let user = app.block_on(app.app().find_user(ada.id)).unwrap().unwrap();
    let factor = app.app().second_factor().unwrap();
    assert!(app.block_on(factor.required(app.app(), &user)).unwrap());
    let verdict = |code: &str| app.block_on(factor.verify(app.app(), &user, code)).unwrap();
    assert_eq!(verdict("123456"), SecondFactorVerdict::Valid);
    assert!(verdict("123456").is_valid());
    assert_eq!(verdict("000000"), SecondFactorVerdict::Invalid);
    assert_eq!(
        verdict("999999"),
        SecondFactorVerdict::TooManyAttempts { retry_after: 42 }
    );
}

#[test]
fn a_second_factor_verdict_becomes_the_endpoints_answer() {
    use smeltery_core::http::IntoResponse as _;
    assert!(
        SecondFactorVerdict::Valid
            .into_result("code", "The code is invalid.")
            .is_ok()
    );
    let invalid = SecondFactorVerdict::Invalid
        .into_result("code", "The code is invalid.")
        .unwrap_err()
        .into_response();
    assert_eq!(invalid.status(), 422);
    assert!(invalid.headers().get(header::RETRY_AFTER).is_none());
    let limited = SecondFactorVerdict::TooManyAttempts { retry_after: 42 }
        .into_result("code", "The code is invalid.")
        .unwrap_err()
        .into_response();
    assert_eq!(limited.status(), 429);
    assert_eq!(
        limited
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("42")
    );
}

// ---- login policy ------------------------------------------------------------------------------

/// Refuses one address with 403; counts its checks.
struct Suspended {
    email: &'static str,
    checks: Arc<Mutex<Vec<i64>>>,
}

impl LoginPolicy for Suspended {
    fn check<'a>(
        &'a self,
        _app: &'a App,
        user: &'a AuthUser,
    ) -> BoxFuture<'a, Result<LoginDecision>> {
        Box::pin(async move {
            self.checks.lock().unwrap().push(user.id());
            let model = user.downcast::<User>().unwrap();
            Ok(if model.email == self.email {
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

/// Allows everyone; records that it ran.
struct Allows(Arc<Mutex<Vec<i64>>>);

impl LoginPolicy for Allows {
    fn check<'a>(
        &'a self,
        _app: &'a App,
        user: &'a AuthUser,
    ) -> BoxFuture<'a, Result<LoginDecision>> {
        Box::pin(async move {
            self.0.lock().unwrap().push(-user.id());
            Ok(LoginDecision::Allow)
        })
    }
}

#[test]
fn login_policies_run_in_order_and_the_first_refusal_wins() {
    let checks = Arc::new(Mutex::new(Vec::new()));
    let app = TestApp::new({
        let checks = Arc::clone(&checks);
        move |b| {
            build(Arc::new(Recorder::default()))(b)
                .login_policy(Allows(Arc::clone(&checks)))
                .login_policy(Suspended {
                    email: "bob@example.com",
                    checks: Arc::clone(&checks),
                })
        }
    });
    let ada = create_user(&app, "ada@example.com", "pw");
    let bob = create_user(&app, "bob@example.com", "pw");
    let find = |id| app.block_on(app.app().find_user(id)).unwrap().unwrap();
    assert!(app.block_on(app.app().check_login(&find(ada.id))).is_ok());
    let err = app
        .block_on(app.app().check_login(&find(bob.id)))
        .unwrap_err();
    assert_eq!(err.status(), 403);
    let Error::Validation(invalid) = err else {
        panic!("a refusal is a validation error");
    };
    assert_eq!(invalid.message, "This account is suspended.");
    assert_eq!(
        invalid.errors.first("email"),
        Some("This account is suspended.")
    );
    assert_eq!(
        *checks.lock().unwrap(),
        vec![-ada.id, ada.id, -bob.id, bob.id]
    );
    // No policy: everyone may sign in.
    let (plain, _) = plain_app();
    let carl = create_user(&plain, "carl@example.com", "pw");
    let carl = plain
        .block_on(plain.app().find_user(carl.id))
        .unwrap()
        .unwrap();
    assert!(plain.block_on(plain.app().check_login(&carl)).is_ok());
}

fn plain_app() -> (TestApp, Arc<Recorder>) {
    app()
}

#[test]
fn attempt_meets_the_login_policy_before_signing_in() {
    let checks = Arc::new(Mutex::new(Vec::new()));
    let app = TestApp::new({
        let checks = Arc::clone(&checks);
        move |b| {
            build(Arc::new(Recorder::default()))(b).login_policy(Suspended {
                email: "bob@example.com",
                checks,
            })
        }
    });
    create_user(&app, "ada@example.com", "pw");
    create_user(&app, "bob@example.com", "pw");
    // A refused user with the right password: 403 for JSON clients, nobody signed in.
    let refused = app.with_header("accept", "application/json").post_form(
        "/login",
        &[("email", "bob@example.com"), ("password", "pw")],
    );
    assert_eq!(refused.status(), 403, "{}", refused.text());
    assert!(refused.text().contains("This account is suspended."));
    app.without_header("accept");
    assert_eq!(app.get("/me").status(), 303);
    // A browser is sent back with the message on `email`.
    let back = form(
        &app,
        "/login",
        &[("email", "bob@example.com"), ("password", "pw")],
    );
    assert_eq!(back.status(), 303);
    assert_eq!(app.get("/me").status(), 303);
    // A wrong password never reaches the policy; an allowed user signs in.
    assert_eq!(
        form(
            &app,
            "/login",
            &[("email", "bob@example.com"), ("password", "nope")]
        )
        .text(),
        "out"
    );
    assert_eq!(checks.lock().unwrap().len(), 2);
    sign_in(&app, "ada@example.com", "pw");
    assert_eq!(app.get("/me").status(), 200);
}

// ---- follow-ups for authentication crates ------------------------------------------------------

async fn refuse(app: App, session: Session) -> smeltery_core::http::Redirect {
    let mut errors = smeltery_core::validation::ValidationErrors::new();
    errors.add("code", "The code is invalid.");
    let input = smeltery_core::validation::Input::from([
        ("email".to_owned(), "ada@example.com".to_owned()),
        ("password".to_owned(), "secret".to_owned()),
        ("nickname".to_owned(), "ada".to_owned()),
    ]);
    session.flash_errors(&app, &errors, &input);
    smeltery_core::http::Redirect::to("/elsewhere")
}

async fn show_flash(session: Session) -> String {
    format!(
        "{:?} {:?} {:?} {:?}",
        session.errors().first("code"),
        session.old("email"),
        session.old("password"),
        session.old("nickname")
    )
}

#[test]
fn flashed_errors_and_input_follow_the_flash_rules() {
    let app = TestApp::new(|b| {
        b.dont_flash(&["nickname"]).routes(|r| {
            r.post("/refuse", refuse);
            r.get("/elsewhere", show_flash);
        })
    });
    let res = form(&app, "/refuse", &[]);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/elsewhere"));
    assert_eq!(
        app.get("/elsewhere").text(),
        r#"Some("The code is invalid.") Some("ada@example.com") None None"#
    );
}

#[test]
fn json_and_inertia_detection_is_public() {
    let mut headers = HeaderMap::new();
    assert!(!smeltery_core::http::wants_json(&headers));
    assert!(!smeltery_core::http::is_inertia(&headers));
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("application/vnd.api+json"),
    );
    headers.insert("x-inertia", HeaderValue::from_static("true"));
    assert!(smeltery_core::http::wants_json(&headers));
    assert!(smeltery_core::http::is_inertia(&headers));
}

struct MarkingCompletion;

impl auth::LoginCompletion for MarkingCompletion {
    fn complete<'a>(
        &'a self,
        _app: &'a App,
        auth: &'a Auth,
        session: &'a Session,
        headers: &'a HeaderMap,
        user: AuthUser,
        remember: bool,
    ) -> BoxFuture<'a, Result<smeltery_core::Response>> {
        Box::pin(async move {
            auth.login_user(&user, remember).await?;
            session.insert("completed_by", "marking");
            let agent = headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("none")
                .to_owned();
            Ok(smeltery_core::http::IntoResponse::into_response(format!(
                "completed {} {agent}",
                user.id()
            )))
        })
    }
}

async fn complete(
    app: App,
    auth: Auth,
    session: Session,
    headers: HeaderMap,
    Form(f): Form<Login>,
) -> Result<smeltery_core::Response> {
    let user = auth.validate(&f.email, &f.password).await?.expect("valid");
    let completion = app.login_completion().expect("registered");
    completion
        .complete(&app, &auth, &session, &headers, user, false)
        .await
}

#[test]
fn a_login_completion_is_found_and_finishes_the_sign_in() {
    let bare = TestApp::new(|b| b);
    assert!(bare.app().login_completion().is_none());
    let app = TestApp::new(|b| {
        build(Arc::new(Recorder::default()))(b)
            .login_completion(MarkingCompletion)
            .routes(|r| {
                r.post("/complete", complete);
            })
    });
    let ada = create_user(&app, "ada@example.com", "pw");
    app.with_header("user-agent", "tests");
    let res = form(
        &app,
        "/complete",
        &[("email", "ada@example.com"), ("password", "pw")],
    );
    assert_eq!(res.text(), format!("completed {} tests", ada.id));
    assert!(
        app.get("/me").text().starts_with("web:session:"),
        "signed in"
    );
}

#[test]
fn send_reset_link_says_whether_a_link_was_issued() {
    let (app, _links) = mailing_app(false, vec![]);
    let ada = create_user(&app, "ada@example.com", "pw");
    assert_eq!(
        app.block_on(passwords::send_reset_link(app.app(), "ada@example.com"))
            .unwrap(),
        Some(ada.id)
    );
    assert_eq!(
        app.block_on(passwords::send_reset_link(app.app(), "ada@example.com"))
            .unwrap(),
        None,
        "one link a minute"
    );
    assert_eq!(
        app.block_on(passwords::send_reset_link(app.app(), "nobody@example.com"))
            .unwrap(),
        None
    );
}

#[test]
fn addresses_are_marked_verified_and_unverified_through_the_model() {
    let (app, _) = reset_app(true, false);
    let ada = create_user(&app, "ada@example.com", "pw");
    let verified_at = |app: &TestApp| {
        app.block_on(User::find(&app.db(), ada.id))
            .unwrap()
            .unwrap()
            .email_verified_at
    };
    assert!(
        app.block_on(auth::mark_verified(app.app(), ada.id))
            .unwrap()
    );
    assert!(verified_at(&app).is_some());
    assert!(
        !app.block_on(auth::mark_verified(app.app(), ada.id))
            .unwrap(),
        "once"
    );
    assert!(
        app.block_on(auth::mark_unverified(app.app(), ada.id))
            .unwrap()
    );
    assert!(verified_at(&app).is_none());
    assert!(
        !app.block_on(auth::mark_unverified(app.app(), ada.id))
            .unwrap()
    );
    // Without `.verify_email` there is no column to write.
    let (plain, _) = self::app();
    assert!(
        plain
            .block_on(auth::mark_unverified(plain.app(), ada.id))
            .is_err()
    );
}

#[test]
fn verifies_email_tells_whether_the_app_requires_verified_addresses() {
    let (plain, _) = app();
    assert!(!plain.app().verifies_email());
    let (verifying, _) = reset_app(true, false);
    assert!(verifying.app().verifies_email());
}

mod other {
    //! A second user model, for the "two models" build check.
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

#[test]
fn two_different_user_models_fail_the_build() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut settings = smeltery_core::config::Settings::from_env();
    settings.env = "testing".into();
    settings.database_url = String::new();
    let err = runtime
        .block_on(
            AppBuilder::new(settings.clone())
                .auth::<User>()
                .auth::<other::Model>()
                .build(),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("two different user models"), "{err}");
    // The same model twice is harmless.
    assert!(
        runtime
            .block_on(
                AppBuilder::new(settings)
                    .auth::<User>()
                    .auth::<User>()
                    .build()
            )
            .is_ok()
    );
}

#[test]
fn cycle_remember_token_ends_remember_me_cookies() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    let res = form(
        &app,
        "/login",
        &[
            ("email", "ada@example.com"),
            ("password", "pw"),
            ("remember", "on"),
        ],
    );
    assert_eq!(res.text(), "in");
    let remember = app.cookie(&remember_cookie(&app)).unwrap();
    let before = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap();
    app.block_on(auth::cycle_remember_token(app.app(), ada.id))
        .unwrap();
    let after = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap();
    assert_ne!(before.remember_token, after.remember_token);
    assert_eq!(
        after.remember_token.as_deref().map(str::len),
        Some(64),
        "a SHA-256"
    );
    app.clear_cookies();
    app.set_cookie(&remember_cookie(&app), &remember);
    assert_eq!(app.get("/me").status(), 303);
}

async fn plain_login(app: App, auth: Auth, Form(f): Form<Login>) -> Result<&'static str> {
    let user = auth::find_by_email::<User>(&app, &f.email)
        .await?
        .expect("the user");
    auth.login(&user, false).await?;
    Ok("in")
}

/// Review FL1: with a `LoginCompletion` registered, `attempt` and `login` refuse (a stock controller cannot skip the
/// completion's steps); the completion signs in with `login_user`.
#[test]
fn attempt_and_login_refuse_while_a_login_completion_is_registered() {
    let app = TestApp::new(|b| {
        build(Arc::new(Recorder::default()))(b)
            .login_completion(MarkingCompletion)
            .routes(|r| {
                r.post("/complete", complete);
                r.post("/plain-login", plain_login);
            })
    });
    let ada = create_user(&app, "ada@example.com", "pw");
    let fields = [("email", "ada@example.com"), ("password", "pw")];
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    for path in ["/login", "/plain-login"] {
        let res = app.request(
            Method::POST,
            path,
            headers.clone(),
            "email=ada%40example.com&password=pw".into(),
        );
        assert_eq!(res.status(), 500, "{path}: {}", res.text());
        assert_eq!(app.get("/me").status(), 303, "{path}: nobody signed in");
    }
    assert_eq!(
        form(&app, "/complete", &fields).text(),
        format!("completed {} none", ada.id)
    );
    assert_eq!(app.get("/me").status(), 200, "the completion signed in");
    // `AuthUser::of` wraps a loaded model for `login_user`.
    let user = auth::AuthUser::of(&ada);
    assert_eq!(user.id(), ada.id);
}

// ---- core builder 2 round 2: remember-me restores, end_credentials, Retry-After floor ------------------------------

/// Refuses everyone while the flag is set.
struct Switch(Arc<std::sync::atomic::AtomicBool>);

impl LoginPolicy for Switch {
    fn check<'a>(
        &'a self,
        _app: &'a App,
        _user: &'a AuthUser,
    ) -> BoxFuture<'a, Result<LoginDecision>> {
        let refuse = self.0.load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            Ok(if refuse {
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

fn remembered(app: &TestApp, email: &str) -> String {
    let res = form(
        app,
        "/login",
        &[("email", email), ("password", "pw"), ("remember", "on")],
    );
    assert_eq!(res.text(), "in");
    app.cookie(&remember_cookie(app)).unwrap()
}

#[test]
fn a_refused_remember_me_restore_stays_a_guest_and_ends_the_cookie() {
    let suspended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let app = TestApp::new({
        let suspended = Arc::clone(&suspended);
        move |b| build(Arc::new(Recorder::default()))(b).login_policy(Switch(suspended))
    });
    let ada = create_user(&app, "ada@example.com", "pw");
    let remember = remembered(&app, "ada@example.com");
    let token_before = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap()
        .remember_token;
    // Allowed: the cookie alone signs in again.
    app.clear_cookies();
    app.set_cookie(&remember_cookie(&app), &remember);
    assert_eq!(app.get("/me").status(), 200);
    // Suspended: the cookie signs nobody in, is removed, and the stored token is replaced.
    suspended.store(true, std::sync::atomic::Ordering::SeqCst);
    app.clear_cookies();
    app.set_cookie(&remember_cookie(&app), &remember);
    let res = app.get("/me");
    assert_eq!(res.status(), 303, "a guest");
    assert!(
        app.cookie(&remember_cookie(&app)).is_none(),
        "the remember cookie is removed"
    );
    let token_after = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap()
        .remember_token;
    assert_ne!(token_before, token_after);
    // Even after the suspension is lifted, the old cookie is spent.
    suspended.store(false, std::sync::atomic::Ordering::SeqCst);
    app.clear_cookies();
    app.set_cookie(&remember_cookie(&app), &remember);
    assert_eq!(app.get("/me").status(), 303);
}

#[test]
fn end_credentials_ends_sessions_remember_me_runs_the_listeners_and_publishes_once() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    create_user(&app, "bob@example.com", "pw");
    let remember = remembered(&app, "ada@example.com");
    let ada_session = app.cookie(&session_cookie(&app)).unwrap();
    app.clear_cookies();
    sign_in(&app, "bob@example.com", "pw");
    let bob_session = app.cookie(&session_cookie(&app)).unwrap();
    // Ada's open session works before.
    app.clear_cookies();
    app.set_cookie(&session_cookie(&app), &ada_session);
    assert_eq!(app.get("/me").status(), 200);
    let events = events(&app, || {
        app.block_on(auth::end_credentials(app.app(), ada.id))
            .unwrap();
    });
    assert_eq!(
        events,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Every,
            except: None,
        }]
    );
    let seen = recorder.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].user_id, ada.id);
    assert_eq!(seen[0].why, CredentialChange::Ended);
    assert_eq!(seen[0].except, None);
    assert!(!seen[0].was_unverified);
    // Ada's open session is signed out on its next request.
    assert_eq!(app.get("/me").status(), 303);
    // Her remember-me cookie signs nobody in any more.
    app.clear_cookies();
    app.set_cookie(&remember_cookie(&app), &remember);
    assert_eq!(app.get("/me").status(), 303);
    // Bob's session is untouched.
    app.clear_cookies();
    app.set_cookie(&session_cookie(&app), &bob_session);
    assert_eq!(app.get("/me").status(), 200);
    // A new sign-in works and is not ended by the old epoch; a second end ends it again.
    app.clear_cookies();
    sign_in(&app, "ada@example.com", "pw");
    assert_eq!(app.get("/me").status(), 200);
    assert_eq!(app.get("/me").status(), 200);
    app.block_on(auth::end_credentials(app.app(), ada.id))
        .unwrap();
    assert_eq!(app.get("/me").status(), 303);
    let epoch = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap()
        .credentials_epoch;
    assert_eq!(epoch, Some(2));
}

#[test]
fn sign_ins_and_logouts_never_change_the_epoch() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    let epoch = |app: &TestApp| {
        app.block_on(User::find(&app.db(), ada.id))
            .unwrap()
            .unwrap()
            .credentials_epoch
    };
    remembered(&app, "ada@example.com");
    form(&app, "/logout", &[]);
    sign_in(&app, "ada@example.com", "pw");
    assert_eq!(epoch(&app), None);
    // A second device signed in stays signed in when the first signs in again.
    let first = app.cookie(&session_cookie(&app)).unwrap();
    app.clear_cookies();
    sign_in(&app, "ada@example.com", "pw");
    app.clear_cookies();
    app.set_cookie(&session_cookie(&app), &first);
    assert_eq!(app.get("/me").status(), 200);
}

mod plain_user {
    //! A user model without `credentials_epoch` (table `plain_users`).
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "plain_users")]
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

struct CreatePlainUsers;

impl Migration for CreatePlainUsers {
    fn name(&self) -> &'static str {
        "2026_10_05_000002_create_plain_users"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("plain_users", |t| {
                t.id();
                t.string("email").unique();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("plain_users").await
    }
}

async fn epochless_login(auth: Auth, Form(f): Form<Login>) -> Result<&'static str> {
    Ok(if auth.attempt(&f.email, &f.password, true).await? {
        "in"
    } else {
        "out"
    })
}

#[test]
fn a_model_without_the_epoch_keeps_working_and_end_credentials_reports_the_sessions() {
    let recorder = Arc::new(Recorder::default());
    let app = TestApp::new({
        let recorder = Arc::clone(&recorder);
        move |b| {
            b.migrations(|m: &mut Migrator| {
                m.add(CreatePlainUsers);
            })
            .auth::<plain_user::Model>()
            .credential_listener(RecorderHandle(recorder))
            .routes(|r| {
                r.get("/login", login_page).name("login");
                r.post("/login", epochless_login);
                r.get("/me", me).middleware("auth");
            })
        }
    });
    let db = app.db();
    let id = app.block_on(async {
        plain_user::Model::create(
            &db,
            plain_user::ActiveModel {
                email: Set("ada@example.com".into()),
                password: Set(auth::hash_password("pw").await.unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    });
    // Sign-in, the session and remember-me work as without epochs.
    assert_eq!(
        form(
            &app,
            "/login",
            &[("email", "ada@example.com"), ("password", "pw")]
        )
        .text(),
        "in"
    );
    assert_eq!(app.get("/me").status(), 200);
    let remember = app.cookie(&remember_cookie(&app)).unwrap();
    app.clear_cookies();
    app.set_cookie(&remember_cookie(&app), &remember);
    assert_eq!(app.get("/me").status(), 200);
    // end_credentials does everything it can, then reports that open sessions were not ended.
    let err = app
        .block_on(auth::end_credentials(app.app(), id))
        .unwrap_err();
    assert!(err.to_string().contains("credentials_epoch"), "{err}");
    assert_eq!(recorder.seen.lock().unwrap().len(), 1, "the listeners ran");
    assert_eq!(app.get("/me").status(), 200, "the open session stays");
    app.clear_cookies();
    app.set_cookie(&remember_cookie(&app), &remember);
    assert_eq!(
        app.get("/me").status(),
        303,
        "the remember token was replaced"
    );
}

#[test]
fn retry_after_is_never_zero() {
    use smeltery_core::http::IntoResponse as _;
    let invalid = smeltery_core::validation::Invalid::too_many("code", "Too many.", 0);
    assert_eq!(invalid.retry_after, Some(1));
    let res = SecondFactorVerdict::TooManyAttempts { retry_after: 0 }
        .into_result("code", "invalid")
        .unwrap_err()
        .into_response();
    assert_eq!(
        res.headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
}

// ---- round 3: end_credentials ordering and failures ---------------------------------------------------------------

#[test]
fn end_credentials_replaces_the_remember_token_before_it_bumps_the_epoch() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    // A trigger records the epoch at the moment the remember token is written: the order of the two steps.
    let db = app.db();
    app.block_on(async {
        db.execute("CREATE TABLE token_writes (epoch INTEGER)")
            .await
            .unwrap();
        db.execute(
            "CREATE TRIGGER record_token_write AFTER UPDATE OF remember_token ON members \
             BEGIN INSERT INTO token_writes (epoch) VALUES (NEW.credentials_epoch); END",
        )
        .await
        .unwrap();
    });
    app.block_on(auth::end_credentials(app.app(), ada.id))
        .unwrap();
    let rows = app
        .block_on(async {
            use sea_orm::{ConnectionTrait, Statement};
            db.conn()
                .query_all_raw(Statement::from_string(
                    db.conn().get_database_backend(),
                    "SELECT epoch FROM token_writes",
                ))
                .await
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    let epoch: Option<i64> = rows[0].try_get("", "epoch").unwrap();
    assert_eq!(
        epoch, None,
        "the remember token is replaced while the epoch is still the old one"
    );
    let after = app
        .block_on(User::find(&app.db(), ada.id))
        .unwrap()
        .unwrap()
        .credentials_epoch;
    assert_eq!(after, Some(1));
}

#[test]
fn a_failing_remember_token_write_never_skips_the_listeners_the_event_or_the_epoch() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    sign_in(&app, "ada@example.com", "pw");
    let db = app.db();
    app.block_on(async {
        db.execute(
            "CREATE TRIGGER refuse_token_write BEFORE UPDATE OF remember_token ON members \
             BEGIN SELECT RAISE(ABORT, 'remember token write refused'); END",
        )
        .await
        .unwrap();
    });
    let mut result = None;
    let events = events(&app, || {
        result = Some(app.block_on(auth::end_credentials(app.app(), ada.id)));
    });
    let err = result.unwrap().unwrap_err();
    assert!(err.to_string().contains("refused"), "{err}");
    assert_eq!(recorder.seen.lock().unwrap().len(), 1, "the listeners ran");
    assert_eq!(
        events,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Every,
            except: None,
        }]
    );
    // The open session still ends.
    assert_eq!(app.get("/me").status(), 303);
}

// ---- sweep 2 (W2-02): a failed remember-token write never skips the revocation steps -------------------------------

/// Make every write of the remember token fail (a transient database error).
fn refuse_remember_writes(app: &TestApp) {
    let db = app.db();
    app.block_on(async {
        db.execute(
            "CREATE TRIGGER refuse_token_write BEFORE UPDATE OF remember_token ON members \
             BEGIN SELECT RAISE(ABORT, 'remember token write refused'); END",
        )
        .await
        .unwrap();
    });
}

/// The reasons the listeners were told, in order.
fn reasons(recorder: &Recorder) -> Vec<CredentialChange> {
    recorder
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.why)
        .collect()
}

fn revoked_all_of(events: &[AuthEvent], user_id: i64) -> bool {
    matches!(
        events,
        [AuthEvent::RevokedAll { user_id: u, kind: CredentialKind::Every, .. }] if *u == user_id
    )
}

#[test]
fn set_password_runs_the_listeners_and_publishes_when_the_remember_token_write_fails() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    sign_in(&app, "ada@example.com", "pw");
    refuse_remember_writes(&app);
    let mut status = 0;
    let events = events(&app, || {
        status = form(&app, "/password", &[("password", "new pw")]).status();
    });
    assert_eq!(status, 500, "the error is still reported");
    assert_eq!(reasons(&recorder), [CredentialChange::Changed]);
    assert!(revoked_all_of(&events, ada.id), "{events:?}");
}

#[test]
fn logout_other_devices_runs_the_listeners_and_publishes_when_the_remember_token_write_fails() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    sign_in(&app, "ada@example.com", "pw");
    refuse_remember_writes(&app);
    let mut status = 0;
    let events = events(&app, || {
        status = form(&app, "/logout-others", &[("password", "pw")]).status();
    });
    assert_eq!(status, 500);
    assert_eq!(
        reasons(&recorder),
        [CredentialChange::OtherDevicesLoggedOut]
    );
    assert!(revoked_all_of(&events, ada.id), "{events:?}");
}

#[test]
fn password_changed_runs_the_listeners_and_publishes_when_the_remember_token_write_fails() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    refuse_remember_writes(&app);
    let mut result = None;
    let events = events(&app, || {
        result = Some(app.block_on(auth::password_changed(app.app(), ada.id, None)));
    });
    let err = result.unwrap().unwrap_err();
    assert!(err.to_string().contains("refused"), "{err}");
    assert_eq!(reasons(&recorder), [CredentialChange::Changed]);
    assert!(revoked_all_of(&events, ada.id), "{events:?}");
}

#[test]
fn a_reset_runs_the_listeners_and_publishes_when_the_remember_token_write_fails() {
    let (app, recorder) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    let token = app
        .block_on(passwords::create_token(&app.db(), ada.id))
        .unwrap();
    refuse_remember_writes(&app);
    let mut result = None;
    let events = events(&app, || {
        result = Some(app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            &token,
            "new pw",
        )));
    });
    let err = result.unwrap().unwrap_err();
    assert!(err.to_string().contains("refused"), "{err}");
    assert_eq!(reasons(&recorder), [CredentialChange::Reset]);
    assert!(revoked_all_of(&events, ada.id), "{events:?}");
    // The password changed (the token is used up before it is written).
    sign_in(&app, "ada@example.com", "new pw");
}

#[test]
fn logout_signs_out_and_publishes_when_the_remember_token_write_fails() {
    let (app, _) = app();
    let ada = create_user(&app, "ada@example.com", "pw");
    sign_in(&app, "ada@example.com", "pw");
    assert_eq!(app.get("/me").status(), 200);
    refuse_remember_writes(&app);
    let mut status = 0;
    let events = events(&app, || {
        status = form(&app, "/logout", &[]).status();
    });
    assert_eq!(status, 500, "the error is still reported");
    assert!(
        matches!(&events[..], [AuthEvent::Revoked { user_id, .. }] if *user_id == ada.id),
        "{events:?}"
    );
    assert_eq!(app.get("/me").status(), 303, "the session is signed out");
}
