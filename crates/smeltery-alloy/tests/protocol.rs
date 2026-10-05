//! Golden protocol tests (ALLOY.md §5.1): each pins the status, the relevant headers and the page object.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use smeltery::alloy::testing::{AlloyAssertions as _, AlloyRequests as _};
use smeltery::alloy::{self, Alloy, AlloyExt as _, Location, Page, Props, SharedCtx};
use smeltery::http::{HeaderMap, HeaderValue, Method, Redirect, StatusCode, header};
use smeltery::prelude::*;
use smeltery::testing::{TestApp, TestResponse};

#[derive(smeltery::Mold, Default)]
#[mold("app", crate = "smeltery::mold", dir = "tests/app/resources/views")]
struct Root {}

/// A Mold page of the same app (`show.mold.html`).
#[derive(smeltery::Mold)]
#[mold("plain", crate = "smeltery::mold", dir = "tests/app/resources/views")]
struct Plain {}

static HOME_RUNS: AtomicUsize = AtomicUsize::new(0);

async fn home() -> Page {
    HOME_RUNS.fetch_add(1, Ordering::SeqCst);
    alloy::render("home").with("greeting", "Hello")
}

async fn props(counter: axum::extract::Extension<Arc<AtomicUsize>>) -> Page {
    let lazy = counter.0.clone();
    let optional = counter.0.clone();
    let deferred = counter.0.clone();
    let grouped = counter.0.clone();
    alloy::render("props")
        .with("user", json!({ "name": "Ada", "email": "ada@example.test", "address": { "city": "X", "zip": "1" } }))
        .with_lazy("lazy", move || async move {
            lazy.fetch_add(1, Ordering::SeqCst);
            Ok("lazy value")
        })
        .optional("optional", move || async move {
            optional.fetch_add(1, Ordering::SeqCst);
            Ok(1)
        })
        .defer("deferred", move || async move {
            deferred.fetch_add(1, Ordering::SeqCst);
            Ok([1, 2])
        })
        .defer_in("side", "grouped", move || async move {
            grouped.fetch_add(1, Ordering::SeqCst);
            Ok("g")
        })
        .merge("posts", [json!({ "id": 1 })])
        .match_on("posts", "id")
        .prepend("news", ["n"])
        .deep_merge("settings", json!({ "a": { "b": 1 } }))
        .always("always", true)
}

async fn shared(ctx: SharedCtx) -> Result<Props> {
    Ok(Props::new()
        .with("app", json!({ "name": "Forge", "path": ctx.uri().path() }))
        .with("greeting", "shared greeting"))
}

async fn store() -> Result<String> {
    Err(Error::validation("email", "The email field is required."))
}

async fn flash_and_go(session: Session) -> Redirect {
    session.flash("status", "Saved.");
    session.flash("user", json!({ "name": "Ada" }));
    Redirect::to("/home")
}

async fn put_found() -> Response {
    (StatusCode::FOUND, [(header::LOCATION, "/home")]).into_response()
}

async fn to_fragment() -> Redirect {
    Redirect::to("/home#section")
}

async fn external() -> Location {
    alloy::location("https://billing.example.test/portal")
}

async fn nothing() {}

async fn plain() -> Response {
    view(Plain {})
}

async fn logout(session: Session) -> Redirect {
    session.invalidate();
    alloy::clear_history(&session);
    Redirect::to("/home")
}

async fn errors_prop() -> Page {
    alloy::render("home").with("errors", "mine")
}

async fn missing() -> Page {
    alloy::render("errors/404").status(StatusCode::NOT_FOUND)
}

async fn encrypted() -> Page {
    alloy::render("home").encrypt_history(true).clear_history()
}

async fn evil() -> Page {
    alloy::render("home").with(
        "bio",
        "</script><script>alert(1)</script><!-- \u{2028}\u{2029} &",
    )
}

#[derive(serde::Serialize, smeltery::Alloy)]
#[alloy("posts/index")]
struct PostsIndex {
    titles: Vec<String>,
}

async fn typed() -> PostsIndex {
    PostsIndex {
        titles: vec!["Hello".into()],
    }
}

/// The derive refuses tuple structs; a hand-written `Component` that is not a JSON object fails at runtime.
#[derive(serde::Serialize)]
struct NotAnObject(u32);

impl alloy::Component for NotAnObject {
    const NAME: &'static str = "broken";
}

async fn broken() -> Page {
    alloy::Component::into_page(NotAnObject(3))
}

async fn failing_lazy() -> Page {
    alloy::render("home").with_lazy("x", || async {
        Err::<u32, _>(Error::internal("lazy failed"))
    })
}

/// A page whose handler marks its response `no-store` (a page with secrets).
async fn private_page() -> smeltery::Response {
    use smeltery::http::IntoResponse as _;
    let mut response = alloy::render("home").with("codes", ["a"]).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// A page whose handler sets headers the protocol owns, two cookies and a stale body length.
async fn meddling_page() -> smeltery::Response {
    use smeltery::http::IntoResponse as _;
    let mut response = alloy::render("home").into_response();
    let headers = response.headers_mut();
    headers.insert("x-inertia", HeaderValue::from_static("false"));
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    headers.insert(header::VARY, HeaderValue::from_static("Cookie"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("3"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    for (name, value) in [
        ("etag", "\"stale\""),
        ("last-modified", "Mon, 05 Oct 2026 08:00:00 GMT"),
        ("content-range", "bytes 0-2/3"),
        ("content-disposition", "attachment"),
        ("keep-alive", "timeout=5"),
        ("upgrade", "websocket"),
        ("te", "trailers"),
        ("trailer", "Expires"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_static("first=1; Path=/"),
    );
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_static("second=2; Path=/"),
    );
    response
}

fn app_with(alloy: Alloy) -> (TestApp, Arc<AtomicUsize>) {
    let counter = Arc::new(AtomicUsize::new(0));
    let layer_counter = counter.clone();
    let app = TestApp::new(move |mut app| {
        app.settings_mut().root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        app.settings_mut().url = "http://localhost".to_owned();
        app.alloy(alloy)
            .global_middleware(
                move |mut req: smeltery::middleware::Request, next: smeltery::middleware::Next| {
                    let counter = layer_counter.clone();
                    async move {
                        req.extensions_mut().insert(counter);
                        next.run(req).await
                    }
                },
            )
            .routes(|r| {
                r.get("/home", home);
                r.get("/props", props);
                r.post("/store", store);
                r.post("/flash", flash_and_go);
                r.put("/put", put_found);
                r.get("/fragment", to_fragment);
                r.get("/external", external);
                r.get("/nothing", nothing);
                r.get("/plain", plain);
                r.post("/plain", plain);
                r.post("/logout", logout);
                r.get("/errors-prop", errors_prop);
                r.get("/missing", missing);
                r.get("/encrypted", encrypted);
                r.get("/evil", evil);
                r.get("/typed", typed);
                r.get("/broken", broken);
                r.get("/failing-lazy", failing_lazy);
                r.get("/private", private_page);
                r.get("/meddling", meddling_page);
            })
            .api_routes(|r| {
                r.get("/page", home);
            })
    });
    (app, counter)
}

fn app() -> (TestApp, Arc<AtomicUsize>) {
    app_with(Alloy::new().root::<Root>())
}

fn inertia(
    app: &TestApp,
    method: Method,
    path: &str,
    extra: &[(&'static str, &str)],
) -> TestResponse {
    let mut headers = HeaderMap::new();
    headers.insert("x-inertia", HeaderValue::from_static("true"));
    headers.insert(
        "x-inertia-version",
        HeaderValue::from_str(&alloy::version(app.app()).unwrap()).unwrap(),
    );
    headers.insert(
        header::REFERER,
        HeaderValue::from_static("http://localhost/form"),
    );
    for (k, v) in extra {
        headers.insert(*k, HeaderValue::from_str(v).unwrap());
    }
    let body = if method == Method::POST {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        "{}".into()
    } else {
        String::new().into()
    };
    app.request(method, path, headers, body)
}

fn vary_names_inertia(res: &TestResponse) -> bool {
    res.headers()
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| {
            v.split(',')
                .any(|p| p.trim().eq_ignore_ascii_case("x-inertia"))
        })
}

#[test]
fn a_first_visit_gets_the_root_template_with_the_page_embedded() {
    let (app, _) = app();
    let res = app.get("/home");
    assert_eq!(res.status(), 200);
    assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
    assert!(vary_names_inertia(&res));
    let html = res.text();
    assert!(
        html.contains("<script data-page=\"app\" type=\"application/json\">"),
        "{html}"
    );
    assert!(html.contains("</script><div id=\"app\"></div>"), "{html}");
    assert_eq!(
        res.alloy_page(),
        json!({ "component": "home", "props": { "greeting": "Hello", "errors": {} }, "url": "/home", "version": "" })
    );
    // `@vite` under APP_ENV=testing without assets renders nothing; `@alloyHead` nothing without SSR.
    assert!(
        html.starts_with(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<title>Alloy</title>\n\n\n</head>"
        ),
        "{html}"
    );
}

#[test]
fn an_inertia_visit_gets_the_page_object_as_json() {
    let (app, _) = app();
    let res = app.get_alloy("/home?tab=2");
    assert_eq!(res.status(), 200);
    assert_eq!(res.header("x-inertia"), Some("true"));
    assert_eq!(res.header("content-type"), Some("application/json"));
    assert!(vary_names_inertia(&res));
    assert_eq!(
        res.json(),
        json!({ "component": "home", "props": { "greeting": "Hello", "errors": {} }, "url": "/home?tab=2", "version": "" })
    );
}

#[test]
fn an_old_asset_version_gets_a_409_before_the_handler_runs_and_keeps_the_flash() {
    let (app, _) = app_with(Alloy::new().root::<Root>().version(|_| "v2".to_owned()));
    let res = app.request(
        Method::POST,
        "/flash",
        HeaderMap::new(),
        String::new().into(),
    );
    assert_eq!(res.status(), 303);
    let before = HOME_RUNS.load(Ordering::SeqCst);
    let res = inertia(
        &app,
        Method::GET,
        "/home?q=1",
        &[("x-inertia-version", "v1")],
    );
    assert_eq!(res.status(), 409);
    assert_eq!(res.header("x-inertia-location"), Some("/home?q=1"));
    assert_eq!(res.header("x-inertia-version"), Some("v2"));
    assert!(res.header("x-inertia").is_none());
    assert!(res.bytes().is_empty());
    assert!(vary_names_inertia(&res));
    // The full reload that follows still sees the flash.
    let page = app.get("/home");
    assert_eq!(page.alloy_page()["flash"]["status"], "Saved.");
    assert_eq!(page.alloy_page()["version"], "v2");
    // A POST is never checked; with the right version the visit is answered.
    assert_eq!(
        inertia(&app, Method::GET, "/home", &[("x-inertia-version", "v2")]).status(),
        200
    );
    // The handler ran for the two answered visits only (other tests may run it concurrently: at least that).
    assert!(HOME_RUNS.load(Ordering::SeqCst) >= before + 2);
}

#[test]
fn the_version_check_skips_the_handler() {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    async fn counted() -> Page {
        RUNS.fetch_add(1, Ordering::SeqCst);
        alloy::render("home")
    }
    let app = TestApp::new(|mut app| {
        app.settings_mut().root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        app.alloy(Alloy::new().root::<Root>().version(|_| "new".to_owned()))
            .routes(|r| {
                r.get("/counted", counted);
            })
    });
    let mut headers = HeaderMap::new();
    headers.insert("x-inertia", HeaderValue::from_static("true"));
    headers.insert("x-inertia-version", HeaderValue::from_static("old"));
    let res = app.request(
        Method::GET,
        "/counted",
        headers.clone(),
        String::new().into(),
    );
    assert_eq!(res.status(), 409);
    let res = app.request(Method::HEAD, "/counted", headers, String::new().into());
    assert_eq!(res.status(), 409);
    assert_eq!(RUNS.load(Ordering::SeqCst), 0, "the handler never ran");
    assert_eq!(app.get_alloy("/counted").status(), 200);
    assert_eq!(RUNS.load(Ordering::SeqCst), 1);
}

#[test]
fn a_protocol_relative_request_path_never_leaves_the_site() {
    // A root catch-all route matches `//evil.example/x`; the client resolves `X-Inertia-Location` and the page's
    // `url` against the page's origin, so `//evil.example/x` would send the visitor to another host (S5-02).
    async fn catch_all(uri: smeltery::http::Uri) -> Response {
        if uri.path().ends_with("plain") {
            view(Plain {})
        } else {
            alloy::render("home").into_response()
        }
    }
    let build = |version: &'static str| {
        TestApp::new(move |mut app| {
            app.settings_mut().root =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
            app.alloy(
                Alloy::new()
                    .root::<Root>()
                    .version(move |_| version.to_owned()),
            )
            .routes(|r| {
                r.get("/{*rest}", catch_all);
            })
        })
    };
    let app = build("v2");
    for (path, local) in [
        ("//evil.example/x", "/evil.example/x"),
        ("///evil.example/x?a=1", "/evil.example/x?a=1"),
        ("/\\evil.example/x", "/evil.example/x"),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("x-inertia", HeaderValue::from_static("true"));
        headers.insert("x-inertia-version", HeaderValue::from_static("v1"));
        // The asset-version 409.
        let res = app.request(Method::GET, path, headers.clone(), String::new().into());
        assert_eq!(res.status(), 409, "{path}");
        assert_eq!(res.header("x-inertia-location"), Some(local), "{path}");
        // The page object's `url`, as JSON and in the first visit's HTML.
        headers.insert("x-inertia-version", HeaderValue::from_static("v2"));
        let res = app.request(Method::GET, path, headers.clone(), String::new().into());
        assert_eq!(res.status(), 200, "{path}");
        assert_eq!(res.json()["url"], local, "{path}");
        let res = app.request(Method::GET, path, HeaderMap::new(), String::new().into());
        assert_eq!(res.alloy_page()["url"], local, "{path}");
    }
    // A Mold page's full-page-load 409.
    for (path, local) in [
        ("//evil.example/plain", "/evil.example/plain"),
        ("/\\evil.example/plain", "/evil.example/plain"),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("x-inertia", HeaderValue::from_static("true"));
        headers.insert("x-inertia-version", HeaderValue::from_static("v2"));
        let res = app.request(Method::GET, path, headers, String::new().into());
        assert_eq!(res.status(), 409, "{path}");
        assert_eq!(res.header("x-inertia-location"), Some(local), "{path}");
    }
}

#[test]
fn a_failed_validation_redirects_back_and_the_next_page_has_the_errors() {
    let (app, _) = app();
    let res = inertia(&app, Method::POST, "/store", &[]);
    assert_eq!(res.status(), 303, "{}", res.text());
    assert_eq!(res.header("location"), Some("/form"));
    assert!(
        vary_names_inertia(&res),
        "the web stack's redirect names X-Inertia too"
    );
    app.get_alloy("/home")
        .assert_prop("errors", json!({ "email": "The email field is required." }));
    // Flashed once.
    app.get_alloy("/home").assert_prop("errors", json!({}));

    // With an error bag.
    inertia(&app, Method::POST, "/store", &[]);
    inertia(
        &app,
        Method::GET,
        "/home",
        &[("x-inertia-error-bag", "login")],
    )
    .assert_prop(
        "errors",
        json!({ "login": { "email": "The email field is required." } }),
    );
    // No errors: `{}` even with a bag.
    inertia(
        &app,
        Method::GET,
        "/home",
        &[("x-inertia-error-bag", "login")],
    )
    .assert_prop("errors", json!({}));

    // `all_errors`: arrays.
    let (app, _) = app_with(Alloy::new().root::<Root>().all_errors());
    inertia(&app, Method::POST, "/store", &[]);
    app.get_alloy("/home").assert_prop(
        "errors",
        json!({ "email": ["The email field is required."] }),
    );
}

#[test]
fn post_alloy_sends_a_form_the_way_inertias_client_does() {
    let (app, _) = app();
    // A JSON body with `X-Inertia`: a redirect back with the errors in the session, never 422 JSON.
    let res = app.post_alloy("/store", &json!({ "email": "" }));
    assert_eq!(res.status(), 303, "{}", res.text());
    app.get_alloy("/home")
        .assert_prop("errors", json!({ "email": "The email field is required." }));
}

#[test]
fn redirect_rules_for_inertia_requests() {
    let (app, _) = app();
    // 302 after PUT → 303.
    let res = inertia(&app, Method::PUT, "/put", &[]);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/home"));
    // Without X-Inertia it stays 302.
    let res = app.request(Method::PUT, "/put", HeaderMap::new(), String::new().into());
    assert_eq!(res.status(), 302);
    // A fragment → 409 X-Inertia-Redirect (not for prefetches).
    let res = inertia(&app, Method::GET, "/fragment", &[]);
    assert_eq!(res.status(), 409);
    assert_eq!(res.header("x-inertia-redirect"), Some("/home#section"));
    let res = inertia(&app, Method::GET, "/fragment", &[("purpose", "prefetch")]);
    assert_eq!(res.status(), 303);
    assert_eq!(app.get("/fragment").status(), 303);
    // `alloy::location`: 409 for Inertia, 303 otherwise.
    let res = inertia(&app, Method::GET, "/external", &[]);
    assert_eq!(res.status(), 409);
    assert_eq!(
        res.header("x-inertia-location"),
        Some("https://billing.example.test/portal")
    );
    let res = app.get("/external");
    assert_eq!(res.status(), 303);
    assert_eq!(
        res.header("location"),
        Some("https://billing.example.test/portal")
    );
    // An empty 200 → back.
    let res = inertia(&app, Method::GET, "/nothing", &[]);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/form"));
    assert_eq!(app.get("/nothing").status(), 200);
    // A Mold page → a full page load.
    let res = inertia(&app, Method::GET, "/plain?x=1", &[]);
    assert_eq!(res.status(), 409);
    assert_eq!(res.header("x-inertia-location"), Some("/plain?x=1"));
    assert_eq!(app.get("/plain").text(), "plain\n");
    // Only for GET / HEAD: a Mold page answering an Inertia POST passes through (reloading the POST URL would lose
    // the submitted data), and the client shows it in its modal.
    let res = inertia(&app, Method::POST, "/plain", &[]);
    assert_eq!(res.status(), 200);
    assert_eq!(res.text(), "plain\n");
    assert!(res.header("x-inertia-location").is_none());
}

#[test]
fn flash_values_reach_the_next_page_once_without_internal_keys() {
    let (app, _) = app();
    inertia(&app, Method::POST, "/store", &[]); // flashes `_errors` and `_old_input`
    let res = app.request(
        Method::POST,
        "/flash",
        HeaderMap::new(),
        String::new().into(),
    );
    assert_eq!(res.status(), 303);
    let page = app.get_alloy("/home").alloy_page();
    assert_eq!(
        page["flash"],
        json!({ "status": "Saved.", "user": { "name": "Ada" } })
    );
    assert!(page["flash"].get("_errors").is_none() && page["flash"].get("_old_input").is_none());
    assert!(
        app.get_alloy("/home").alloy_page().get("flash").is_none(),
        "gone on the page after"
    );
}

#[test]
fn clear_history_marks_the_next_page_once() {
    let (app, _) = app();
    let res = app.request(
        Method::POST,
        "/logout",
        HeaderMap::new(),
        String::new().into(),
    );
    assert_eq!(res.status(), 303);
    assert_eq!(app.get_alloy("/home").alloy_page()["clearHistory"], true);
    assert!(
        app.get_alloy("/home")
            .alloy_page()
            .get("clearHistory")
            .is_none()
    );
    let page = app.get_alloy("/encrypted").alloy_page();
    assert_eq!(page["clearHistory"], true);
    assert_eq!(page["encryptHistory"], true);
    let (app, _) = app_with(Alloy::new().root::<Root>().encrypt_history());
    assert_eq!(app.get_alloy("/home").alloy_page()["encryptHistory"], true);
}

#[test]
fn shared_props_merge_under_the_page_and_errors_stay_alloys() {
    let (app, _) = app_with(Alloy::new().root::<Root>().share(shared));
    let page = app.get_alloy("/home").alloy_page();
    assert_eq!(
        page["props"]["app"],
        json!({ "name": "Forge", "path": "/home" })
    );
    assert_eq!(page["props"]["greeting"], "Hello", "the page wins");
    assert_eq!(page["sharedProps"], json!(["app", "greeting"]));
    // A prop named `errors` is a debug-build error.
    let res = app.get_alloy("/errors-prop");
    assert_eq!(res.status(), 500);
}

#[test]
fn prop_kinds_on_full_visits_and_partial_reloads() {
    let (app, counter) = app();
    let page = app.get_alloy("/props").alloy_page();
    assert_eq!(
        page,
        json!({
            "component": "props",
            "props": {
                "user": { "name": "Ada", "email": "ada@example.test", "address": { "city": "X", "zip": "1" } },
                "lazy": "lazy value",
                "posts": [{ "id": 1 }],
                "news": ["n"],
                "settings": { "a": { "b": 1 } },
                "always": true,
                "errors": {},
            },
            "url": "/props",
            "version": "",
            "mergeProps": ["posts"],
            "prependProps": ["news"],
            "deepMergeProps": ["settings"],
            "matchPropsOn": ["posts.id"],
            "deferredProps": { "default": ["deferred"], "side": ["grouped"] },
        })
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "only the lazy prop was computed"
    );

    // A partial reload naming two props: only those (plus always / errors), nothing else computed.
    counter.store(0, Ordering::SeqCst);
    let res = app.reload_alloy("/props", "props", &["optional", "deferred"]);
    assert_eq!(
        res.alloy_page()["props"],
        json!({ "optional": 1, "deferred": [1, 2], "always": true, "errors": {} })
    );
    assert!(
        res.alloy_page().get("deferredProps").is_none(),
        "partial reloads list no deferred props"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);

    // Dot paths narrow inside a value; `except` removes after `only`.
    let res = inertia(
        &app,
        Method::GET,
        "/props",
        &[
            ("x-inertia-partial-component", "props"),
            (
                "x-inertia-partial-data",
                "user.name,user.address,posts,lazy",
            ),
            ("x-inertia-partial-except", "user.address.zip,lazy"),
        ],
    );
    assert_eq!(
        res.alloy_page()["props"],
        json!({ "user": { "name": "Ada", "address": { "city": "X" } }, "posts": [{ "id": 1 }], "always": true, "errors": {} })
    );
    // `except` alone.
    let res = inertia(
        &app,
        Method::GET,
        "/props",
        &[
            ("x-inertia-partial-component", "props"),
            ("x-inertia-partial-except", "user,lazy,settings,news"),
        ],
    );
    let props = res.alloy_page()["props"].clone();
    assert!(props.get("user").is_none() && props.get("lazy").is_none());
    assert!(
        props.get("optional").is_some(),
        "every declared prop that is not excluded"
    );
    // `X-Inertia-Reset` drops the merge metadata of the props it names.
    let res = inertia(
        &app,
        Method::GET,
        "/props",
        &[
            ("x-inertia-partial-component", "props"),
            ("x-inertia-partial-data", "posts,news"),
            ("x-inertia-reset", "posts"),
        ],
    );
    let page = res.alloy_page();
    assert!(page.get("mergeProps").is_none() && page.get("matchPropsOn").is_none());
    assert_eq!(page["prependProps"], json!(["news"]));
    // A partial reload of another component is a full visit.
    let res = app.reload_alloy("/props", "home", &["optional"]);
    let page = res.alloy_page();
    assert!(page["props"].get("optional").is_none());
    assert!(page["props"].get("user").is_some());
    assert!(page.get("deferredProps").is_some());
}

#[test]
fn a_prop_can_never_close_the_script_element() {
    let (app, _) = app();
    let html = app.get("/evil").text();
    assert!(!html.contains("</script><script>"), "{html}");
    assert!(!html.contains("<!--"), "{html}");
    assert_eq!(
        app.get("/evil").prop("bio"),
        Some(Value::from(
            "</script><script>alert(1)</script><!-- \u{2028}\u{2029} &"
        ))
    );
}

#[test]
fn derived_components_and_page_options() {
    let (app, _) = app();
    app.get_alloy("/typed")
        .assert_component("posts/index")
        .assert_prop("titles", ["Hello"]);
    app.get("/typed").assert_component("posts/index");
    let res = app.get_alloy("/broken");
    assert_eq!(res.status(), 500);
    let res = app.get_alloy("/missing");
    assert_eq!(res.status(), 404);
    res.assert_component("errors/404");
    assert_eq!(app.get("/missing").status(), 404);
    assert_eq!(app.get_alloy("/failing-lazy").status(), 500);
}

#[test]
fn a_page_outside_the_web_stack_is_an_error() {
    let (app, _) = app();
    let res = app.get("/api/page");
    assert_eq!(res.status(), 500);
    assert!(
        res.text().contains("Alloy pages need web routes"),
        "{}",
        res.text()
    );
    // An app without `.alloy(…)`.
    let app = TestApp::new(|app| {
        app.routes(|r| {
            r.get("/home", home);
        })
    });
    let res = app.get("/home");
    assert_eq!(res.status(), 500);
    assert!(res.text().contains("`.alloy(…)`"), "{}", res.text());
    assert!(
        !vary_names_inertia(&res),
        "no Vary: X-Inertia without Alloy (A8)"
    );
}

#[test]
fn alloy_turns_on_the_xsrf_cookie() {
    let (app, _) = app();
    app.get("/home");
    assert!(app.cookie("XSRF-TOKEN").is_some());
}

#[test]
fn an_app_without_a_root_template_fails_to_build() {
    let result = std::panic::catch_unwind(|| TestApp::new(|app| app.alloy(Alloy::new())));
    let message = match result {
        Ok(_) => panic!("the build should fail"),
        Err(e) => e.downcast_ref::<String>().cloned().unwrap_or_default(),
    };
    assert!(message.contains("needs a root template"), "{message}");
}

#[test]
fn an_app_with_an_unsafe_build_dir_fails_to_boot() {
    for dir in ["../outside", "build\"><script>", "a//b", ""] {
        let result = std::panic::catch_unwind(|| {
            TestApp::new(|mut app| {
                app.settings_mut().root =
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
                app.alloy(Alloy::new().root::<Root>().build_dir(dir))
            })
        });
        let message = match result {
            Ok(_) => panic!("{dir:?}: the build should fail"),
            Err(e) => e.downcast_ref::<String>().cloned().unwrap_or_default(),
        };
        assert!(
            message.contains("invalid Alloy build directory"),
            "{dir:?}: {message}"
        );
    }
    // A nested folder is fine.
    let app = TestApp::new(|mut app| {
        app.settings_mut().root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        app.alloy(Alloy::new().root::<Root>().build_dir("assets/build"))
            .routes(|r| {
                r.get("/home", home);
            })
    });
    assert_eq!(app.get("/home").status(), 200);
}

/// Headers the handler sets on a page's response survive the protocol: on a first visit (HTML) and on an Inertia
/// visit (JSON); the protocol's own `Content-Type` wins.
#[test]
fn a_page_keeps_the_headers_its_handler_set() {
    let (app, _) = app();
    let first = app.get("/private");
    assert_eq!(first.status(), 200);
    assert_eq!(first.header("cache-control"), Some("no-store"));
    assert!(
        first
            .header("content-type")
            .unwrap_or_default()
            .starts_with("text/html")
    );
    let visit = inertia(&app, Method::GET, "/private", &[]);
    assert_eq!(visit.status(), 200);
    assert_eq!(visit.header("cache-control"), Some("no-store"));
    assert!(
        visit
            .header("content-type")
            .unwrap_or_default()
            .contains("json")
    );
    // Without it, no such header.
    assert_eq!(app.get("/home").header("cache-control"), None);
}

/// Stage C review L1, L2, N1: the protocol's headers win over the handler's, every value of a repeated handler header
/// survives, and headers about the old body or the connection are dropped.
#[test]
fn the_protocol_wins_over_the_handlers_headers() {
    let (app, _) = app();
    let visit = inertia(&app, Method::GET, "/meddling", &[]);
    assert_eq!(visit.status(), 200);
    assert_eq!(visit.header("x-inertia"), Some("true"));
    assert!(
        visit
            .header("content-type")
            .unwrap_or_default()
            .contains("json")
    );
    let vary: Vec<&str> = visit
        .headers()
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    assert!(
        vary.iter()
            .any(|v| v.to_ascii_lowercase().contains("x-inertia")),
        "{vary:?}"
    );
    for name in [
        "connection",
        "etag",
        "last-modified",
        "content-range",
        "content-disposition",
        "keep-alive",
        "upgrade",
        "te",
        "trailer",
    ] {
        assert!(visit.headers().get(name).is_none(), "{name} was kept");
    }
    let length = visit.header("content-length").unwrap_or("absent");
    assert_ne!(length, "3", "the handler's stale length is dropped");
    assert_eq!(visit.json()["component"], "home");
    let cookies: Vec<&str> = visit
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    assert!(cookies.contains(&"first=1; Path=/"), "{cookies:?}");
    assert!(cookies.contains(&"second=2; Path=/"), "{cookies:?}");
    // A first visit: HTML, the same rules.
    let first = app.get("/meddling");
    assert!(
        first
            .header("content-type")
            .unwrap_or_default()
            .starts_with("text/html")
    );
    assert!(first.headers().get(header::CONNECTION).is_none());
}
