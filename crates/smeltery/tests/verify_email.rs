//! Email verification through the facade, the way a generated app uses it: the `verified`
//! middleware, signed links, the verify and resend handlers, and the mail.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use smeltery::auth::{self, Auth, EmailVerificationRequest, verification};
use smeltery::db::migration::{Migration, Migrator, Schema};
use smeltery::db::prelude::Set;
use smeltery::http::{HeaderMap, HeaderValue, Method, header};
use smeltery::prelude::*;
use smeltery::testing::{TestApp, TestResponse};

mod user {
    //! The `User` model (table `users`), with `email_verified_at`.
    use smeltery::db::prelude::*;

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

    impl smeltery::auth::MustVerifyEmail for Model {
        fn email(&self) -> &str {
            &self.email
        }
        fn email_verified_at(&self) -> Option<DateTimeUtc> {
            self.email_verified_at
        }
    }
}

use user::Model as User;

/// Another model, for the build check.
mod other {
    use smeltery::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "admins")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub email: String,
        pub email_verified_at: Option<DateTimeUtc>,
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

    impl smeltery::auth::MustVerifyEmail for Model {
        fn email(&self) -> &str {
            &self.email
        }
        fn email_verified_at(&self) -> Option<DateTimeUtc> {
            self.email_verified_at
        }
    }
}

struct CreateUsers;

impl Migration for CreateUsers {
    fn name(&self) -> &'static str {
        "2026_10_04_000001_create_users_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("users", |t| {
                t.id();
                t.string("name");
                // No unique index: two accounts may share an address here (the id check is
                // then the only guard).
                t.string("email");
                t.datetime("email_verified_at").nullable();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
                t.datetime("updated_at").nullable();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("users").await
    }
}

// ---- handlers, as a generated app writes them -------------------------------------------

async fn dashboard() -> &'static str {
    "dashboard"
}

async fn login_page() -> &'static str {
    "login page"
}

#[derive(serde::Deserialize)]
struct Login {
    email: String,
    password: String,
}

/// The generated login handler: back to the page the `auth` middleware turned the guest away
/// from.
async fn login(app: App, auth: Auth, Form(f): Form<Login>) -> Result<Response> {
    if auth.attempt(&f.email, &f.password, false).await? {
        return Ok(auth.intended(&app.settings().auth_home).into_response());
    }
    Ok("wrong".into_response())
}

async fn notice(app: App, auth: Auth) -> Result<Response> {
    if auth.has_verified_email().await? {
        return Ok(Redirect::to(&app.settings().auth_home).into_response());
    }
    Ok("verify notice".into_response())
}

async fn verify(app: App, session: Session, request: EmailVerificationRequest) -> Result<Redirect> {
    let newly = request.fulfill().await?;
    session.flash("status", if newly { "verified" } else { "already" });
    Ok(Redirect::to(&app.settings().auth_home))
}

async fn send(auth: Auth, back: Back, session: Session) -> Result<Redirect> {
    auth.resend_verification_email().await?;
    session.flash("status", "verification-link-sent");
    Ok(back.redirect())
}

async fn status(session: Session) -> String {
    session.get::<String>("status").unwrap_or_default()
}

/// Registers `name` and signs in, then sends the link like the generated register handler.
async fn register(db: Db, auth: Auth, Path(name): Path<String>) -> Result<String> {
    let user = User::create(
        &db,
        user::ActiveModel {
            name: Set(name.clone()),
            email: Set(format!("{name}@example.com")),
            password: Set(auth::hash_password("pw").await?),
            ..Default::default()
        },
    )
    .await?;
    auth.login(&user, false).await?;
    let sent = auth.send_verification_email().await?;
    Ok(format!("{}:{sent}", user.id))
}

/// A profile update: the new address is unverified, and a link goes to it.
async fn change_email(db: Db, auth: Auth, Path(email): Path<String>) -> Result<String> {
    let user = auth.user::<User>().await?.expect("signed in");
    user.update(&db, |m| {
        m.email = Set(email);
        m.email_verified_at = Set(None);
    })
    .await?;
    Ok(auth.send_verification_email().await?.to_string())
}

/// Behind `verified` only (no `auth`).
async fn members() -> &'static str {
    "members"
}

fn routes(app: AppBuilder) -> AppBuilder {
    let mut app = app
        .migrations(|m: &mut Migrator| {
            m.add(CreateUsers);
        })
        .routes(|r| {
            r.get("/login", login_page).name("login");
            r.post("/login", login);
            r.get("/register/{name}", register);
            r.get("/status", status);
            r.get("/change-email/{email}", change_email)
                .middleware("auth");
            r.get("/members", members).middleware("verified");
            r.get("/dashboard", dashboard)
                .name("dashboard")
                .middleware("auth")
                .middleware("verified");
            r.get("/email/verify", notice)
                .name("verification.notice")
                .middleware("auth");
            r.get("/email/verify/{id}/{hash}", verify)
                .name("verification.verify")
                .middleware("auth");
            r.post("/email/verification-notification", send)
                .name("verification.send")
                .middleware("auth");
        });
    app.settings_mut().url = "https://app.example".to_owned();
    app
}

/// An app that requires verification, with the fake mailer.
fn verifying() -> TestApp {
    TestApp::new(|b| routes(b).auth::<User>().verify_email::<User>().mail())
}

fn mailbox(app: &TestApp) -> smeltery::mail::Mailbox {
    Mailer::of(app.app()).unwrap().mailbox().unwrap()
}

/// Register `name`; their id and the link in the verification mail, as a path.
fn register_and_link(app: &TestApp, name: &str) -> (i64, String) {
    let res = app.get(&format!("/register/{name}"));
    let (id, sent) = res
        .text()
        .split_once(':')
        .map(|(a, b)| (a.parse::<i64>().unwrap(), b.to_owned()))
        .unwrap();
    assert_eq!(sent, "true");
    let (mail, email) = mailbox(app)
        .sent_of::<smeltery::mail::VerifyEmail>()
        .pop()
        .expect("the verification mail");
    assert!(email.has_recipient(&format!("{name}@example.com")));
    let path = mail
        .url
        .strip_prefix("https://app.example")
        .unwrap_or_else(|| panic!("the link is not on APP_URL: {}", mail.url))
        .to_owned();
    (id, path)
}

fn find(app: &TestApp, id: i64) -> User {
    app.block_on(User::find(&app.db(), id)).unwrap().unwrap()
}

fn location(res: &TestResponse) -> (u16, Option<&str>) {
    (res.status(), res.header("location"))
}

// ---- tests ------------------------------------------------------------------------------

#[test]
fn unverified_users_are_sent_to_the_notice_and_json_clients_get_403() {
    let app = verifying();
    let (id, _) = register_and_link(&app, "ada");
    let res = app.get("/dashboard");
    assert_eq!(location(&res), (303, Some("/email/verify")));
    let res = app.get_json("/dashboard");
    assert_eq!(res.status(), 403);
    assert_eq!(res.json()["error"], "Your email address is not verified.");
    assert_eq!(app.get("/email/verify").text(), "verify notice");

    // Verified: through.
    let db = app.db();
    let verified = find(&app, id);
    app.block_on(verified.update(&db, |m| {
        m.email_verified_at = Set(Some(smeltery::db::prelude::ChronoUtc::now()));
    }))
    .unwrap();
    assert_eq!(app.get("/dashboard").text(), "dashboard");
    assert_eq!(app.get_json("/dashboard").status(), 200);
    // The notice sends verified users home.
    assert_eq!(
        location(&app.get("/email/verify")),
        (303, Some("/dashboard"))
    );
}

#[test]
fn a_valid_link_verifies_once_and_is_idempotent() {
    let app = verifying();
    let (id, path) = register_and_link(&app, "ada");
    assert!(path.starts_with(&format!("/email/verify/{id}/")), "{path}");
    assert!(
        path.contains("?expires=") && path.contains("&signature="),
        "{path}"
    );
    assert!(find(&app, id).email_verified_at.is_none());

    assert_eq!(location(&app.get(&path)), (303, Some("/dashboard")));
    assert_eq!(app.get("/status").text(), "verified");
    let first = find(&app, id).email_verified_at.expect("verified");
    // `updated_at` moves with it, like a saved model.
    assert_eq!(find(&app, id).updated_at, Some(first));
    assert_eq!(app.get("/dashboard").text(), "dashboard");

    // The same link again: still fine, the time is kept.
    assert_eq!(location(&app.get(&path)), (303, Some("/dashboard")));
    assert_eq!(app.get("/status").text(), "already");
    assert_eq!(find(&app, id).email_verified_at, Some(first));

    // Verified users are not mailed again.
    let before = mailbox(&app).len();
    assert_eq!(
        location(&app.post_form("/email/verification-notification", &[])),
        (303, Some("/"))
    );
    assert_eq!(mailbox(&app).len(), before);
}

#[test]
fn tampered_links_are_rejected() {
    let app = verifying();
    let (id, path) = register_and_link(&app, "ada");
    let (base, query) = path.split_once('?').unwrap();
    let forbidden = |p: &str| {
        let res = app.get(p);
        assert_eq!(res.status(), 403, "{p}");
        assert!(
            res.text()
                .contains("This verification link is invalid or has expired."),
            "{p}"
        );
    };
    // Another signature, a later expiry, no signature, another hash, another id.
    let signature = query.split("signature=").nth(1).unwrap();
    forbidden(&path.replace(signature, "AAAA"));
    let expires: u64 = query
        .split('&')
        .find_map(|p| p.strip_prefix("expires="))
        .unwrap()
        .parse()
        .unwrap();
    forbidden(&path.replace(
        &format!("expires={expires}"),
        &format!("expires={}", expires + 3600),
    ));
    forbidden(base);
    let hash = base.rsplit('/').next().unwrap();
    forbidden(&path.replace(hash, &"0".repeat(64)));
    forbidden(&path.replace(&format!("/verify/{id}/"), &format!("/verify/{}/", id + 1)));
    assert!(find(&app, id).email_verified_at.is_none());
    assert_eq!(app.get("/dashboard").status(), 303);
    // Untouched, it still works.
    assert_eq!(location(&app.get(&path)), (303, Some("/dashboard")));
}

#[test]
fn a_link_only_works_for_its_user_while_the_email_is_unchanged() {
    let app = verifying();
    let (ada, ada_link) = register_and_link(&app, "ada");
    let (bob, _) = register_and_link(&app, "bob");
    // Bob is signed in now: Ada's link is not his.
    assert_eq!(app.get(&ada_link).status(), 403);
    assert!(find(&app, ada).email_verified_at.is_none());

    // Ada changes her email: the old link stops working.
    app.acting_as(ada);
    let db = app.db();
    app.block_on(find(&app, ada).update(&db, |m| {
        m.email = Set("ada.new@example.com".into());
    }))
    .unwrap();
    assert_eq!(app.get(&ada_link).status(), 403);
    assert!(find(&app, ada).email_verified_at.is_none());
    // A new link for the new address works.
    let url = verification::verification_url(app.app(), &find(&app, ada)).unwrap();
    let path = url.strip_prefix("https://app.example").unwrap();
    assert_eq!(location(&app.get(path)), (303, Some("/dashboard")));
    assert!(find(&app, ada).email_verified_at.is_some());
    assert!(find(&app, bob).email_verified_at.is_none());
}

#[test]
fn guests_clicking_a_link_are_sent_to_login() {
    let app = verifying();
    let (id, path) = register_and_link(&app, "ada");
    app.clear_cookies();
    assert_eq!(location(&app.get(&path)), (303, Some("/login")));
    assert!(find(&app, id).email_verified_at.is_none());
}

#[test]
fn a_link_opened_while_signed_out_works_after_the_login() {
    let app = verifying();
    let (id, path) = register_and_link(&app, "ada");
    assert!(path.contains("?expires=") && path.contains("&signature="));
    app.clear_cookies();
    assert_eq!(location(&app.get(&path)), (303, Some("/login")));
    // The login sends the user back to the whole link, query included.
    let res = app.post_form(
        "/login",
        &[("email", "ada@example.com"), ("password", "pw")],
    );
    assert_eq!(location(&res), (303, Some(path.as_str())));
    assert_eq!(location(&app.get(&path)), (303, Some("/dashboard")));
    assert!(find(&app, id).email_verified_at.is_some());
    assert_eq!(app.get("/status").text(), "verified");
}

#[test]
fn expired_links_are_rejected() {
    let app = TestApp::new(|b| {
        let mut b = routes(b).auth::<User>().verify_email::<User>().mail();
        b.settings_mut().verification_expire = std::time::Duration::ZERO;
        b
    });
    let (id, path) = register_and_link(&app, "ada");
    let (mail, _) = mailbox(&app)
        .sent_of::<smeltery::mail::VerifyEmail>()
        .pop()
        .unwrap();
    assert_eq!(mail.minutes, 0);
    assert_eq!(app.get(&path).status(), 403);
    assert!(find(&app, id).email_verified_at.is_none());
}

#[test]
fn the_link_uses_app_url_not_the_host_header() {
    let app = verifying();
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("evil.example"));
    let res = app.request(Method::GET, "/register/ada", headers, String::new().into());
    assert_eq!(res.status(), 200);
    let (mail, email) = mailbox(&app)
        .sent_of::<smeltery::mail::VerifyEmail>()
        .pop()
        .unwrap();
    assert!(
        mail.url.starts_with("https://app.example/email/verify/"),
        "{}",
        mail.url
    );
    assert!(!mail.url.contains("evil"), "{}", mail.url);
    assert_eq!(
        email.subject(),
        format!("Verify your {} email address", app.app().settings().name)
    );
    assert!(email.html_body().unwrap().contains("expires in 60 minutes"));
}

#[test]
fn resending_is_throttled_to_six_a_minute_per_user() {
    let app = verifying();
    register_and_link(&app, "ada");
    let mailbox = mailbox(&app);
    let before = mailbox.len();
    for _ in 0..6 {
        let res = app.post_form("/email/verification-notification", &[]);
        assert_eq!(res.status(), 303);
    }
    assert_eq!(mailbox.len(), before + 6);
    assert_eq!(app.get("/status").text(), "verification-link-sent");

    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    let res = app.request(
        Method::POST,
        "/email/verification-notification",
        headers,
        String::new().into(),
    );
    assert_eq!(res.status(), 429);
    let message = res.json()["errors"]["email"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.starts_with("Too many verification emails. Please try again in "),
        "{message}"
    );
    assert_eq!(mailbox.len(), before + 6);
    // Web forms are sent back with the message on `email`.
    let mut headers = HeaderMap::new();
    headers.insert(header::REFERER, HeaderValue::from_static("/email/verify"));
    let res = app.request(
        Method::POST,
        "/email/verification-notification",
        headers,
        String::new().into(),
    );
    assert_eq!(location(&res), (303, Some("/email/verify")));

    // Another user has their own budget.
    register_and_link(&app, "bob");
    assert_eq!(
        app.post_form("/email/verification-notification", &[])
            .status(),
        303
    );
}

#[test]
fn without_verify_email_everyone_signed_in_passes() {
    let app = TestApp::new(|b| routes(b).auth::<User>().mail());
    let res = app.get("/register/ada");
    assert!(res.text().ends_with(":false"), "nothing is sent");
    mailbox(&app).assert_nothing_sent();
    assert_eq!(app.get("/dashboard").text(), "dashboard");
    assert_eq!(app.get_json("/dashboard").status(), 200);
    assert_eq!(
        location(&app.get("/email/verify")),
        (303, Some("/dashboard"))
    );
    // A link cannot verify anything here.
    assert_eq!(
        app.get("/email/verify/1/abc?expires=1&signature=x")
            .status(),
        403
    );
}

#[test]
fn guests_do_not_pass_verified() {
    let app = verifying();
    let res = app.get("/dashboard");
    // `auth` runs first and sends guests to login.
    assert_eq!(location(&res), (303, Some("/login")));
}

#[test]
fn verify_email_needs_the_auth_model() {
    let build = |b: AppBuilder| async move { b.build().await.map(|_| ()) };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let settings = || smeltery::config::Settings::from_env();
    let err = rt
        .block_on(build(AppBuilder::new(settings()).verify_email::<User>()))
        .unwrap_err();
    assert!(err.to_string().contains(".verify_email::<User>()"), "{err}");
    let err = rt
        .block_on(build(
            AppBuilder::new(settings())
                .auth::<User>()
                .verify_email::<other::Model>(),
        ))
        .unwrap_err();
    assert!(err.to_string().contains("same model"), "{err}");
    rt.block_on(build(
        AppBuilder::new(settings())
            .auth::<User>()
            .verify_email::<User>(),
    ))
    .unwrap();
}

#[test]
fn the_id_check_holds_when_two_accounts_share_an_email() {
    let app = verifying();
    let (ada, ada_link) = register_and_link(&app, "ada");
    // Bob signs up with the same address (this table has no unique index).
    let bob = app
        .block_on(User::create(
            &app.db(),
            user::ActiveModel {
                name: Set("bob".into()),
                email: Set("ada@example.com".into()),
                password: Set("x".into()),
                ..Default::default()
            },
        ))
        .unwrap();
    app.acting_as(bob.id);
    // Same email hash, valid signature: only the id tells them apart.
    assert_eq!(app.get(&ada_link).status(), 403);
    assert!(find(&app, ada).email_verified_at.is_none());
    assert!(find(&app, bob.id).email_verified_at.is_none());
}

#[test]
fn guests_never_pass_verified_even_without_verify_email() {
    for app in [TestApp::new(|b| routes(b).auth::<User>()), verifying()] {
        assert_eq!(location(&app.get("/members")), (303, Some("/email/verify")));
        let res = app.get_json("/members");
        assert_eq!(res.status(), 403);
        assert_eq!(res.json()["error"], "Your email address is not verified.");
    }
    // Signed in without `.verify_email`: through.
    let app = TestApp::new(|b| routes(b).auth::<User>().mail());
    app.get("/register/ada");
    assert_eq!(app.get("/members").text(), "members");
}

#[test]
fn a_changed_email_gets_its_link_in_the_same_request() {
    let app = verifying();
    let (id, path) = register_and_link(&app, "ada");
    assert_eq!(location(&app.get(&path)), (303, Some("/dashboard")));
    assert_eq!(app.get("/change-email/ada.new@example.com").text(), "true");
    let (mail, email) = mailbox(&app)
        .sent_of::<smeltery::mail::VerifyEmail>()
        .pop()
        .unwrap();
    assert!(
        email.has_recipient("ada.new@example.com"),
        "{:?}",
        mail.email
    );
    assert!(find(&app, id).email_verified_at.is_none());
    // The new link verifies the new address.
    let path = mail.url.strip_prefix("https://app.example").unwrap();
    assert_eq!(location(&app.get(path)), (303, Some("/dashboard")));
    assert!(find(&app, id).email_verified_at.is_some());
}

#[test]
fn a_verify_route_without_id_and_hash_stops_the_build() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(
            AppBuilder::new(smeltery::config::Settings::from_env())
                .auth::<User>()
                .verify_email::<User>()
                .api_routes(|r| {
                    r.get("/email/verify/{user}", dashboard)
                        .name("verification.verify");
                })
                .build(),
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("needs the parameters {id} and {hash}"),
        "{err}"
    );
}
