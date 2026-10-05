//! CORS for listed origins (`CORS_ALLOWED_ORIGINS`, `CORS_PATHS`): preflights answered before routing, the
//! headers on answers to listed origins only, never credentials, `*` refused, `null` only when listed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use smeltery_core::AppBuilder;
use smeltery_core::config::Settings;
use smeltery_core::http::{HeaderMap, HeaderValue, Json, Method};
use smeltery_core::testing::{TestApp, TestResponse};

async fn api() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

fn build(app: AppBuilder) -> AppBuilder {
    app.api_routes(|r| {
        r.post("/broadcasting/auth", api);
        r.get("/me", api);
    })
    .routes(|r| {
        r.get("/page", || async { "page" });
    })
}

fn app_with(configure: impl FnOnce(&mut Settings) + Send + 'static) -> TestApp {
    TestApp::new(move |mut app| {
        configure(app.settings_mut());
        build(app)
    })
}

fn hybrid() -> TestApp {
    app_with(|s| {
        s.cors_allowed_origins =
            "capacitor://localhost, tauri://localhost, http://tauri.localhost".into();
    })
}

fn send(
    app: &TestApp,
    method: Method,
    path: &str,
    headers: &[(&'static str, &str)],
) -> TestResponse {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.append(*name, HeaderValue::from_str(value).unwrap());
    }
    app.request(method, path, map, axum::body::Body::empty())
}

fn preflight(app: &TestApp, origin: &str, path: &str) -> TestResponse {
    send(
        app,
        Method::OPTIONS,
        path,
        &[
            ("origin", origin),
            ("access-control-request-method", "POST"),
            (
                "access-control-request-headers",
                "authorization, content-type",
            ),
        ],
    )
}

#[test]
fn a_listed_origin_gets_its_preflight_and_its_header() {
    let app = hybrid();
    for origin in [
        "capacitor://localhost",
        "tauri://localhost",
        "http://tauri.localhost",
    ] {
        let res = preflight(&app, origin, "/api/broadcasting/auth");
        assert_eq!(res.status(), 204, "{origin}");
        assert_eq!(res.header("access-control-allow-origin"), Some(origin));
        assert_eq!(res.header("access-control-allow-methods"), Some("POST"));
        assert!(
            res.header("access-control-allow-headers")
                .unwrap()
                .contains("Authorization")
        );
        assert_eq!(
            res.header("access-control-allow-credentials"),
            None,
            "never credentials"
        );
        assert_eq!(res.header("vary"), Some("Origin"));
    }
    let res = send(
        &app,
        Method::GET,
        "/api/me",
        &[("origin", "capacitor://localhost")],
    );
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.header("access-control-allow-origin"),
        Some("capacitor://localhost")
    );
    assert_eq!(res.header("access-control-allow-credentials"), None);
}

#[test]
fn other_origins_paths_and_apps_without_the_setting_get_nothing() {
    let app = hybrid();
    // Another origin, a lookalike, two Origin headers.
    for origin in ["https://evil.example", "capacitor://localhost.evil", "null"] {
        let res = preflight(&app, origin, "/api/broadcasting/auth");
        assert_ne!(res.status(), 204, "{origin}");
        assert_eq!(res.header("access-control-allow-origin"), None, "{origin}");
    }
    let res = send(
        &app,
        Method::GET,
        "/api/me",
        &[
            ("origin", "capacitor://localhost"),
            ("origin", "tauri://localhost"),
        ],
    );
    assert_eq!(res.header("access-control-allow-origin"), None);
    // A path outside CORS_PATHS (a web page).
    let res = send(
        &app,
        Method::GET,
        "/page",
        &[("origin", "capacitor://localhost")],
    );
    assert_eq!(res.header("access-control-allow-origin"), None);
    // Without CORS_ALLOWED_ORIGINS: no CORS at all.
    let plain = app_with(|_| {});
    let res = preflight(&plain, "capacitor://localhost", "/api/broadcasting/auth");
    assert_ne!(res.status(), 204);
    assert_eq!(res.header("access-control-allow-origin"), None);
}

#[test]
fn null_is_allowed_only_when_listed() {
    let app = app_with(|s| s.cors_allowed_origins = "null".into());
    let res = preflight(&app, "null", "/api/broadcasting/auth");
    assert_eq!(res.status(), 204);
    assert_eq!(res.header("access-control-allow-origin"), Some("null"));
}

#[test]
fn wildcards_and_bad_entries_stop_the_build() {
    for (origins, paths) in [
        ("*", "/api/"),
        ("https://*.example.com", "/api/"),
        ("https://app.example.com/path", "/api/"),
        ("capacitor://localhost", "api/"),
    ] {
        let mut settings = Settings::from_env();
        settings.cors_allowed_origins = origins.into();
        settings.cors_paths = paths.into();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let built = runtime.block_on(build(AppBuilder::new(settings)).build());
        let err = built.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err.contains("CORS_"), "{origins} {paths}: {err}");
    }
}

/// Every `Vary` value of an answer, joined.
fn vary(res: &TestResponse) -> String {
    res.headers()
        .get_all("vary")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(", ")
}

/// LC-1: a shared cache must keep answers to different origins apart, the ones without CORS headers included.
#[test]
fn every_answer_on_a_covered_path_varies_by_origin() {
    let app = hybrid();
    for headers in [
        vec![("origin", "https://evil.example")],
        vec![],
        vec![("origin", "capacitor://localhost")],
    ] {
        let res = send(&app, Method::GET, "/api/me", &headers);
        assert_eq!(res.status(), 200);
        assert!(vary(&res).contains("Origin"), "{headers:?}: {}", vary(&res));
    }
    let res = send(
        &app,
        Method::GET,
        "/page",
        &[("origin", "https://evil.example")],
    );
    assert!(!vary(&res).contains("Origin"), "{}", vary(&res));
}

/// LC-2: answers the stack makes itself (the request timeout's 408, a panic's 500) reach a listed origin readable.
#[test]
fn timeouts_and_panics_carry_the_header() {
    let app = TestApp::new(|mut app| {
        app.settings_mut().cors_allowed_origins = "capacitor://localhost".into();
        app.settings_mut().request_timeout = std::time::Duration::from_millis(50);
        app.api_routes(|r| {
            r.get("/slow", || async {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                "late"
            });
            r.get("/panic", || async {
                if true {
                    panic!("boom");
                }
                "never"
            });
        })
    });
    for (path, status) in [("/api/slow", 408), ("/api/panic", 500)] {
        let res = send(
            &app,
            Method::GET,
            path,
            &[("origin", "capacitor://localhost")],
        );
        assert_eq!(res.status(), status, "{path}");
        assert_eq!(
            res.header("access-control-allow-origin"),
            Some("capacitor://localhost"),
            "{path}"
        );
        assert!(vary(&res).contains("Origin"), "{path}");
    }
}
