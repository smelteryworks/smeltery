//! The core seams Alloy (Inertia) builds on: Inertia requests are not JSON clients in the web stack, the opt-in
//! `XSRF-TOKEN` cookie and `X-XSRF-TOKEN` header, the Inertia-friendly CSRF failure, web middleware inside the
//! session stack, `Session::flashed` and the view seam (`PagePayload`, `is_view`, `AlloyRenderer`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;

use smeltery_core::http::IntoResponse as _;
use smeltery_core::http::{HeaderMap, HeaderValue, Method, header};
use smeltery_core::middleware::{Next, Request};
use smeltery_core::session::Session;
use smeltery_core::testing::{TestApp, TestFile, TestResponse, multipart_body};
use smeltery_core::view::{AlloyRenderer, PagePayload, RequestHost, ViewData, is_view, view};
use smeltery_core::{Error, Response, Result};
use smeltery_mold::{Host as _, Template as _};
use smeltery_mold_macros::Mold;

async fn store() -> Result<String> {
    Err(Error::validation("email", "The email field is required."))
}

async fn errors(session: Session) -> String {
    serde_json::to_string(&session.errors()).unwrap()
}

fn inertia_json_post(app: &TestApp, path: &str, extra: &[(&'static str, &str)]) -> TestResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    // What `@inertiajs/core` 3.8.0 sends with a form visit.
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("text/html, application/xhtml+xml"),
    );
    headers.insert(
        "x-requested-with",
        HeaderValue::from_static("XMLHttpRequest"),
    );
    headers.insert("x-inertia", HeaderValue::from_static("true"));
    headers.insert(
        header::REFERER,
        HeaderValue::from_static("http://localhost/form"),
    );
    for (k, v) in extra {
        headers.insert(*k, HeaderValue::from_str(v).unwrap());
    }
    app.request(
        Method::POST,
        path,
        headers,
        axum::body::Body::from(r#"{"email":""}"#),
    )
}

fn validation_app() -> TestApp {
    TestApp::new(|mut app| {
        // `Back` follows only a same-site `Referer`.
        app.settings_mut().url = "http://localhost".to_owned();
        app.routes(|r| {
            r.post("/store", store);
            r.get("/errors", errors);
        })
    })
}

/// Before D-275 this answered 422 JSON (a JSON body made the request a "JSON client"), which opens Inertia's
/// error modal; Inertia wants the redirect back with the errors in the session.
#[test]
fn a_failed_validation_on_an_inertia_json_post_redirects_back() {
    let app = validation_app();
    let res = inertia_json_post(&app, "/store", &[]);
    assert_eq!(res.status(), 303, "{}", res.text());
    assert_eq!(res.header("location"), Some("/form"));
    let flashed = app.get("/errors").text();
    assert!(
        flashed.contains("The email field is required."),
        "{flashed}"
    );
}

/// A JSON body without `X-Inertia` is still a JSON client.
#[test]
fn a_plain_json_post_still_gets_422_json() {
    let app = validation_app();
    let res = app.post_json("/store", &serde_json::json!({ "email": "" }));
    assert_eq!(res.status(), 422);
    assert!(res.json()["errors"]["email"].is_array(), "{}", res.text());
}

// ---- web middleware -------------------------------------------------------------------------------------------

fn push_trail(session: &Session, step: &str) -> Vec<String> {
    let mut trail: Vec<String> = session.get("trail").unwrap_or_default();
    trail.push(step.to_owned());
    session.insert("trail", &trail);
    trail
}

async fn trail_mw_1(mut req: Request, next: Next) -> Response {
    let session = req
        .extensions()
        .get::<Session>()
        .cloned()
        .expect("a session");
    push_trail(&session, "mw1");
    req.headers_mut()
        .insert("x-web-mw", HeaderValue::from_static("1"));
    next.run(req).await
}

async fn trail_mw_2(req: Request, next: Next) -> Response {
    let session = req
        .extensions()
        .get::<Session>()
        .cloned()
        .expect("a session");
    push_trail(&session, "mw2");
    let res = next.run(req).await;
    // After the handler, still before the session is saved.
    session.flash("out", "flashed on the way out");
    res
}

async fn trail_alias(req: Request, next: Next) -> Response {
    let session = req
        .extensions()
        .get::<Session>()
        .cloned()
        .expect("a session");
    push_trail(&session, "alias");
    next.run(req).await
}

async fn trail(session: Session) -> String {
    let trail = push_trail(&session, "handler");
    format!(
        "{} | {}",
        trail.join(","),
        serde_json::Value::Object(session.flashed())
    )
}

async fn saw_web_mw(headers: HeaderMap) -> String {
    format!("{:?}", headers.get("x-web-mw"))
}

#[test]
fn web_middleware_runs_inside_the_session_stack_in_order_on_web_routes_only() {
    let app = TestApp::new(|app| {
        app.middleware("alias", trail_alias)
            .web_middleware(trail_mw_1)
            .web_middleware(trail_mw_2)
            .routes(|r| {
                r.get("/trail", trail).middleware("alias");
            })
            .api_routes(|r| {
                r.get("/saw", saw_web_mw);
            })
    });
    assert_eq!(
        app.get("/trail").text(),
        "mw1,mw2,alias,handler | {}",
        "registration order, outside the route's own middleware"
    );
    // The session changes of the first request were saved, the flash made on the way out too.
    assert_eq!(
        app.get("/trail").text(),
        "mw1,mw2,alias,handler,mw1,mw2,alias,handler | {\"out\":\"flashed on the way out\"}"
    );
    assert_eq!(app.get("/api/saw").text(), "None", "API routes skip them");
}

// ---- XSRF-TOKEN cookie and X-XSRF-TOKEN header ------------------------------------------------------------------

/// Records that the action ran (read back through `/ran`).
async fn accepted(session: Session) -> &'static str {
    session.insert("ran", session.get::<u32>("ran").unwrap_or(0) + 1);
    "accepted"
}

/// How often the action and the web middleware ran on a POST.
async fn ran(session: Session) -> String {
    format!(
        "action {} middleware {}",
        session.get::<u32>("ran").unwrap_or(0),
        session.get::<u32>("mw_posts").unwrap_or(0)
    )
}

async fn count_posts(req: Request, next: Next) -> Response {
    if req.method() == Method::POST
        && let Some(session) = req.extensions().get::<Session>()
    {
        session.insert("mw_posts", session.get::<u32>("mw_posts").unwrap_or(0) + 1);
    }
    next.run(req).await
}

async fn page() -> &'static str {
    "page"
}

async fn flashed(session: Session) -> String {
    serde_json::Value::Object(session.flashed()).to_string()
}

async fn echo_body(body: String) -> String {
    body
}

fn xsrf_set_cookie(res: &TestResponse) -> Option<String> {
    res.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("XSRF-TOKEN="))
        .map(str::to_owned)
}

fn csrf_app(xsrf: bool, url: &'static str) -> TestApp {
    TestApp::new(move |mut app| {
        app.settings_mut().url = url.to_owned();
        let app = app.web_middleware(count_posts).routes(|r| {
            r.get("/page", page);
            r.get("/ran", ran);
            r.post("/submit", accepted);
            r.post("/store", store);
            r.get("/flashed", flashed);
            r.post("/upload", echo_body);
        });
        if xsrf { app.xsrf_cookie() } else { app }
    })
    .with_csrf()
}

fn post(app: &TestApp, path: &str, headers: &[(&'static str, &str)]) -> TestResponse {
    let mut map = HeaderMap::new();
    map.insert(
        header::REFERER,
        HeaderValue::from_static("http://localhost/page"),
    );
    for (k, v) in headers {
        map.insert(*k, HeaderValue::from_str(v).unwrap());
    }
    app.request(Method::POST, path, map, axum::body::Body::empty())
}

#[test]
fn the_xsrf_cookie_is_off_by_default() {
    let app = csrf_app(false, "http://localhost");
    let res = app.get("/page");
    assert_eq!(res.status(), 200);
    assert!(xsrf_set_cookie(&res).is_none());
    assert!(app.cookie("XSRF-TOKEN").is_none());
}

#[test]
fn the_xsrf_cookie_carries_a_fresh_masked_token_that_the_header_returns() {
    let app = csrf_app(true, "http://localhost");
    let first = app.get("/page");
    let set = xsrf_set_cookie(&first).expect("the XSRF-TOKEN cookie");
    let parsed = cookie::Cookie::parse(set.clone()).unwrap();
    assert_eq!(parsed.path(), Some("/"));
    assert_eq!(parsed.same_site(), Some(cookie::SameSite::Lax));
    assert_ne!(
        parsed.http_only(),
        Some(true),
        "the page's JavaScript reads it: {set}"
    );
    assert_ne!(parsed.secure(), Some(true), "http APP_URL: {set}");
    assert!(
        parsed.max_age().is_none() && parsed.expires().is_none(),
        "{set}"
    );
    let token_1 = parsed.value().to_owned();
    assert_eq!(app.cookie("XSRF-TOKEN").unwrap(), token_1);
    let token_2 = xsrf_set_cookie(&app.get("/page"))
        .map(|c| cookie::Cookie::parse(c).unwrap().value().to_owned())
        .unwrap();
    assert_ne!(token_1, token_2, "a fresh mask per response");

    // Inertia's client echoes the cookie as X-XSRF-TOKEN; every mask of the token is accepted.
    for token in [&token_1, &token_2] {
        let res = post(&app, "/submit", &[("x-xsrf-token", token)]);
        assert_eq!(res.status(), 200, "{}", res.text());
        assert_eq!(res.text(), "accepted");
    }
    assert_eq!(app.get("/ran").text(), "action 2 middleware 2");
    // A wrong token is refused (a plain request: the 419 page), and the answer carries a fresh cookie.
    let res = post(&app, "/submit", &[("x-xsrf-token", "nope")]);
    assert_eq!(res.status(), 419);
    assert!(xsrf_set_cookie(&res).is_some());
    // Neither the action nor the web middleware ran for the refused post.
    assert_eq!(app.get("/ran").text(), "action 2 middleware 2");
}

#[test]
fn the_xsrf_cookie_is_secure_under_an_https_app_url() {
    let app = csrf_app(true, "https://example.test");
    let set = xsrf_set_cookie(&app.get("/page")).unwrap();
    assert_eq!(cookie::Cookie::parse(set).unwrap().secure(), Some(true));
}

#[test]
fn x_csrf_token_is_read_before_x_xsrf_token() {
    let app = csrf_app(true, "http://localhost");
    app.get("/page");
    let good = app.cookie("XSRF-TOKEN").unwrap();
    assert_eq!(
        post(
            &app,
            "/submit",
            &[("x-csrf-token", &good), ("x-xsrf-token", "bad")]
        )
        .status(),
        200
    );
    assert_eq!(
        post(
            &app,
            "/submit",
            &[("x-csrf-token", "bad"), ("x-xsrf-token", &good)]
        )
        .status(),
        419,
        "the first header present decides"
    );
}

#[test]
fn a_csrf_failure_on_an_inertia_request_redirects_back_with_a_flash_message() {
    let app = csrf_app(true, "http://localhost");
    app.get("/page");
    let res = post(&app, "/submit", &[("x-inertia", "true")]);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/page"));
    assert_eq!(
        app.get("/flashed").text(),
        r#"{"error":"The page expired. Please try again."}"#
    );
    assert_eq!(
        app.get("/ran").text(),
        "action 0 middleware 0",
        "the action and the web middleware never ran"
    );
    assert_eq!(app.get("/flashed").text(), "{}", "flashed once");
    // An Inertia JSON post with a bad token: the same redirect, never 419 JSON.
    let res = inertia_json_post(&app, "/submit", &[("x-xsrf-token", "bad")]);
    assert_eq!(res.status(), 303);
    // Other clients keep the 419 page / JSON.
    assert_eq!(post(&app, "/submit", &[]).status(), 419);
    let res = app.post_json("/submit", &serde_json::json!({}));
    assert_eq!(res.status(), 419);
    assert_eq!(res.json()["error"], "CSRF token mismatch");
    assert_eq!(app.get("/ran").text(), "action 0 middleware 0");
}

/// The `Vary` values naming `X-Inertia` (the compression layer adds `accept-encoding` to compressible answers).
fn vary(res: &TestResponse) -> Vec<String> {
    res.headers()
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter(|v| v.to_ascii_lowercase().contains("x-inertia"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn vary_web_responses_names_the_header_on_every_web_answer_once() {
    async fn varied() -> Response {
        let mut res = "varied".into_response();
        res.headers_mut().insert(
            header::VARY,
            HeaderValue::from_static("Accept-Encoding, x-inertia"),
        );
        res
    }
    let app = TestApp::new(|mut app| {
        app.settings_mut().url = "http://localhost".to_owned();
        app.vary_web_responses("X-Inertia")
            .vary_web_responses("x-inertia")
            .routes(|r| {
                r.get("/page", page);
                r.get("/varied", varied);
                r.post("/submit", accepted);
                r.post("/store", store);
            })
            .api_routes(|r| {
                r.get("/api-page", page);
            })
    })
    .with_csrf();
    assert_eq!(vary(&app.get("/page")), ["X-Inertia"]);
    assert_eq!(
        vary(&app.get("/varied")),
        ["Accept-Encoding, x-inertia"],
        "listed already"
    );
    // The session stack's own answers: the CSRF failure and the redirect after a failed validation.
    assert_eq!(vary(&post(&app, "/submit", &[])), ["X-Inertia"]);
    assert_eq!(
        vary(&post(&app, "/submit", &[("x-inertia", "true")])),
        ["X-Inertia"]
    );
    app.get("/page");
    let token = app.cookie("XSRF-TOKEN");
    assert!(token.is_none(), "no XSRF cookie without xsrf_cookie()");
    let res = inertia_json_post(&app, "/store", &[]);
    assert_eq!(res.status(), 303, "the CSRF failure redirect");
    assert_eq!(vary(&res), ["X-Inertia"]);
    assert!(
        vary(&app.get("/api/api-page")).is_empty(),
        "API routes are not web routes"
    );
    // Apps that do not ask for it keep their headers.
    assert!(vary(&validation_app().get("/errors")).is_empty());
}

#[test]
fn a_failed_validation_redirect_carries_vary_too() {
    let app = TestApp::new(|mut app| {
        app.settings_mut().url = "http://localhost".to_owned();
        app.vary_web_responses("X-Inertia").routes(|r| {
            r.post("/store", store);
        })
    });
    let res = inertia_json_post(&app, "/store", &[]);
    assert_eq!(res.status(), 303);
    assert_eq!(vary(&res), ["X-Inertia"]);
}

#[test]
fn an_inertia_multipart_post_passes_the_csrf_check_by_header_and_keeps_its_body() {
    let app = csrf_app(true, "http://localhost");
    app.get("/page");
    let token = app.cookie("XSRF-TOKEN").unwrap();
    let (content_type, body) = multipart_body(
        &[("title", "Sea")],
        &[TestFile::new("image", "sea.txt", "waves")],
    );
    let send = |token: Option<&str>| {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&content_type).unwrap(),
        );
        headers.insert("x-inertia", HeaderValue::from_static("true"));
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("http://localhost/page"),
        );
        if let Some(token) = token {
            headers.insert("x-xsrf-token", HeaderValue::from_str(token).unwrap());
        }
        app.request(
            Method::POST,
            "/upload",
            headers,
            axum::body::Body::from(body.clone()),
        )
    };
    let res = send(Some(&token));
    assert_eq!(res.status(), 200, "{}", res.text());
    let text = res.text();
    assert!(
        text.contains("name=\"title\"") && text.contains("Sea"),
        "{text}"
    );
    assert!(text.contains("waves"), "the whole body reaches the handler");
    assert_eq!(send(None).status(), 303, "an Inertia CSRF failure");
}

#[test]
fn a_failed_validation_on_an_inertia_post_redirects_back_behind_the_csrf_check() {
    let app = csrf_app(true, "http://localhost");
    app.get("/page");
    let token = app.cookie("XSRF-TOKEN").unwrap();
    let res = inertia_json_post(&app, "/store", &[("x-xsrf-token", &token)]);
    assert_eq!(res.status(), 303, "{}", res.text());
    assert_eq!(res.header("location"), Some("/form"));
}

// ---- the view seam ---------------------------------------------------------------------------------------------

#[derive(Mold)]
#[mold(
    "alloy_root",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
struct AlloyRoot {}

struct FakeAlloy;

impl AlloyRenderer for FakeAlloy {
    fn page(&self, host: &RequestHost, id: &str) -> std::result::Result<String, String> {
        let json = host
            .page_payload()
            .ok_or_else(|| "no Alloy page in this response".to_owned())?
            .json();
        Ok(format!("<script data-page=\"{id}\">{json}</script>"))
    }
    fn head(&self, _host: &RequestHost) -> String {
        "<meta name=\"head\">".to_owned()
    }
    fn vite(&self, _host: &RequestHost, entries: &[&str]) -> std::result::Result<String, String> {
        Ok(entries.join("+"))
    }
}

async fn alloy_page() -> Response {
    let mut res = view(AlloyRoot {});
    res.extensions_mut().insert(PagePayload::new(
        &serde_json::json!({ "component": "home" }),
    ));
    res
}

async fn root_without_page() -> Response {
    view(AlloyRoot {})
}

async fn not_a_view() -> &'static str {
    "text"
}

/// What Alloy's middleware does with `is_view`: tell a Mold page from other responses.
async fn mark_views(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let marker = if is_view(&res) { "yes" } else { "no" };
    res.headers_mut()
        .insert("x-is-view", HeaderValue::from_static(marker));
    res
}

fn view_app(renderer: bool, debug: bool) -> TestApp {
    TestApp::new(move |mut app| {
        app.settings_mut().root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        app.settings_mut().debug = debug;
        let app = app.web_middleware(mark_views).routes(|r| {
            r.get("/alloy", alloy_page);
            r.get("/bare", root_without_page);
            r.get("/text", not_a_view);
        });
        if renderer {
            let renderer: Arc<dyn AlloyRenderer> = Arc::new(FakeAlloy);
            app.service(renderer)
        } else {
            app
        }
    })
}

#[test]
fn the_root_template_renders_through_the_alloy_renderer_with_the_page_payload() {
    let app = view_app(true, false);
    let res = app.get("/alloy");
    assert_eq!(res.status(), 200);
    assert_eq!(res.header("x-is-view"), Some("yes"));
    assert_eq!(
        res.text(),
        "<head>resources/js/app.tsx+x.css<meta name=\"head\"></head>\n\
         <body><script data-page=\"app\">{\"component\":\"home\"}</script></body>\n"
    );
    assert_eq!(app.get("/text").header("x-is-view"), Some("no"));
    // The renderer's own error (no page) is a template error at the directive.
    let res = view_app(true, true).get("/bare");
    assert_eq!(res.status(), 500);
    let text = res.text();
    assert!(text.contains("alloy_root.mold.html:2:7"), "{text}");
    assert!(text.contains("no Alloy page in this response"), "{text}");
}

#[test]
fn without_an_alloy_renderer_alloy_and_vite_are_template_errors() {
    let res = view_app(false, true).get("/alloy");
    assert_eq!(res.status(), 500);
    let text = res.text();
    assert!(
        text.contains("alloy_root.mold.html:1:7"),
        "@vite fails first: {text}"
    );
    assert!(
        text.contains("Alloy is not enabled: call `.alloy(…)` in bootstrap/app.rs"),
        "{text}"
    );
    // The request host's defaults, in the compiled mode too.
    let app = view_app(false, false);
    let host = RequestHost::new(app.app().clone(), ViewData::default());
    assert_eq!(host.alloy_head(), "");
    assert!(host.vite(&[]).is_err());
    assert!(host.alloy_page("app").is_err());
    assert!(AlloyRoot {}.render_compiled(&host).is_err());
}
