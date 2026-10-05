//! `Valid<T>` on Inertia form posts (D-275, A4): a JSON body and a multipart body with an `UploadedFile`, both
//! behind the CSRF check through the `X-XSRF-TOKEN` header; failures redirect back with the errors flashed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde::Deserialize;
use smeltery::http::{HeaderMap, HeaderValue, Method, UploadedFile, header};
use smeltery::prelude::*;
use smeltery::testing::{TestApp, TestFile, TestResponse, multipart_body};

#[derive(Debug, Deserialize, smeltery::Validate)]
struct Signup {
    #[validate(required, email)]
    email: String,
}

async fn signup(Valid(form): Valid<Signup>) -> String {
    format!("welcome {}", form.email)
}

#[derive(Debug, Deserialize, smeltery::Validate)]
struct Avatar {
    #[validate(required, max = 255)]
    name: String,
    #[validate(required, max = 2)]
    avatar: Option<UploadedFile>,
}

async fn avatar(Valid(form): Valid<Avatar>) -> Result<String> {
    let file = form.avatar.expect("required");
    Ok(format!(
        "{} {} {}",
        form.name,
        file.name(),
        String::from_utf8_lossy(&file.bytes().await?)
    ))
}

async fn page() -> &'static str {
    "page"
}

async fn errors(session: Session) -> String {
    serde_json::to_string(&session.errors()).unwrap()
}

fn app(root: &std::path::Path) -> TestApp {
    let root = root.to_path_buf();
    TestApp::new(move |mut app| {
        app.settings_mut().root = root;
        app.settings_mut().url = "http://localhost".to_owned();
        app.xsrf_cookie().routes(|r| {
            r.get("/form", page);
            r.get("/errors", errors);
            r.post("/signup", signup);
            r.post("/avatar", avatar);
        })
    })
    .with_csrf()
}

fn inertia(app: &TestApp, path: &str, content_type: &str, body: Vec<u8>) -> TestResponse {
    let token = app.cookie("XSRF-TOKEN").expect("the XSRF-TOKEN cookie");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).unwrap(),
    );
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("text/html, application/xhtml+xml"),
    );
    headers.insert("x-inertia", HeaderValue::from_static("true"));
    headers.insert("x-xsrf-token", HeaderValue::from_str(&token).unwrap());
    headers.insert(
        header::REFERER,
        HeaderValue::from_static("http://localhost/form"),
    );
    app.request(Method::POST, path, headers, body.into())
}

#[test]
fn valid_reads_an_inertia_json_post_and_redirects_back_on_failure() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path());
    app.get("/form");
    let ok = inertia(
        &app,
        "/signup",
        "application/json",
        br#"{"email":"ada@example.com"}"#.to_vec(),
    );
    assert_eq!(ok.status(), 200, "{}", ok.text());
    assert_eq!(ok.text(), "welcome ada@example.com");
    let bad = inertia(
        &app,
        "/signup",
        "application/json",
        br#"{"email":"nope"}"#.to_vec(),
    );
    assert_eq!(bad.status(), 303, "{}", bad.text());
    assert_eq!(bad.header("location"), Some("/form"));
    let flashed: serde_json::Value = serde_json::from_str(&app.get("/errors").text()).unwrap();
    assert!(flashed["email"].is_array(), "{flashed}");
}

#[test]
fn valid_reads_an_inertia_multipart_post_with_an_uploaded_file() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path());
    app.get("/form");
    let (content_type, body) = multipart_body(
        &[("name", "Ada")],
        &[TestFile::new("avatar", "ada.txt", "pixels")],
    );
    let res = inertia(&app, "/avatar", &content_type, body);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(res.text(), "Ada ada.txt pixels");
    // A missing file: the redirect back with the error, not 422 JSON.
    let (content_type, body) = multipart_body(&[("name", "Ada")], &[]);
    let res = inertia(&app, "/avatar", &content_type, body);
    assert_eq!(res.status(), 303, "{}", res.text());
    let flashed: serde_json::Value = serde_json::from_str(&app.get("/errors").text()).unwrap();
    assert!(flashed["avatar"].is_array(), "{flashed}");
}
