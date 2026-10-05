//! Views through the HTTP stack: deferred rendering, the request host, render errors.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use smeltery_core::http::StatusCode;
use smeltery_core::middleware::{Next, Request};
use smeltery_core::testing::TestApp;
use smeltery_core::view::{RequestHost, ViewData, view, view_with_status};
use smeltery_core::{AppBuilder, Response};
use smeltery_mold::Template;
use smeltery_mold_macros::Mold;

const VIEWS: &str = "tests/app/resources/views";

#[derive(Mold)]
#[mold("show", crate = "smeltery_mold", dir = "tests/app/resources/views")]
struct Show {
    title: String,
    id: u32,
}

#[derive(Mold)]
#[mold("broken", crate = "smeltery_mold", dir = "tests/app/resources/views")]
struct Broken {
    a: i32,
    b: i32,
}

async fn show() -> Response {
    view(Show {
        title: "Tom & Jerry".into(),
        id: 7,
    })
}

async fn created() -> Response {
    view_with_status(
        StatusCode::CREATED,
        Show {
            title: "new".into(),
            id: 1,
        },
    )
}

async fn broken() -> Response {
    view(Broken { a: 1, b: 0 })
}

async fn post() {}

/// Stands in for the session middleware of a later milestone.
async fn session(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let mut data = ViewData::default();
    data.csrf_token = Some("tok".into());
    data.authenticated = true;
    res.extensions_mut().insert(data);
    res
}

fn app(debug: bool, with_session: bool) -> TestApp {
    TestApp::new(move |mut b: AppBuilder| {
        b.settings_mut().root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        b.settings_mut().debug = debug;
        let b = b
            .routes(|r| {
                r.get("/show", show);
                r.get("/created", created);
                r.get("/broken", broken);
                r.get("/posts/{post}", post).name("posts.show");
            })
            // API routes run without a session.
            .api_routes(|r| {
                r.get("/show", show);
            });
        if with_session {
            b.global_middleware(session)
        } else {
            b
        }
    })
}

#[test]
fn a_view_renders_with_the_request_host() {
    let app = app(false, true);
    let res = app.get("/show");
    assert_eq!(res.status(), 200);
    assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
    let expected = "<h1>Tom &amp; Jerry</h1>\n<a href=\"/posts/7\">post</a>\n\
                    <input type=\"hidden\" name=\"_token\" value=\"tok\">\nin\n";
    assert_eq!(res.text(), expected);

    // The compiled code gives the same bytes for the same host data.
    let mut data = ViewData::default();
    data.csrf_token = Some("tok".into());
    data.authenticated = true;
    let host = RequestHost::new(app.app().clone(), data);
    let t = Show {
        title: "Tom & Jerry".into(),
        id: 7,
    };
    assert_eq!(t.render_compiled(&host).unwrap(), expected);
    assert_eq!(
        t.render_runtime_with(app.app().views(), &host).unwrap(),
        expected
    );
    assert!(app.app().views().views_dir().ends_with(VIEWS));
    assert_eq!(app.get("/created").status(), 201);
}

#[test]
fn csrf_without_a_session_is_a_render_error() {
    let app = app(false, false);
    let res = app.get("/api/show");
    assert_eq!(res.status(), 500);
    // Web routes have a session, so `@csrf` renders there.
    assert_eq!(app.get("/show").status(), 200);
    assert!(!res.text().contains("show.mold.html"));
}

#[test]
fn render_errors_show_the_template_position_in_debug() {
    let res = app(true, false).get("/broken");
    assert_eq!(res.status(), 500);
    assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
    let text = res.text();
    assert!(text.contains("Mold error"), "{text}");
    assert!(text.contains("broken.mold.html:2:6"), "{text}");
    assert!(text.contains("division by zero"));
}

#[test]
fn render_errors_are_a_plain_500_in_production() {
    let res = app(false, false).get("/broken");
    assert_eq!(res.status(), 500);
    let text = res.text();
    assert!(text.contains("500 Internal Server Error"), "{text}");
    assert!(!text.contains("broken.mold.html"));
    assert!(!text.contains("division"));
}
