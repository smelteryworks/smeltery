//! File uploads in controllers: `Valid<T>` reading a multipart form with `UploadedFile` fields.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path as FsPath, PathBuf};

use serde::Deserialize;
use smeltery::http::{HeaderMap, HeaderValue, Method, UploadedFile, header};
use smeltery::prelude::*;
use smeltery::testing::{TestApp, TestFile, TestResponse, multipart_body};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01";

#[derive(Debug, Deserialize, smeltery::Validate)]
struct PhotoForm {
    #[validate(required, max = 255)]
    title: String,
    #[validate(required, max = 2, mimes = "png,jpg")]
    image: Option<UploadedFile>,
}

async fn store(Valid(form): Valid<PhotoForm>) -> Result<String> {
    let image = form.image.expect("required");
    let path = image.store("public/photos").await?;
    Ok(format!(
        "{} {} {} {}",
        form.title,
        image.name(),
        image.mime(),
        path
    ))
}

#[derive(Debug, Deserialize, smeltery::Validate)]
struct LooseForm {
    title: Option<String>,
    file: Option<UploadedFile>,
}

/// Looks at the upload and stores nothing.
async fn peek(Valid(form): Valid<LooseForm>) -> Result<String> {
    Ok(match &form.file {
        Some(f) => format!(
            "{} {} {} {}",
            f.name(),
            f.size(),
            f.mime(),
            String::from_utf8_lossy(&f.bytes().await?)
        ),
        None => format!("none {:?}", form.title),
    })
}

#[derive(Debug, Deserialize)]
struct Target {
    dir: String,
    name: Option<String>,
}

async fn store_into(Query(t): Query<Target>, Valid(form): Valid<LooseForm>) -> Result<String> {
    let file = form.file.expect("a file");
    match t.name {
        Some(name) => file.store_as(&t.dir, &name).await,
        None => file.store(&t.dir).await,
    }
}

/// Where the upload waits while the request runs.
async fn temp_dir(Valid(form): Valid<LooseForm>) -> String {
    let file = form.file.expect("a file");
    let dir = file.temp_path().parent().expect("a directory");
    format!("{} {}", dir.display(), file.temp_path().exists())
}

async fn update(Path(id): Path<u32>, Valid(form): Valid<LooseForm>) -> String {
    format!("put {id} {}", form.file.map_or(0, |f| f.size()))
}

async fn errors(session: Session) -> String {
    let errors: serde_json::Value = session.get("_errors").unwrap_or_default();
    let old: serde_json::Value = session.get("_old_input").unwrap_or_default();
    format!("{errors} {old}")
}

async fn token(session: Session) -> String {
    session.token()
}

fn build_in(root: PathBuf, upload_max: Option<u64>) -> impl FnOnce(AppBuilder) -> AppBuilder {
    move |app| {
        let mut app = app
            .routes(|r| {
                r.post("/photos", store);
                r.post("/peek", peek);
                r.post("/store", store_into);
                r.post("/temp", temp_dir);
                r.put("/photos/{photo}", update);
                r.get("/errors", errors);
                r.get("/token", token);
            })
            .api_routes(|r| {
                r.post("/photos", store);
            });
        app.settings_mut().root = root;
        if let Some(max) = upload_max {
            app.settings_mut().upload_max_bytes = max;
        }
        app
    }
}

fn app(root: &FsPath) -> TestApp {
    TestApp::new(build_in(root.to_path_buf(), None))
}

fn post(
    app: &TestApp,
    path: &str,
    fields: &[(&str, &str)],
    files: &[TestFile],
    extra: &[(&str, &str)],
) -> TestResponse {
    let (content_type, body) = multipart_body(fields, files);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap(),
    );
    for (k, v) in extra {
        headers.insert(
            header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    app.request(Method::POST, path, headers, body.into())
}

const JSON: &[(&str, &str)] = &[("accept", "application/json")];

fn tmp_files(root: &FsPath) -> usize {
    std::fs::read_dir(root.join("storage/framework/uploads")).map_or(0, Iterator::count)
}

#[test]
fn upload_temp_files_live_in_storage_framework() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path());
    let res = app.post_multipart("/temp", &[], &[TestFile::new("file", "a.txt", "hi")]);
    assert_eq!(res.status(), 200, "{}", res.text());
    // Built from components: the app joins them with the OS separator (`\` on Windows).
    let expected = root
        .path()
        .join("storage")
        .join("framework")
        .join("uploads");
    assert_eq!(res.text(), format!("{} true", expected.display()));
    assert_eq!(
        tmp_files(root.path()),
        0,
        "deleted once the request is over"
    );
    // Nothing was stored, so `storage/app` (user files only) was never created.
    assert!(!root.path().join("storage/app").exists());
}

#[test]
fn an_upload_is_stored_and_read_back() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path());
    let res = app.post_multipart(
        "/photos",
        &[("title", "Sea")],
        &[TestFile::new("image", "sea.PNG", PNG)],
    );
    assert_eq!(res.status(), 200, "{}", res.text());
    let text = res.text();
    let parts: Vec<&str> = text.split(' ').collect();
    assert_eq!(&parts[..3], ["Sea", "sea.PNG", "image/png"]);
    let path = parts[3];
    assert!(path.starts_with("public/photos/") && path.ends_with(".png"));
    let stored = root.path().join("storage/app").join(path);
    assert_eq!(std::fs::read(stored).unwrap(), PNG);
    assert_eq!(tmp_files(root.path()), 0);

    // `bytes()` and the other accessors; the temp file is gone once the request is over.
    let res = app.post_multipart("/peek", &[], &[TestFile::new("file", "notes.txt", "hello")]);
    assert_eq!(res.text(), "notes.txt 5 text/plain hello");
    assert_eq!(tmp_files(root.path()), 0);

    // A file input left empty is absent.
    let res = app.post_multipart("/peek", &[("title", "")], &[TestFile::new("file", "", "")]);
    assert_eq!(res.text(), "none None");
}

#[test]
fn file_rules_report_messages() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path());
    let big = [PNG, &[0u8; 4096]].concat();
    let res = post(
        &app,
        "/photos",
        &[("title", "Sea")],
        &[TestFile::new("image", "sea.png", big)],
        JSON,
    );
    assert_eq!(res.status(), 422);
    assert_eq!(
        res.json()["errors"]["image"][0],
        "The image field must not be greater than 2 kilobytes."
    );

    let gif = b"GIF89a\x01\0\x01\0".to_vec();
    let res = post(
        &app,
        "/photos",
        &[("title", "Sea")],
        &[TestFile::new("image", "sea.gif", gif)],
        JSON,
    );
    assert_eq!(
        res.json()["errors"]["image"][0],
        "The image field must be a file of type: png, jpg."
    );

    let res = post(&app, "/photos", &[("title", "")], &[], JSON);
    assert_eq!(
        res.json()["errors"]["image"][0],
        "The image field is required."
    );
    assert_eq!(
        res.json()["errors"]["title"][0],
        "The title field is required."
    );
    assert_eq!(tmp_files(root.path()), 0);
}

#[test]
fn content_that_contradicts_the_type_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path());
    // A script named and declared as a PNG.
    let res = post(
        &app,
        "/photos",
        &[("title", "Sea")],
        &[TestFile::new(
            "image",
            "shell.png",
            "<?php system($_GET['c']);",
        )],
        JSON,
    );
    assert_eq!(res.status(), 422);
    assert_eq!(
        res.json()["errors"]["image"],
        serde_json::json!(["The image field must be a file whose content matches its type."])
    );
    // PNG bytes declared as text.
    let res = post(
        &app,
        "/peek",
        &[],
        &[TestFile::new("file", "a.png", PNG).with_mime("text/plain")],
        JSON,
    );
    assert_eq!(res.status(), 422);
    assert_eq!(tmp_files(root.path()), 0);
}

#[test]
fn the_request_limit_is_a_validation_error() {
    let root = tempfile::tempdir().unwrap();
    let app = TestApp::new(build_in(root.path().to_path_buf(), Some(1024)));
    let big = [PNG, &[0u8; 4096]].concat();
    let res = post(
        &app,
        "/peek",
        &[("title", "Sea")],
        &[TestFile::new("file", "sea.png", big.clone())],
        JSON,
    );
    assert_eq!(res.status(), 422);
    assert_eq!(
        res.json()["errors"]["file"][0],
        "The file field must not be greater than 1 kilobytes."
    );
    assert_eq!(tmp_files(root.path()), 0);

    // On a web form: back with the error and the text input.
    let res = post(
        &app,
        "/peek",
        &[("title", "Sea")],
        &[TestFile::new("file", "sea.png", big)],
        &[("referer", "/photos/create")],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/photos/create"));
    let flashed = app.get("/errors").text();
    assert!(
        flashed.contains("must not be greater than 1 kilobytes"),
        "{flashed}"
    );
    assert!(flashed.contains(r#"{"title":"Sea"}"#), "{flashed}");
}

#[test]
fn validation_failures_redirect_back_without_the_file() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path());
    let res = post(
        &app,
        "/photos",
        &[("title", "Sea"), ("password", "secret")],
        &[TestFile::new("image", "sea.gif", b"GIF89a".to_vec())],
        &[("referer", "/photos/create")],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/photos/create"));
    let flashed = app.get("/errors").text();
    assert!(flashed.contains("must be a file of type"), "{flashed}");
    // Old input holds the text fields only (never files or passwords).
    assert!(flashed.ends_with(r#"{"title":"Sea"}"#), "{flashed}");
    assert_eq!(tmp_files(root.path()), 0);
}

#[test]
fn paths_cannot_leave_storage() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path());
    let res = app.post_multipart(
        "/photos",
        &[("title", "Sea")],
        &[TestFile::new("image", "../../../evil.png", PNG)],
    );
    assert_eq!(res.status(), 200);
    assert!(
        res.text()
            .starts_with("Sea evil.png image/png public/photos/")
    );

    for query in [
        "dir=public%2F..%2F..",
        "dir=..%2Fpublic",
        "dir=%2Fetc",
        "dir=uploads",
        "dir=public&name=..%2Fx.txt",
        "dir=public&name=.htaccess",
    ] {
        let res = post(
            &app,
            &format!("/store?{query}"),
            &[],
            &[TestFile::new("file", "a.txt", "x")],
            &[],
        );
        assert_eq!(res.status(), 400, "{query}: {}", res.text());
    }
    let res = post(
        &app,
        "/store?dir=private%2Fnotes&name=a.txt",
        &[],
        &[TestFile::new("file", "a.txt", "x")],
        &[],
    );
    assert_eq!(res.text(), "private/notes/a.txt");
    assert!(root.path().join("storage/app/private/notes/a.txt").exists());
    assert!(!root.path().join("public").exists());
    assert_eq!(tmp_files(root.path()), 0);
}

#[test]
fn csrf_and_method_spoofing_read_multipart_fields() {
    let root = tempfile::tempdir().unwrap();
    let app = TestApp::new(build_in(root.path().to_path_buf(), None)).with_csrf();
    let file = || TestFile::new("file", "a.txt", "hello");
    let res = post(&app, "/peek", &[("_token", "wrong")], &[file()], &[]);
    assert_eq!(res.status(), 419);
    let res = post(&app, "/peek", &[], &[file()], &[]);
    assert_eq!(res.status(), 419);

    let token = csrf_token(&app);
    let res = post(&app, "/peek", &[("_token", &token)], &[file()], &[]);
    assert_eq!(res.text(), "a.txt 5 text/plain hello");

    // `_method` from a multipart form reaches the PUT route.
    let res = post(
        &app,
        "/photos/7",
        &[("_token", &token), ("_method", "PUT")],
        &[file()],
        &[],
    );
    assert_eq!(res.text(), "put 7 5");

    // A file larger than BODY_LIMIT still streams past the CSRF check. The closure gets the path only: moving the
    // `TempDir` into it deleted the folder as soon as the app was built, and the upload then re-created
    // `storage/framework/uploads` under the deleted path, left behind in the system temp folder.
    let small_root = root.path().to_path_buf();
    let small = TestApp::new(move |app| {
        let mut app = build_in(small_root, None)(app);
        app.settings_mut().body_limit = 512;
        app
    })
    .with_csrf();
    let token = csrf_token(&small);
    let big = vec![b'a'; 64 * 1024];
    let res = post(
        &small,
        "/peek",
        &[("_token", &token)],
        &[TestFile::new("file", "big.txt", big)],
        &[],
    );
    assert!(
        res.text().starts_with("big.txt 65536 text/plain"),
        "{}",
        res.status()
    );
    // The app's folder still exists, and the upload's temp file is gone.
    assert!(root.path().is_dir());
    assert_eq!(tmp_files(root.path()), 0);
}

/// The session's CSRF token.
fn csrf_token(app: &TestApp) -> String {
    app.get("/token").text()
}

#[test]
fn a_file_field_needs_a_multipart_form() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path());
    let res = app.post_json(
        "/api/photos",
        &serde_json::json!({"title": "Sea", "image": "/etc/passwd"}),
    );
    assert_eq!(res.status(), 422);
    assert_eq!(
        res.json()["errors"]["image"][0],
        "The image field is invalid."
    );
    let res = post(
        &app,
        "/api/photos",
        &[("title", "Sea"), ("image", "\u{1}smeltery-upload:guess")],
        &[],
        &[],
    );
    assert_eq!(res.status(), 422);
}
