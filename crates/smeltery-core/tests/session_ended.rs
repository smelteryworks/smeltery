//! A session another request ended (a logout deleted its row or file) is never written back by a request of that
//! session that was still running: the database and file drivers only update a session that came with the request
//! and kept its id, and create rows or files only for new ids.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde::Deserialize;
use smeltery_core::auth::{self, Auth};
use smeltery_core::db::Record;
use smeltery_core::db::migration::{Migration, Migrator, Schema};
use smeltery_core::db::prelude::Set;
use smeltery_core::http::Form;
use smeltery_core::testing::TestApp;
use smeltery_core::{App, AppBuilder, Result};

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
}

use user::Model as User;

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
                t.timestamps();
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
        schema.drop_if_exists("users").await
    }
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

async fn login_page() -> &'static str {
    "login page"
}

async fn me(auth: Auth) -> String {
    format!("user {}", auth.id().unwrap_or_default())
}

/// A request of the session that is still running while the owner's logout ends it: the logout's save deletes
/// the row (database driver) or the file (file driver) between this request's load and its save.
async fn in_flight(app: App) -> Result<&'static str> {
    if app.settings().session_driver == "file" {
        let dir = app
            .settings()
            .storage_dir()
            .join("framework")
            .join("sessions");
        for entry in std::fs::read_dir(dir)? {
            std::fs::remove_file(entry?.path())?;
        }
    } else {
        app.db()?.execute("DELETE FROM sessions").await?;
    }
    Ok("done")
}

async fn touch(session: smeltery_core::session::Session) -> &'static str {
    session.insert("seen", true);
    "touched"
}

fn build(
    driver: &'static str,
    root: Option<std::path::PathBuf>,
) -> impl FnOnce(AppBuilder) -> AppBuilder {
    move |b: AppBuilder| {
        let mut b = b
            .migrations(|m: &mut Migrator| {
                m.add(CreateTables);
            })
            .auth::<User>()
            .routes(|r| {
                r.get("/login", login_page).name("login");
                r.post("/login", login);
                r.get("/me", me).middleware("auth");
                r.get("/in-flight", in_flight);
                r.get("/touch", touch);
            });
        b.settings_mut().session_driver = driver.into();
        if let Some(root) = root {
            b.settings_mut().root = root;
        }
        b
    }
}

fn signed_in_app(driver: &'static str, root: Option<std::path::PathBuf>) -> TestApp {
    let app = TestApp::new(build(driver, root));
    let db = app.db();
    app.block_on(async {
        User::create(
            &db,
            user::ActiveModel {
                email: Set("ada@example.com".into()),
                password: Set(auth::hash_password("correct horse").await.unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    });
    let res = app.post_form(
        "/login",
        &[("email", "ada@example.com"), ("password", "correct horse")],
    );
    assert_eq!(res.text(), "in");
    assert_eq!(app.get("/me").text(), "user 1");
    app
}

const SESSION: &str = "smeltery_session";

/// The stolen (or second tab's) copy of the cookie runs a request while the session is ended, then is used again.
fn ended_session_stays_ended(app: &TestApp) {
    let copy = app.cookie(SESSION).unwrap();
    let res = app.get("/in-flight");
    assert_eq!(res.text(), "done");
    assert!(
        res.headers()
            .get_all("set-cookie")
            .iter()
            .all(|c| !c.to_str().unwrap().starts_with(&format!("{SESSION}="))),
        "no cookie names the ended session again"
    );
    app.clear_cookies();
    app.set_cookie(SESSION, &copy);
    assert_eq!(
        app.get("/me").status(),
        303,
        "the ended session is not written back"
    );
}

#[test]
fn a_running_request_never_writes_back_a_database_session_a_logout_ended() {
    let app = signed_in_app("database", None);
    ended_session_stays_ended(&app);
}

#[test]
fn a_running_request_never_writes_back_a_file_session_a_logout_ended() {
    let root = tempfile::tempdir().unwrap();
    let app = signed_in_app("file", Some(root.path().to_path_buf()));
    ended_session_stays_ended(&app);
}

#[test]
fn kept_sessions_are_still_updated_and_new_ones_created() {
    for driver in ["database", "file"] {
        let root = tempfile::tempdir().unwrap();
        let app = signed_in_app(driver, Some(root.path().to_path_buf()));
        // An ordinary request of a stored session updates it (the session stays signed in).
        assert_eq!(app.get("/touch").text(), "touched");
        assert_eq!(app.get("/me").text(), "user 1", "{driver}");
        // A new visitor still gets a stored session.
        app.clear_cookies();
        assert_eq!(app.get("/touch").text(), "touched");
        assert!(app.cookie(SESSION).is_some(), "{driver}");
    }
}
