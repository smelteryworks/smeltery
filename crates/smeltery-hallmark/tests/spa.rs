//! The same-origin SPA mode: first-party requests on `auth:hallmark` API routes are authenticated by the session,
//! with core's CSRF check; everything else stays bearer-only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use smeltery_core::testing::{TestApp, TestResponse};
use smeltery_hallmark::Hallmark;
use support::*;

const APP_URL: &str = "http://example.test";

fn spa_app(hallmark: Hallmark, csrf: bool) -> TestApp {
    let app = TestApp::new(move |b| {
        let mut b = build(hallmark)(b);
        b.settings_mut().url = APP_URL.to_owned();
        b
    });
    let app = if csrf { app.with_csrf() } else { app };
    create_user(&app, "ada@example.com", "secret one");
    app
}

/// The masked CSRF token from `GET /hallmark/csrf-cookie`.
fn xsrf(app: &TestApp) -> String {
    let res = app.get("/hallmark/csrf-cookie");
    assert_eq!(res.status(), 204);
    app.cookie("XSRF-TOKEN").expect("the XSRF-TOKEN cookie")
}

/// Sign Ada in through the web login (sending the CSRF token as an SPA would).
fn sign_in(app: &TestApp) {
    let token = xsrf(app);
    app.with_header("x-xsrf-token", &token);
    let res = app.post_form(
        "/login",
        &[("email", "ada@example.com"), ("password", "secret one")],
    );
    app.without_header("x-xsrf-token");
    assert_eq!(res.text(), "in");
}

fn get_from(app: &TestApp, path: &str, headers: &[(&str, &str)]) -> TestResponse {
    for (k, v) in headers {
        app.with_header(k, v);
    }
    let res = app.get_json(path);
    for (k, _) in headers {
        app.without_header(k);
    }
    res
}

#[test]
fn a_same_origin_request_is_authenticated_by_the_session() {
    let app = spa_app(Hallmark::new().spa(), false);
    sign_in(&app);
    let res = get_from(&app, "/api/me", &[("sec-fetch-site", "same-origin")]);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert!(res.text().starts_with("web:session:"), "{}", res.text());
    // Without Sec-Fetch-Site, an Origin (or Referer) equal to APP_URL's counts.
    let res = get_from(&app, "/api/me", &[("origin", APP_URL)]);
    assert_eq!(res.status(), 200);
    let res = get_from(
        &app,
        "/api/me",
        &[("referer", "http://example.test/dashboard")],
    );
    assert_eq!(res.status(), 200);
}

#[test]
fn a_same_site_sibling_is_not_first_party() {
    let app = spa_app(Hallmark::new().spa(), false);
    sign_in(&app);
    for headers in [
        &[
            ("sec-fetch-site", "same-site"),
            ("origin", "http://api.example.test"),
        ][..],
        &[("origin", "http://api.example.test")],
        &[("sec-fetch-site", "same-site")],
    ] {
        let res = get_from(&app, "/api/me", headers);
        assert_eq!(res.status(), 401, "{headers:?}");
    }
}

#[test]
fn a_foreign_origin_with_a_cookie_is_treated_as_bearer_only() {
    let app = spa_app(Hallmark::new().spa(), false);
    sign_in(&app);
    for headers in [
        &[("origin", "http://evil.test")][..],
        &[
            ("sec-fetch-site", "cross-site"),
            ("origin", "http://evil.test"),
        ],
        &[("origin", "null")],
        &[],
    ] {
        let res = get_from(&app, "/api/me", headers);
        assert_eq!(res.status(), 401, "{headers:?}");
        assert_eq!(res.header("www-authenticate"), Some("Bearer"));
    }
}

#[test]
fn a_stateful_post_without_the_xsrf_header_gets_419() {
    let app = spa_app(Hallmark::new().spa(), true);
    sign_in(&app);
    app.with_header("sec-fetch-site", "same-origin");
    let res = app.post_json("/api/stateful", &serde_json::json!({}));
    assert_eq!(res.status(), 419, "{}", res.text());
    let token = xsrf(&app);
    app.with_header("x-xsrf-token", &token);
    let res = app.post_json("/api/stateful", &serde_json::json!({}));
    assert_eq!(res.status(), 200, "{}", res.text());
    assert!(res.text().starts_with("web:session:"));
}

#[test]
fn listed_origins_are_first_party() {
    let app = spa_app(
        Hallmark::new().spa().stateful(&["http://localhost:5173"]),
        false,
    );
    sign_in(&app);
    assert_eq!(
        get_from(&app, "/api/me", &[("origin", "http://localhost:5173")]).status(),
        200
    );
    assert_eq!(
        get_from(&app, "/api/me", &[("origin", "http://localhost:5174")]).status(),
        401
    );
}

#[test]
fn bearer_tokens_keep_working_in_spa_mode() {
    let app = spa_app(Hallmark::new().spa(), false);
    let ada = app
        .block_on(smeltery_core::auth::find_by_email::<User>(
            app.app(),
            "ada@example.com",
        ))
        .unwrap()
        .unwrap();
    let token = create(&app, &ada, &["*"]);
    let auth = format!("Bearer {}", token.plain_text());
    // Cross-site with a token: bearer-only, the token decides.
    assert_eq!(
        get_from(
            &app,
            "/api/me",
            &[("authorization", &auth), ("origin", "http://evil.test")]
        )
        .status(),
        200
    );
    // First-party without a session: the guards run inside the web stack.
    assert_eq!(
        get_from(
            &app,
            "/api/me",
            &[("authorization", &auth), ("sec-fetch-site", "same-origin")]
        )
        .status(),
        200
    );
}

#[test]
fn without_spa_mode_api_routes_never_read_the_session() {
    let app = spa_app(Hallmark::new(), false);
    // No CSRF cookie route, and a signed-in session never reaches API routes.
    assert_eq!(app.get("/hallmark/csrf-cookie").status(), 404);
    let res = app.post_form(
        "/login",
        &[("email", "ada@example.com"), ("password", "secret one")],
    );
    assert_eq!(res.text(), "in");
    for headers in [
        &[("sec-fetch-site", "same-origin")][..],
        &[("origin", APP_URL)],
    ] {
        assert_eq!(
            get_from(&app, "/api/me", headers).status(),
            401,
            "{headers:?}"
        );
    }
}

#[test]
fn bad_stateful_origins_stop_the_app_at_boot() {
    for bad in ["*", "null", "http://x.test/path"] {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let built = rt.block_on(
            build(Hallmark::new().spa().stateful(&[bad]))(smeltery_core::AppBuilder::new(
                smeltery_core::config::Settings::from_env(),
            ))
            .build(),
        );
        let err = built.err().unwrap().to_string();
        assert!(err.contains("HALLMARK_STATEFUL"), "{bad}: {err}");
    }
}

/// A bearer call without browser signals (no `Sec-Fetch-Site`, `Origin` or `Referer`) is never first-party: the web
/// stack does not run, so no session is stored and no cookie is set.
#[test]
fn a_bearer_call_without_browser_signals_never_runs_the_web_stack() {
    let app = spa_app(Hallmark::new().spa(), false);
    let ada = app
        .block_on(smeltery_core::auth::find_by_email::<User>(
            app.app(),
            "ada@example.com",
        ))
        .unwrap()
        .unwrap();
    let token = create(&app, &ada, &["*"]);
    let res = get_from(
        &app,
        "/api/me",
        &[("authorization", &format!("Bearer {}", token.plain_text()))],
    );
    assert_eq!(res.status(), 200);
    assert!(res.text().starts_with("hallmark:token:"), "{}", res.text());
    assert!(
        res.headers().get_all("set-cookie").iter().next().is_none(),
        "{:?}",
        res.headers()
    );
    let db = app.db();
    let sessions = app
        .block_on(db.query_with("SELECT COUNT(*) AS n FROM sessions", []))
        .map(|rows| rows[0].try_get::<i64>("", "n").unwrap())
        .unwrap_or(0);
    assert_eq!(sessions, 0);
}

/// Review M-B: browsers send `Sec-Fetch-Site` on every fetch; a page on a listed origin (a Vite dev server on
/// another port) is `same-site`, and passes only with its listed `Origin`.
#[test]
fn listed_origins_work_with_browser_fetch_metadata() {
    let app = spa_app(
        Hallmark::new().spa().stateful(&["http://localhost:5173"]),
        false,
    );
    sign_in(&app);
    let cases: [(&[(&str, &str)], u16); 6] = [
        (
            &[
                ("sec-fetch-site", "same-site"),
                ("origin", "http://localhost:5173"),
            ],
            200,
        ),
        (
            &[
                ("sec-fetch-site", "cross-site"),
                ("origin", "http://localhost:5173"),
            ],
            200,
        ),
        (
            &[
                ("sec-fetch-site", "same-site"),
                ("origin", "http://localhost:5174"),
            ],
            401,
        ),
        (
            &[
                ("sec-fetch-site", "cross-site"),
                ("origin", "http://evil.test"),
            ],
            401,
        ),
        // APP_URL's own origin is same-origin in a browser; claiming same-site with it is refused.
        (&[("sec-fetch-site", "same-site"), ("origin", APP_URL)], 401),
        (&[("sec-fetch-site", "same-site")], 401),
    ];
    for (headers, status) in cases {
        assert_eq!(
            get_from(&app, "/api/me", headers).status(),
            status,
            "{headers:?}"
        );
    }
}

/// Review L-B: on a first-party request the CSRF check runs before the guards, so a token alone does not skip it.
#[test]
fn a_first_party_bearer_post_still_needs_the_xsrf_header() {
    let app = spa_app(Hallmark::new().spa(), true);
    let ada = app
        .block_on(smeltery_core::auth::find_by_email::<User>(
            app.app(),
            "ada@example.com",
        ))
        .unwrap()
        .unwrap();
    let token = create(&app, &ada, &["*"]);
    app.with_bearer(token.plain_text());
    app.with_header("sec-fetch-site", "same-origin");
    let res = app.post_json("/api/stateful", &serde_json::json!({}));
    assert_eq!(res.status(), 419, "{}", res.text());
    let xsrf = xsrf(&app);
    app.with_header("x-xsrf-token", &xsrf);
    let res = app.post_json("/api/stateful", &serde_json::json!({}));
    assert_eq!(res.status(), 200, "{}", res.text());
    assert!(res.text().starts_with("hallmark:token:"), "{}", res.text());
}
