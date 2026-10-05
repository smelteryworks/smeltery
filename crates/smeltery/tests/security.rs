//! Security behaviour of sessions, sign-in, password resets, CSRF and uploads through the
//! facade: what security audit 1 found, each case with the request that showed it.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use smeltery::auth::{self, Auth, passwords};
use smeltery::db::migration::{Migration, Migrator, Schema};
use smeltery::http::{HeaderMap, HeaderValue, Method, UploadedFile, header};
use smeltery::prelude::*;
use smeltery::testing::{TestApp, TestFile, TestResponse};

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
        "2026_10_05_000001_create_auth_tables"
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

async fn token(session: Session) -> String {
    session.token()
}

async fn quiet() -> &'static str {
    "quiet"
}

async fn submit() -> &'static str {
    "accepted"
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

#[derive(Deserialize)]
struct Password {
    password: String,
}

async fn logout_others(auth: Auth, Form(f): Form<Password>) -> Result<String> {
    Ok(auth.logout_other_devices(&f.password).await?.to_string())
}

#[derive(Debug, Deserialize, smeltery::Validate)]
struct Signup {
    #[validate(required, email)]
    email: String,
    #[validate(required)]
    name: String,
}

async fn signup(Valid(_form): Valid<Signup>) -> &'static str {
    "ok"
}

#[derive(Mold)]
#[mold("signup", dir = "tests/app/resources/views")]
struct SignupPage {}

async fn signup_page() -> SignupPage {
    SignupPage {}
}

async fn old(session: Session) -> String {
    let old: serde_json::Value = session.get("_old_input").unwrap_or_default();
    old.to_string()
}

#[derive(Debug, Deserialize, smeltery::Validate)]
struct UploadForm {
    file: Option<UploadedFile>,
}

async fn upload(Valid(form): Valid<UploadForm>) -> Result<String> {
    let file = form.file.expect("a file");
    file.store("public/uploads").await
}

fn build(app: AppBuilder) -> AppBuilder {
    let mut app = app
        .migrations(|m: &mut Migrator| {
            m.add(CreateTables);
        })
        .auth::<User>()
        .routes(|r| {
            r.get("/token", token);
            r.get("/quiet", quiet);
            r.post("/submit", submit);
            r.any("/anything", submit);
            r.get("/login", login_page).name("login");
            r.post("/login", login);
            r.get("/me", me).middleware("auth");
            r.post("/logout", logout);
            r.post("/logout-others", logout_others);
            r.get("/signup", signup_page);
            r.post("/signup", signup);
            r.get("/old", old);
            r.post("/upload", upload);
            r.post("/limited", submit).middleware("throttle:2,1");
            r.post("/other-limited", submit).middleware("throttle:2");
        })
        .api_routes(|r| {
            r.post("/limited", submit).middleware("throttle:1,1");
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

fn sign_in(app: &TestApp, email: &str, password: &str) {
    let res = form(
        app,
        "/login",
        &[("email", email), ("password", password)],
        &[],
    );
    assert_eq!(res.text(), "in");
}

fn set_cookies(res: &TestResponse) -> Vec<String> {
    res.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_owned))
        .collect()
}

const SESSION: &str = "smeltery_session";

use smeltery::db::prelude::Set;

// ---- S1-01: password resets use the stored address and the account id ----------------

#[test]
fn reset_mails_the_stored_address_and_changes_the_account_by_id() {
    let app = TestApp::new(|b| build(b).mail());
    // A column that compares case-insensitively, like MySQL's default collations do.
    let db = app.db();
    app.block_on(db.execute("DROP TABLE users")).unwrap();
    app.block_on(db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
         email TEXT NOT NULL COLLATE NOCASE UNIQUE, password TEXT NOT NULL, \
         remember_token TEXT NULL, created_at TEXT NULL, updated_at TEXT NULL)",
    ))
    .unwrap();
    let ada = create_user(&app, "ada@example.com", "old password");
    let bob = create_user(&app, "bob@example.com", "bob password");
    let mailbox = Mailer::of(app.app()).unwrap().mailbox().unwrap();

    app.block_on(passwords::send_reset_link(app.app(), " ADA@Example.COM "))
        .unwrap();
    let (mail, email) = mailbox
        .sent_of::<smeltery::mail::ResetPassword>()
        .pop()
        .expect("the reset mail");
    assert!(
        email.has_recipient("ada@example.com"),
        "the mail goes to the stored address, never the typed one"
    );
    assert!(
        mail.url.ends_with("?email=ada%40example.com"),
        "{}",
        mail.url
    );
    let token = mail
        .url
        .split("/reset-password/")
        .nth(1)
        .and_then(|rest| rest.split('?').next())
        .unwrap()
        .to_owned();
    // The token row is the account's, by its id.
    let rows = app
        .block_on(db.execute(&format!(
            "UPDATE password_reset_tokens SET token = token WHERE user_id = {}",
            ada.id
        )))
        .unwrap();
    assert_eq!(rows, 1);

    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "ADA@EXAMPLE.COM",
            &token,
            "new password"
        ))
        .unwrap()
        .is_some()
    );
    let ada_after = app.block_on(User::find(&db, ada.id)).unwrap().unwrap();
    let bob_after = app.block_on(User::find(&db, bob.id)).unwrap().unwrap();
    assert_ne!(ada_after.password, ada.password);
    assert_eq!(bob_after.password, bob.password, "only the account itself");
    sign_in(&app, "ada@example.com", "new password");
}

#[test]
fn reset_refuses_an_address_that_only_collates_equal() {
    let app = TestApp::new(build);
    let db = app.db();
    let ada = create_user(&app, "ada@example.com", "old password");
    let token = app.block_on(passwords::create_token(&db, ada.id)).unwrap();
    // Any other character than ASCII case makes it another address.
    for typed in [
        "ádá@example.com",
        "ada@exämple.com",
        "ada@example.com\u{200b}",
    ] {
        assert!(
            !app.block_on(passwords::reset(app.app(), typed, &token, "new password"))
                .unwrap()
                .is_some(),
            "{typed}"
        );
    }
}

/// R-4: reset tokens belong to an account id. Two accounts whose addresses a MySQL `*_ai_ci`
/// collation compares equal each keep their own pending token, and using one leaves the other.
#[test]
fn reset_tokens_belong_to_one_account() {
    let app = TestApp::new(build);
    let db = app.db();
    let plain = create_user(&app, "ada@example.com", "old plain");
    let accented = create_user(&app, "ádá@example.com", "old accented");
    let plain_token = app
        .block_on(passwords::create_token(&db, plain.id))
        .unwrap();
    let accented_token = app
        .block_on(passwords::create_token(&db, accented.id))
        .unwrap();
    // One account's token never resets the other.
    assert!(
        !app.block_on(passwords::reset(
            app.app(),
            "ádá@example.com",
            &plain_token,
            "stolen"
        ))
        .unwrap()
        .is_some()
    );
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "ada@example.com",
            &plain_token,
            "new plain"
        ))
        .unwrap()
        .is_some()
    );
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "ádá@example.com",
            &accented_token,
            "new accented"
        ))
        .unwrap()
        .is_some(),
        "the other account's token survived"
    );
    let after = app.block_on(User::find(&db, accented.id)).unwrap().unwrap();
    assert_ne!(after.password, accented.password);
    // Addresses that differ only in letter case (two rows of a case-sensitive `users.email`):
    // the second account's token leaves the first one's in place.
    let lower = create_user(&app, "bea@example.com", "old lower");
    let upper = create_user(&app, "BEA@example.com", "old upper");
    let lower_token = app
        .block_on(passwords::create_token(&db, lower.id))
        .unwrap();
    app.block_on(passwords::create_token(&db, upper.id))
        .unwrap();
    assert!(
        app.block_on(passwords::reset(
            app.app(),
            "bea@example.com",
            &lower_token,
            "new lower"
        ))
        .unwrap()
        .is_some(),
        "the first account's token survived the second one's"
    );
    // Deleting an account deletes its pending token with it.
    app.block_on(passwords::create_token(&db, plain.id))
        .unwrap();
    app.block_on(db.execute(&format!("DELETE FROM users WHERE id = {}", plain.id)))
        .unwrap();
    let left = app
        .block_on(db.execute(&format!(
            "UPDATE password_reset_tokens SET token = token WHERE user_id = {}",
            plain.id
        )))
        .unwrap();
    assert_eq!(left, 0);
}

// ---- S1-02 / S6-14: the login throttle and email normalization ------------------------

#[test]
fn logins_compare_normalized_addresses() {
    let app = TestApp::new(build);
    create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "  ADA@Example.COM ", "correct horse");
}

#[test]
fn the_login_throttle_counts_every_spelling_of_an_address_together() {
    let app = TestApp::new(build);
    create_user(&app, "ada@example.com", "correct horse");
    for email in [
        "ada@example.com",
        "ADA@example.com",
        " ada@EXAMPLE.com",
        "Ada@Example.Com ",
        "ada@example.COM",
    ] {
        let res = form(&app, "/login", &[("email", email), ("password", "x")], &[]);
        assert_eq!(res.text(), "out");
    }
    let res = form(
        &app,
        "/login",
        &[("email", "aDa@example.com"), ("password", "correct horse")],
        &[("accept", "application/json")],
    );
    assert_eq!(res.status(), 429);
}

// ---- S1-03: uploads never become pages of the site -----------------------------------

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01";

fn upload_app(root: &std::path::Path) -> TestApp {
    let root = root.to_path_buf();
    TestApp::new(move |b| {
        let mut b = build(b);
        b.settings_mut().root = root;
        b
    })
}

#[test]
fn stored_uploads_never_keep_an_active_extension() {
    let root = tempfile::tempdir().unwrap();
    let app = upload_app(root.path());
    let store = |name: &str, mime: &str, bytes: &[u8]| {
        let file = TestFile::new("file", name, bytes.to_vec()).with_mime(mime);
        let res = app.post_multipart("/upload", &[], &[file]);
        assert_eq!(res.status(), 200, "{name}: {}", res.text());
        res.text()
    };
    for (name, mime, body) in [
        (
            "x.html",
            "text/html",
            b"<script>alert(1)</script>".as_slice(),
        ),
        (
            "x.HTM",
            "text/html",
            b"<script>alert(1)</script>".as_slice(),
        ),
        (
            "x.svg",
            "image/svg+xml",
            b"<svg onload=alert(1)>".as_slice(),
        ),
        ("x.xml", "application/xml", b"<x/>".as_slice()),
        ("x.js", "text/javascript", b"alert(1)".as_slice()),
        ("x.php", "application/x-php", b"<?php".as_slice()),
    ] {
        let path = store(name, mime, body);
        assert!(path.ends_with(".bin"), "{name} → {path}");
    }
    // Recognised content names the extension; a harmless name keeps its own.
    assert!(store("photo.exe", "application/octet-stream", PNG).ends_with(".png"));
    assert!(store("photo.PNG", "image/png", PNG).ends_with(".png"));
    assert!(store("notes.txt", "text/plain", b"hello").ends_with(".txt"));
    assert!(store("photo.jpeg", "image/jpeg", b"\xff\xd8\xff\xe0\0\x10JFIF").ends_with(".jpeg"));
}

/// R-2: only allow-listed extensions are kept; the XML types browsers run script in (`xsd`,
/// `mathml`, `xhtml`, `rss` …) and unknown ones are stored as `.bin`.
#[test]
fn stored_uploads_keep_only_allow_listed_extensions() {
    let root = tempfile::tempdir().unwrap();
    let app = upload_app(root.path());
    let store = |name: &str, mime: &str| {
        let body = b"<x:script xmlns:x=\"http://www.w3.org/1999/xhtml\">alert(1)</x:script>";
        let file = TestFile::new("file", name, body.to_vec()).with_mime(mime);
        let res = app.post_multipart("/upload", &[], &[file]);
        assert_eq!(res.status(), 200, "{name}: {}", res.text());
        res.text()
    };
    for (name, mime) in [
        ("x.xsd", "text/xml"),
        ("x.mathml", "application/mathml+xml"),
        ("x.xhtml", "application/xhtml+xml"),
        ("x.rss", "application/rss+xml"),
        ("x.dtd", "text/xml"),
        ("x.unknownext", "application/octet-stream"),
    ] {
        let path = store(name, mime);
        assert!(path.ends_with(".bin"), "{name} → {path}");
    }
    for (name, ext) in [
        ("data.CSV", ".csv"),
        ("clip.mp4", ".mp4"),
        ("report.docx", ".docx"),
    ] {
        let path = store(name, "application/octet-stream");
        assert!(path.ends_with(ext), "{name} → {path}");
    }
}

/// R-2: a folder spelled so a file system opens another one (`storage.`, `storage%2e`, NTFS
/// streams, trailing spaces) is refused on every OS: on Windows these served `/storage` files
/// without the upload headers.
#[test]
fn storage_aliases_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let uploads = root.path().join("public/storage/up");
    std::fs::create_dir_all(&uploads).unwrap();
    std::fs::write(uploads.join("evil.html"), "<script>alert(1)</script>").unwrap();
    let app = upload_app(root.path());
    for path in [
        "/storage./up/evil.html",
        "/storage%2e/up/evil.html",
        "/storage%2E/up/evil.html",
        "/storage%20/up/evil.html",
        "/storage::$INDEX_ALLOCATION/up/evil.html",
        "/storage%3A%3A$INDEX_ALLOCATION/up/evil.html",
        "/./storage/up/evil.html",
        "/storage/up/evil.html.",
        "/storage/up/evil.html::$DATA",
        "/storage%5Cup/evil.html",
    ] {
        let res = app.get(path);
        assert_eq!(res.status(), 404, "{path}");
        assert!(!res.text().contains("<script>"), "{path}");
    }
    let res = app.get("/storage/up/evil.html");
    assert_eq!(res.status(), 200);
    assert_eq!(res.header("content-security-policy"), Some("sandbox"));
}

/// R-2: the upload headers follow the file that was served: another link to the uploads
/// folder (here `public/media` → `storage/app/public`) gets them too.
#[test]
fn files_reached_through_another_link_to_the_uploads_are_sandboxed() {
    let root = tempfile::tempdir().unwrap();
    let uploads = root.path().join("storage/app/public");
    std::fs::create_dir_all(&uploads).unwrap();
    std::fs::create_dir_all(root.path().join("public")).unwrap();
    std::fs::write(uploads.join("evil.html"), "<script>alert(1)</script>").unwrap();
    std::fs::write(root.path().join("public/page.html"), "<p>page</p>").unwrap();
    let link = root.path().join("public/media");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&uploads, &link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&uploads, &link);
    if let Err(e) = made {
        eprintln!("SKIPPED: this session may not create symlinks ({e})");
        return;
    }
    let app = upload_app(root.path());
    let res = app.get("/media/evil.html");
    assert_eq!(res.status(), 200);
    assert_eq!(res.header("content-security-policy"), Some("sandbox"));
    assert_eq!(res.header("content-disposition"), Some("attachment"));
    let page = app.get("/page.html");
    assert_eq!(page.status(), 200);
    assert_ne!(page.header("content-security-policy"), Some("sandbox"));
}

#[test]
fn files_under_storage_are_sandboxed_and_downloaded() {
    let root = tempfile::tempdir().unwrap();
    let uploads = root.path().join("public/storage/photos");
    std::fs::create_dir_all(&uploads).unwrap();
    std::fs::write(uploads.join("evil.html"), "<script>alert(1)</script>").unwrap();
    std::fs::write(uploads.join("evil.svg"), "<svg onload=alert(1)/>").unwrap();
    std::fs::write(uploads.join("ok.png"), PNG).unwrap();
    std::fs::write(uploads.join("doc.pdf"), b"%PDF-1.7").unwrap();
    std::fs::write(root.path().join("public/page.html"), "<p>page</p>").unwrap();
    let app = upload_app(root.path());

    for path in [
        "/storage/photos/evil.html",
        "/storage/photos/evil.svg",
        "/storage/photos/%65vil.html",
    ] {
        let res = app.get(path);
        assert_eq!(res.status(), 200, "{path}");
        assert_eq!(
            res.header("content-security-policy"),
            Some("sandbox"),
            "{path}"
        );
        assert_eq!(
            res.header("content-disposition"),
            Some("attachment"),
            "{path}"
        );
        assert_eq!(
            res.header("x-content-type-options"),
            Some("nosniff"),
            "{path}"
        );
    }
    // Another spelling of the folder: a case-insensitive file system (Windows, macOS) serves the file, and then it
    // gets the same headers; a case-sensitive one (Linux) has no such folder, so nothing is served.
    let res = app.get("/STORAGE/photos/evil.html");
    if res.status() == 200 {
        assert_eq!(res.header("content-security-policy"), Some("sandbox"));
        assert_eq!(res.header("content-disposition"), Some("attachment"));
    } else {
        assert_eq!(res.status(), 404, "served or not found, nothing else");
    }
    let image = app.get("/storage/photos/ok.png");
    assert_eq!(image.status(), 200);
    assert_eq!(image.header("content-security-policy"), Some("sandbox"));
    assert_eq!(
        image.header("content-disposition"),
        None,
        "images show inline"
    );
    let pdf = app.get("/storage/photos/doc.pdf");
    assert_eq!(pdf.header("content-disposition"), None);
    assert_ne!(pdf.header("content-security-policy"), Some("sandbox"));
    // Files of the site itself are untouched.
    let page = app.get("/page.html");
    assert_eq!(page.status(), 200);
    assert_ne!(page.header("content-security-policy"), Some("sandbox"));
    assert_eq!(page.header("content-disposition"), None);
}

// ---- S1-04: multipart bodies are bounded ----------------------------------------------

#[test]
fn a_multipart_body_with_too_many_parts_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let app = upload_app(root.path());
    let fields: Vec<(String, String)> = (0..1001)
        .map(|i| (format!("f{i}"), String::new()))
        .collect();
    let fields: Vec<(&str, &str)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let res = app.post_multipart("/upload", &fields, &[]);
    assert_eq!(res.status(), 413, "{}", res.text());
}

// ---- S1-07: sessions end with the credentials they were made with ---------------------

fn database_sessions(b: AppBuilder) -> AppBuilder {
    let mut b = build(b);
    b.settings_mut().session_driver = "database".into();
    b
}

#[test]
fn logout_ends_this_session_and_leaves_other_devices_signed_in() {
    let app = TestApp::new(database_sessions);
    create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    let device_b = app.cookie(SESSION).unwrap();
    app.clear_cookies();
    sign_in(&app, "ada@example.com", "correct horse");
    let device_a = app.cookie(SESSION).unwrap();
    assert_eq!(form(&app, "/logout", &[], &[]).text(), "bye");
    // The database driver deleted A's row: a copy of its cookie is a guest.
    app.clear_cookies();
    app.set_cookie(SESSION, &device_a);
    assert_eq!(
        app.get("/me").status(),
        303,
        "the logged-out session is gone"
    );
    app.clear_cookies();
    app.set_cookie(SESSION, &device_b);
    assert_eq!(
        app.get("/me").status(),
        200,
        "the other device stays signed in"
    );
}

#[test]
fn a_password_change_signs_out_every_session() {
    let app = TestApp::new(build);
    let ada = create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    let db = app.db();
    let hash = app.block_on(auth::hash_password("new password")).unwrap();
    app.block_on(async {
        let user = User::find(&db, ada.id).await.unwrap().unwrap();
        user.update(&db, |m| m.password = Set(hash)).await.unwrap();
    });
    assert_eq!(app.get("/me").status(), 303);
}

#[test]
fn logout_other_devices_ends_them_but_not_this_one() {
    let app = TestApp::new(database_sessions);
    create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    let device_b = app.cookie(SESSION).unwrap();
    app.clear_cookies();
    sign_in(&app, "ada@example.com", "correct horse");
    let wrong = form(&app, "/logout-others", &[("password", "nope")], &[]);
    assert_eq!(wrong.text(), "false");
    let res = form(
        &app,
        "/logout-others",
        &[("password", "correct horse")],
        &[],
    );
    assert_eq!(res.text(), "true");
    assert_eq!(app.get("/me").status(), 200, "this device stays signed in");
    app.clear_cookies();
    app.set_cookie(SESSION, &device_b);
    assert_eq!(
        app.get("/me").status(),
        303,
        "the other device is signed out"
    );
    app.clear_cookies();
    sign_in(&app, "ada@example.com", "correct horse");
}

#[test]
fn a_password_reset_signs_out_every_session() {
    let app = TestApp::new(build);
    let ada = create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    let other_device = app.cookie(SESSION).unwrap();
    let db = app.db();
    let token = app.block_on(passwords::create_token(&db, ada.id)).unwrap();
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
    app.set_cookie(SESSION, &other_device);
    assert_eq!(app.get("/me").status(), 303);
}

#[test]
fn remember_me_on_another_device_leaves_this_session_signed_in() {
    let app = TestApp::new(build);
    create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    let first = app.cookie(SESSION).unwrap();
    app.clear_cookies();
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
    assert_eq!(app.get("/me").status(), 200, "the new session works");
    app.clear_cookies();
    app.set_cookie(SESSION, &first);
    assert_eq!(app.get("/me").status(), 200);
}

#[test]
fn sessions_end_after_their_absolute_lifetime() {
    let app = TestApp::new(|b| {
        let mut b = build(b);
        b.settings_mut().session_absolute_lifetime = Duration::from_secs(3);
        b
    });
    create_user(&app, "ada@example.com", "correct horse");
    sign_in(&app, "ada@example.com", "correct horse");
    assert_eq!(app.get("/me").status(), 200);
    std::thread::sleep(Duration::from_millis(1200));
    // Activity does not extend it.
    assert_eq!(app.get("/me").status(), 200);
    std::thread::sleep(Duration::from_millis(3300));
    assert_eq!(app.get("/me").status(), 303);
}

// ---- S1-08: password reset requests -----------------------------------------------------

#[test]
fn one_reset_mail_per_address_a_minute() {
    let app = TestApp::new(|b| build(b).mail());
    create_user(&app, "ada@example.com", "old password");
    let mailbox = Mailer::of(app.app()).unwrap().mailbox().unwrap();
    for typed in ["ada@example.com", "ADA@example.com", "ada@example.com"] {
        app.block_on(passwords::send_reset_link(app.app(), typed))
            .unwrap();
    }
    assert_eq!(mailbox.sent_of::<smeltery::mail::ResetPassword>().len(), 1);
}

/// Records what it was asked to send, after a delay; fails when told to.
#[derive(Default)]
struct SlowNotifier {
    sent: Mutex<Vec<String>>,
    fail: bool,
}

impl passwords::ResetNotifier for SlowNotifier {
    fn send<'a>(
        &'a self,
        _app: &'a App,
        email: &'a str,
        _url: &'a str,
    ) -> smeltery::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if self.fail {
                return Err(Error::internal("the mail server is down"));
            }
            self.sent.lock().unwrap().push(email.to_owned());
            Ok(())
        })
    }
}

fn notifier_app(notifier: Arc<SlowNotifier>) -> TestApp {
    TestApp::new(move |b| {
        let mut b = build(b);
        // Outside `testing` the mail leaves in the background.
        b.settings_mut().env = "local".to_owned();
        let notifier: Arc<dyn passwords::ResetNotifier> = notifier;
        b.service(notifier)
    })
}

#[test]
fn reset_requests_answer_before_the_mail_is_sent() {
    let notifier = Arc::new(SlowNotifier::default());
    let app = notifier_app(Arc::clone(&notifier));
    create_user(&app, "ada@example.com", "old password");
    let started = Instant::now();
    app.block_on(passwords::send_reset_link(app.app(), "ada@example.com"))
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "a known address answers as fast as an unknown one: {:?}",
        started.elapsed()
    );
    assert!(notifier.sent.lock().unwrap().is_empty());
    app.block_on(async { tokio::time::sleep(Duration::from_millis(2500)).await });
    assert_eq!(*notifier.sent.lock().unwrap(), ["ada@example.com"]);
}

#[test]
fn a_failing_reset_mail_does_not_fail_the_request() {
    let notifier = Arc::new(SlowNotifier {
        fail: true,
        ..SlowNotifier::default()
    });
    let app = TestApp::new({
        let notifier = Arc::clone(&notifier);
        move |b| {
            let notifier: Arc<dyn passwords::ResetNotifier> = notifier;
            build(b).service(notifier)
        }
    });
    create_user(&app, "ada@example.com", "old password");
    // Under `testing` the mail is sent before the call returns; its failure is logged only.
    assert!(
        app.block_on(passwords::send_reset_link(app.app(), "ada@example.com"))
            .is_ok()
    );
}

// ---- S1-09: a new CSRF token at sign-in and sign-out ----------------------------------

#[test]
fn the_csrf_token_changes_at_sign_in_and_sign_out() {
    let app = TestApp::new(build).with_csrf();
    create_user(&app, "ada@example.com", "correct horse");
    let before = app.get("/token").text();
    let res = form(
        &app,
        "/login",
        &[
            ("_token", &before),
            ("email", "ada@example.com"),
            ("password", "correct horse"),
        ],
        &[],
    );
    assert_eq!(res.text(), "in");
    let signed_in = app.get("/token").text();
    assert_ne!(
        signed_in, before,
        "a token known before sign-in is useless after it"
    );
    assert_eq!(
        form(&app, "/submit", &[("_token", &before)], &[]).status(),
        419
    );
    assert_eq!(
        form(&app, "/logout", &[("_token", &signed_in)], &[]).text(),
        "bye"
    );
    assert_ne!(app.get("/token").text(), signed_in);
}

// ---- S1-10: sessions are stored only when there is something to keep ------------------

#[test]
fn a_visit_that_stores_nothing_creates_no_session() {
    let app = TestApp::new(|b| {
        let mut b = build(b);
        b.settings_mut().session_driver = "database".into();
        b
    });
    let res = app.get("/quiet");
    assert_eq!(res.text(), "quiet");
    assert!(
        set_cookies(&res).iter().all(|c| !c.starts_with(SESSION)),
        "{:?}",
        set_cookies(&res)
    );
    let db = app.db();
    let rows = app
        .block_on(db.execute("UPDATE sessions SET last_activity = last_activity"))
        .unwrap();
    assert_eq!(rows, 0, "no row for a visit without a session");
    // A page (which may hold a form) gets a session and its CSRF token.
    let page = app.get("/signup");
    assert!(set_cookies(&page).iter().any(|c| c.starts_with(SESSION)));
    // So does a request that stores something.
    let token = app.get("/token");
    assert!(!token.text().is_empty());
}

// ---- S1-12: secrets are never flashed back --------------------------------------------

#[test]
fn secret_looking_fields_are_not_flashed_as_old_input() {
    let app = TestApp::new(|b| build(b).dont_flash(&["nickname"]));
    let res = form(
        &app,
        "/signup",
        &[
            ("email", "nope"),
            ("current_password", "hunter2"),
            ("api_key", "sk-123"),
            ("card_number", "4111"),
            ("reset_token", "abc"),
            ("nickname", "ada"),
            ("city", "London"),
        ],
        &[],
    );
    assert_eq!(res.status(), 303);
    let old: serde_json::Value = serde_json::from_str(&app.get("/old").text()).unwrap();
    assert_eq!(
        old,
        serde_json::json!({ "email": "nope", "city": "London" })
    );
}

// ---- S1-13: CSRF on every method that is not a read ------------------------------------

#[test]
fn extension_methods_need_the_csrf_token() {
    let app = TestApp::new(build).with_csrf();
    for method in ["PROPFIND", "FOO", "PATCH"] {
        let method = Method::from_bytes(method.as_bytes()).unwrap();
        let res = app.request(method.clone(), "/anything", HeaderMap::new(), "".into());
        assert_eq!(res.status(), 419, "{method}");
    }
    for method in [Method::GET, Method::OPTIONS] {
        let res = app.request(method.clone(), "/anything", HeaderMap::new(), "".into());
        assert_eq!(res.text(), "accepted", "{method}");
    }
}

// ---- S1-14: cookies under an https APP_URL --------------------------------------------

#[test]
fn https_cookies_are_secure_and_host_prefixed_in_any_letter_case() {
    let app = TestApp::new(|b| {
        let mut b = build(b);
        b.settings_mut().url = "HTTPS://app.example".into();
        b
    });
    create_user(&app, "ada@example.com", "correct horse");
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
    let cookies = set_cookies(&res);
    let session = cookies
        .iter()
        .find(|c| c.starts_with("__Host-smeltery_session="))
        .unwrap_or_else(|| panic!("{cookies:?}"));
    let remember = cookies
        .iter()
        .find(|c| c.starts_with("__Host-remember_smeltery_session="))
        .unwrap_or_else(|| panic!("{cookies:?}"));
    for cookie in [session, remember] {
        assert!(
            cookie.contains("Secure") && cookie.contains("Path=/"),
            "{cookie}"
        );
        assert!(!cookie.contains("Domain"), "{cookie}");
    }
    assert_eq!(app.get("/me").status(), 200);
    // The unprefixed name is not read.
    let value = app.cookie("__Host-smeltery_session").unwrap();
    app.clear_cookies();
    app.set_cookie(SESSION, &value);
    assert_eq!(app.get("/me").status(), 303);
}

// ---- S6-03: the `throttle:` route middleware ------------------------------------------

#[test]
fn throttle_middleware_limits_requests_per_route() {
    let app = TestApp::new(build);
    for left in ["1", "0"] {
        let res = form(&app, "/limited", &[], &[]);
        assert_eq!(res.text(), "accepted");
        assert_eq!(res.header("x-ratelimit-limit"), Some("2"));
        assert_eq!(res.header("x-ratelimit-remaining"), Some(left));
    }
    let res = form(&app, "/limited", &[], &[("accept", "application/json")]);
    assert_eq!(res.status(), 429);
    let retry: u64 = res.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=60).contains(&retry), "{retry}");
    assert_eq!(res.json()["error"], "Too Many Requests");
    // Another route counts on its own.
    assert_eq!(form(&app, "/other-limited", &[], &[]).text(), "accepted");
    // API routes count per client too.
    assert_eq!(
        app.post_json("/api/limited", &serde_json::json!({})).text(),
        "accepted"
    );
    assert_eq!(
        app.post_json("/api/limited", &serde_json::json!({}))
            .status(),
        429
    );
    // `route:list` shows the alias as written.
    let route = app
        .app()
        .routes()
        .iter()
        .find(|r| r.path == "/limited")
        .unwrap();
    assert_eq!(route.middleware, ["throttle:2,1"]);
}

#[tokio::test]
async fn an_invalid_throttle_alias_stops_the_build() {
    let built = AppBuilder::new(smeltery::config::Settings::from_env())
        .api_routes(|r| {
            r.post("/x", submit).middleware("throttle:0,1");
        })
        .build()
        .await;
    let err = built.map(|_| ()).unwrap_err().to_string();
    assert!(err.contains("throttle:<max>,<minutes>"), "{err}");
}

// ---- the `throttle:` middleware counts in the app's cache ------------------------------

async fn errors(session: Session) -> String {
    let errors: serde_json::Value = session.get("_errors").unwrap_or_default();
    errors.to_string()
}

fn throttled(cache_store: &'static str, cache_path: Option<std::path::PathBuf>) -> TestApp {
    TestApp::new(move |b| {
        let mut b = b
            .routes(|r| {
                r.post("/reset/{token}", submit).middleware("throttle:2,1");
                r.post("/named", submit).middleware("throttle:1,1,name");
                r.get("/errors", errors);
            })
            .api_routes(|r| {
                r.post("/ping", submit).middleware("throttle:1,1");
            });
        b.settings_mut().cache_store = cache_store.to_owned();
        if let Some(path) = cache_path {
            b.settings_mut().cache_path = path;
        }
        b
    })
}

#[test]
fn throttle_counts_per_route_pattern_not_per_url() {
    let app = throttled("array", None);
    assert_eq!(form(&app, "/reset/aaa", &[], &[]).text(), "accepted");
    assert_eq!(form(&app, "/reset/bbb", &[], &[]).text(), "accepted");
    let res = form(&app, "/reset/ccc", &[], &[("accept", "application/json")]);
    assert_eq!(res.status(), 429, "another token, the same route");
    assert!(res.header("retry-after").is_some());
}

#[test]
fn throttled_forms_go_back_with_the_message_on_a_field() {
    let app = throttled("array", None);
    assert_eq!(
        form(&app, "/named", &[("name", "Ada")], &[]).text(),
        "accepted"
    );
    let res = form(&app, "/named", &[("name", "Ada")], &[("referer", "/form")]);
    assert_eq!((res.status(), res.header("location")), (303, Some("/form")));
    let errors: serde_json::Value = serde_json::from_str(&app.get("/errors").text()).unwrap();
    let message = errors["name"][0].as_str().unwrap();
    assert!(
        message.starts_with("Too many attempts. Please try again in "),
        "{message}"
    );
    // JSON clients get a plain 429.
    let res = form(&app, "/named", &[], &[("accept", "application/json")]);
    assert_eq!(res.status(), 429);
    assert_eq!(
        res.json(),
        serde_json::json!({ "error": "Too Many Requests" })
    );
}

#[test]
fn throttle_counts_are_shared_by_every_process_through_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let web = throttled("file", Some(dir.path().to_path_buf()));
    let worker = throttled("file", Some(dir.path().to_path_buf()));
    assert_eq!(
        web.post_json("/api/ping", &serde_json::json!({})).text(),
        "accepted"
    );
    assert_eq!(
        worker
            .post_json("/api/ping", &serde_json::json!({}))
            .status(),
        429,
        "the second process sees the first one's request"
    );
}

#[test]
fn throttle_fails_closed_when_the_cache_fails() {
    let app = TestApp::new(|b| {
        let mut b = b.api_routes(|r| {
            r.post("/ping", submit).middleware("throttle:5,1");
        });
        // The database store without a database: every cache call fails.
        b.settings_mut().cache_store = "database".to_owned();
        b.settings_mut().database_url = String::new();
        b
    });
    let res = app.post_json("/api/ping", &serde_json::json!({}));
    assert_eq!(res.status(), 500);
    assert_ne!(res.text(), "accepted");
}

#[test]
fn without_a_cache_store_the_throttle_counts_in_memory() {
    let app = throttled("null", None);
    assert_eq!(
        app.post_json("/api/ping", &serde_json::json!({})).text(),
        "accepted"
    );
    assert_eq!(
        app.post_json("/api/ping", &serde_json::json!({})).status(),
        429
    );
}
