//! The HTTP stack end to end, through the test client.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use smeltery_core::http::{Html, Json, Path, StatusCode};
use smeltery_core::middleware::{Next, Request};
use smeltery_core::testing::TestApp;
use smeltery_core::{App, AppBuilder, Error, Response, Result};

async fn home() -> Html<&'static str> {
    Html("<h1>home</h1>")
}

async fn show(Path(id): Path<u32>) -> Result<String> {
    if id == 0 {
        return Err(Error::not_found());
    }
    Ok(format!("post {id}"))
}

async fn update(Path(id): Path<u32>) -> String {
    format!("updated {id}")
}

async fn fail() -> Result<String> {
    Err(Error::internal("secret detail"))
}

async fn boom() -> String {
    panic!("boom")
}

async fn link(app: App) -> Result<String> {
    app.url("posts.show", &[("id", "5")])
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn tag(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut()
        .append("x-order", "outer".parse().unwrap());
    res
}

async fn tag_inner(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut()
        .append("x-order", "inner".parse().unwrap());
    res
}

async fn deny(_req: Request, _next: Next) -> Result<Response> {
    Err(Error::forbidden())
}

fn build(app: AppBuilder) -> AppBuilder {
    app.middleware("outer", tag)
        .middleware("inner", tag_inner)
        .middleware("deny", deny)
        .routes(|r| {
            r.get("/", home).name("home");
            r.get("/posts/{id}", show).name("posts.show");
            r.put("/posts/{id}", update)
                .middleware("outer")
                .middleware("inner");
            r.get("/fail", fail);
            r.get("/boom", boom);
            r.get("/link", link);
            r.get("/secret", home).middleware("deny");
        })
        .api_routes(|r| {
            r.get("/health", health);
        })
}

fn app_with(debug: bool, root: &std::path::Path) -> TestApp {
    TestApp::new(|mut app| {
        app.settings_mut().debug = debug;
        app.settings_mut().root = root.to_path_buf();
        build(app)
    })
}

fn app(debug: bool) -> TestApp {
    app_with(debug, std::path::Path::new("/nonexistent-smeltery-root"))
}

#[test]
fn routes_answer() {
    let app = app(false);
    let res = app.get("/");
    assert_eq!(res.status(), 200);
    assert_eq!(res.text(), "<h1>home</h1>");
    assert_eq!(app.get("/posts/3").text(), "post 3");
    assert_eq!(app.get("/link").text(), "/posts/5");
    assert_eq!(app.get_json("/api/health").json()["status"], "ok");
    assert!(app.get("/").header("x-request-id").is_some());
}

#[test]
fn not_found_renders_a_page_or_json() {
    let app = app(false);
    let res = app.get("/nope");
    assert_eq!(res.status(), 404);
    assert!(res.text().contains("404 Not Found"));
    assert!(res.header("content-type").unwrap().starts_with("text/html"));
    let res = app.get("/posts/0");
    assert_eq!(res.status(), 404);
    let res = app.get_json("/nope");
    assert_eq!(res.json()["error"], "Not Found");
}

#[test]
fn internal_details_only_in_debug() {
    let res = app(false).get("/fail");
    assert_eq!(res.status(), 500);
    assert!(!res.text().contains("secret detail"));
    let res = app(true).get("/fail");
    assert!(res.text().contains("secret detail"));
}

#[test]
fn panics_become_500() {
    let res = app(false).get("/boom");
    assert_eq!(res.status(), 500);
    assert!(res.text().contains("500 Internal Server Error"));
}

#[test]
fn method_not_allowed_keeps_allow_header() {
    let res = app(false).post_form("/", &[]);
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED.as_u16());
    assert!(res.text().contains("405"));
}

#[test]
fn form_method_spoofing_reaches_put_route_and_middleware_order() {
    let res = app(false).post_form("/posts/9", &[("_method", "PUT")]);
    assert_eq!(res.status(), 200);
    assert_eq!(res.text(), "updated 9");
    let order: Vec<&str> = res
        .headers()
        .get_all("x-order")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    // Responses pass back through the inner middleware first.
    assert_eq!(order, ["inner", "outer"]);
}

#[test]
fn middleware_can_reject() {
    let res = app(false).get("/secret");
    assert_eq!(res.status(), 403);
    assert!(res.text().contains("Forbidden"));
}

#[test]
fn serves_public_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("public/assets")).unwrap();
    std::fs::write(dir.path().join("public/assets/app.css"), "body{}").unwrap();
    let app = app_with(false, dir.path());
    let res = app.get("/assets/app.css");
    assert_eq!(res.status(), 200);
    assert_eq!(res.text(), "body{}");
    assert!(res.header("content-type").unwrap().contains("css"));
    assert_eq!(app.get("/assets/missing.css").status(), 404);
}

#[test]
fn server_drains_and_stops_on_shutdown() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.env = "local".into();
        settings.key = String::new();
        let built = build(AppBuilder::new(settings.clone()))
            .build()
            .await
            .unwrap();
        // Without APP_KEY the server of an app with web routes refuses to start.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let err = smeltery_core::serve_on(built.app, built.router, listener)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("smeltery key:generate"), "{err}");

        settings.key = "0123456789abcdef0123456789abcdef".into();
        let built = build(AppBuilder::new(settings)).build().await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let app = built.app.clone();
        let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));
        app.shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("server stops")
            .unwrap()
            .unwrap();
    });
}

/// `on_serve` hooks run once when the server starts (after `serves_http` is set), never on a plain build, even when
/// `serve --no-agents` dropped the start hooks; a failing one stops the server from starting.
#[test]
fn serve_hooks_run_only_when_serving() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.key = "0123456789abcdef0123456789abcdef".into();
        let runs = Arc::new(AtomicU32::new(0));
        let saw_serving = Arc::new(AtomicBool::new(false));
        let (r, s) = (runs.clone(), saw_serving.clone());
        let built = build(AppBuilder::new(settings.clone()))
            .on_serve(move |app| async move {
                r.fetch_add(1, Ordering::SeqCst);
                s.store(app.serves_http(), Ordering::SeqCst);
                Ok(())
            })
            .build()
            .await
            .unwrap();
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "building runs no serve hook"
        );
        let app = built.app.clone();
        app.skip_background();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));
        for _ in 0..200 {
            if runs.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        app.shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("server stops")
            .unwrap()
            .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(saw_serving.load(Ordering::SeqCst));

        let built = build(AppBuilder::new(settings))
            .on_serve(|_| async { Err(smeltery_core::Error::internal("not ready")) })
            .build()
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let err = smeltery_core::serve_on(built.app, built.router, listener)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not ready"), "{err}");
    });
}
