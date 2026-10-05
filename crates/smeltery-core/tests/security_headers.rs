//! The default security headers (`SECURITY_HEADERS`, `FRAME_OPTIONS`, `HSTS_MAX_AGE`) on every
//! kind of response, and the rule that a header the app sets itself wins.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use smeltery_core::config::Settings;
use smeltery_core::http::{HeaderValue, Html, IntoResponse, Json, header};
use smeltery_core::middleware::{Next, Request};
use smeltery_core::testing::{TestApp, TestResponse};
use smeltery_core::{AppBuilder, Error, Response, Result};

async fn page() -> Html<&'static str> {
    Html("<h1>page</h1>")
}

async fn api() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(5)).await;
    "late"
}

async fn boom() -> String {
    panic!("boom")
}

async fn fail() -> Result<String> {
    Err(Error::internal("detail"))
}

/// Decides about framing itself, like the Watchfire dashboard.
async fn deny_frames() -> Response {
    ([(header::X_FRAME_OPTIONS, "DENY")], "deny").into_response()
}

/// A page that may be framed by a partner: its own policy, no `X-Frame-Options`.
async fn partner_frames() -> Response {
    (
        [(
            header::CONTENT_SECURITY_POLICY,
            "frame-ancestors https://partner.example",
        )],
        "partner",
    )
        .into_response()
}

async fn own_referrer_policy() -> Response {
    (
        [
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        "own",
    )
        .into_response()
}

async fn own_hsts() -> Response {
    ([(header::STRICT_TRANSPORT_SECURITY, "max-age=60")], "hsts").into_response()
}

/// A global middleware's header counts as the app's own.
async fn global_frame_header(req: Request, next: Next) -> Response {
    let framed = req.uri().path() == "/global";
    let mut res = next.run(req).await;
    if framed {
        res.headers_mut()
            .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    res
}

fn build(app: AppBuilder) -> AppBuilder {
    app.global_middleware(global_frame_header)
        .routes(|r| {
            r.get("/", page);
            r.get("/fail", fail);
            r.get("/slow", slow);
            r.get("/boom", boom);
            r.get("/deny", deny_frames);
            r.get("/partner", partner_frames);
            r.get("/own", own_referrer_policy);
            r.get("/own-hsts", own_hsts);
            r.get("/global", page);
        })
        .api_routes(|r| {
            r.get("/health", api);
        })
}

fn app_with(configure: impl FnOnce(&mut Settings)) -> (TestApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("public")).unwrap();
    std::fs::write(dir.path().join("public/app.css"), "body{}").unwrap();
    std::fs::write(dir.path().join("public/page.html"), "<p>static</p>").unwrap();
    let root = dir.path().to_path_buf();
    let app = TestApp::new(move |mut app| {
        let settings = app.settings_mut();
        settings.root = root;
        settings.url = "http://127.0.0.1:8000".to_owned();
        settings.security_headers = true;
        settings.frame_options = "SAMEORIGIN".to_owned();
        settings.hsts_max_age = Duration::ZERO;
        settings.request_timeout = Duration::from_millis(500);
        configure(settings);
        build(app)
    });
    (app, dir)
}

/// The four headers of a response: `X-Content-Type-Options`, `Referrer-Policy`,
/// `X-Frame-Options`, `Content-Security-Policy`.
fn security(res: &TestResponse) -> [Option<String>; 4] {
    [
        "x-content-type-options",
        "referrer-policy",
        "x-frame-options",
        "content-security-policy",
    ]
    .map(|name| res.header(name).map(str::to_owned))
}

fn defaults(frame: &str, ancestors: &str) -> [Option<String>; 4] {
    [
        Some("nosniff".to_owned()),
        Some("strict-origin-when-cross-origin".to_owned()),
        Some(frame.to_owned()),
        Some(ancestors.to_owned()),
    ]
}

#[test]
fn every_kind_of_response_gets_the_defaults() {
    let (app, _dir) = app_with(|_| {});
    let expected = defaults("SAMEORIGIN", "frame-ancestors 'self'");
    let responses = [
        ("web page", app.get("/")),
        ("API route", app.get_json("/api/health")),
        ("static file", app.get("/app.css")),
        ("static HTML file", app.get("/page.html")),
        ("404 page", app.get("/missing")),
        ("404 JSON", app.get_json("/missing")),
        ("405", app.post_form("/", &[])),
        ("500 page", app.get("/fail")),
        ("panic 500", app.get("/boom")),
        ("408 timeout", app.get("/slow")),
        ("health route", app.get("/up")),
    ];
    for (what, res) in &responses {
        assert_eq!(security(res), expected, "{what} ({})", res.status());
        assert_eq!(res.header("strict-transport-security"), None, "{what}");
    }
    let statuses: Vec<u16> = responses.iter().map(|(_, r)| r.status()).collect();
    assert_eq!(
        statuses,
        [200, 200, 200, 200, 404, 404, 405, 500, 500, 408, 200]
    );
    // Exactly one value each: nothing is appended twice.
    let res = app.get("/");
    for name in [
        "x-content-type-options",
        "x-frame-options",
        "referrer-policy",
    ] {
        assert_eq!(res.headers().get_all(name).iter().count(), 1, "{name}");
    }
}

#[test]
fn headers_the_app_sets_win() {
    let (app, _dir) = app_with(|_| {});
    // `X-Frame-Options` from the handler: its decision stands, no CSP is added beside it.
    let res = app.get("/deny");
    assert_eq!(res.header("x-frame-options"), Some("DENY"));
    assert_eq!(res.header("content-security-policy"), None);
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        res.headers().get_all("x-frame-options").iter().count(),
        1,
        "never a second value"
    );
    // A handler's own CSP is kept untouched; `X-Frame-Options` is still added (browsers prefer
    // the CSP's `frame-ancestors` when both are present).
    let res = app.get("/partner");
    assert_eq!(
        res.header("content-security-policy"),
        Some("frame-ancestors https://partner.example")
    );
    assert_eq!(
        res.headers()
            .get_all("content-security-policy")
            .iter()
            .count(),
        1
    );
    assert_eq!(res.header("x-frame-options"), Some("SAMEORIGIN"));
    // Any other header the handler set.
    let res = app.get("/own");
    assert_eq!(res.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(
        res.headers()
            .get_all("x-content-type-options")
            .iter()
            .count(),
        1
    );
    // A global middleware's header too.
    let res = app.get("/global");
    assert_eq!(res.header("x-frame-options"), Some("DENY"));
    assert_eq!(res.header("content-security-policy"), None);
}

#[test]
fn frame_options_deny_and_off() {
    let (app, _dir) = app_with(|s| s.frame_options = "deny".to_owned());
    assert_eq!(
        security(&app.get("/")),
        defaults("DENY", "frame-ancestors 'none'")
    );

    let (app, _dir) = app_with(|s| s.frame_options = "off".to_owned());
    let res = app.get("/");
    assert_eq!(
        security(&res),
        [
            Some("nosniff".to_owned()),
            Some("strict-origin-when-cross-origin".to_owned()),
            None,
            None
        ]
    );
}

#[test]
fn security_headers_can_be_turned_off() {
    let (app, _dir) = app_with(|s| s.security_headers = false);
    for res in [
        app.get("/"),
        app.get("/app.css"),
        app.get_json("/api/health"),
    ] {
        assert_eq!(security(&res), [None, None, None, None]);
    }
    // The app's own headers are untouched.
    assert_eq!(app.get("/deny").header("x-frame-options"), Some("DENY"));
}

#[tokio::test]
async fn an_unknown_frame_option_stops_the_build() {
    for bad in ["SAMEORGIN", "ALLOW-FROM https://x.example", "", "false"] {
        let mut builder = AppBuilder::new(Settings::from_env());
        builder.settings_mut().frame_options = bad.to_owned();
        let Err(err) = build(builder).build().await else {
            panic!("the build must fail for {bad:?}");
        };
        assert!(
            err.to_string()
                .contains("FRAME_OPTIONS must be SAMEORIGIN, DENY or off"),
            "{err}"
        );
    }
    // With the headers off, the value is not read.
    let mut builder = AppBuilder::new(Settings::from_env());
    builder.settings_mut().security_headers = false;
    builder.settings_mut().frame_options = "nonsense".to_owned();
    assert!(build(builder).build().await.is_ok());
}

#[test]
fn hsts_only_when_asked_for_and_app_url_is_https() {
    let year = Duration::from_secs(31_536_000);
    let (app, _dir) = app_with(|s| {
        s.url = "https://app.example".to_owned();
        s.hsts_max_age = year;
    });
    for res in [
        app.get("/"),
        app.get("/app.css"),
        app.get_json("/api/health"),
    ] {
        assert_eq!(
            res.header("strict-transport-security"),
            Some("max-age=31536000")
        );
    }
    // The app's own value wins.
    assert_eq!(
        app.get("/own-hsts").header("strict-transport-security"),
        Some("max-age=60")
    );

    // Off by default, also on https.
    let (app, _dir) = app_with(|s| s.url = "https://app.example".to_owned());
    assert_eq!(app.get("/").header("strict-transport-security"), None);

    // Never for an http APP_URL: a wrong header could lock visitors out of a site without
    // a certificate.
    let (app, _dir) = app_with(|s| {
        s.url = "http://app.example".to_owned();
        s.hsts_max_age = year;
    });
    assert_eq!(app.get("/").header("strict-transport-security"), None);

    // HSTS is its own setting: `SECURITY_HEADERS=false` leaves it on.
    let (app, _dir) = app_with(|s| {
        s.url = "https://app.example".to_owned();
        s.hsts_max_age = year;
        s.security_headers = false;
    });
    let res = app.get("/");
    assert_eq!(
        res.header("strict-transport-security"),
        Some("max-age=31536000")
    );
    assert_eq!(res.header("x-frame-options"), None);
}

#[test]
fn settings_defaults() {
    let s = Settings::from_env();
    assert!(s.security_headers);
    assert_eq!(s.frame_options, "SAMEORIGIN");
    assert_eq!(s.hsts_max_age, Duration::ZERO);
}

#[test]
fn the_https_check_for_hsts_ignores_case() {
    let (app, _dir) = app_with(|s| {
        s.url = "HTTPS://App.Example".to_owned();
        s.hsts_max_age = Duration::from_secs(600);
    });
    assert_eq!(
        app.get("/").header("strict-transport-security"),
        Some("max-age=600")
    );
}
