//! The JSON API, access control and the dashboard through `TestApp`; the SSE stream, the
//! console commands and the headless API against servers on 127.0.0.1:0.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use axum::body::Body;
use http::{HeaderMap, HeaderValue, Method};
use serde::{Deserialize, Serialize};
use smeltery_core::AppBuilder;
use smeltery_core::config::Settings;
use smeltery_core::console::dispatch;
use smeltery_core::testing::{TestApp, TestResponse};
use smeltery_watchfire::prelude::*;
use smeltery_watchfire::web::{Access, api_token};
use smeltery_watchfire::{Agents, WatchfireSettings};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

static DONE: tokio::sync::Notify = tokio::sync::Notify::const_new();

const KEY: &str = "watchfire-test-key-0123456789abcdef";

#[derive(Serialize, Deserialize)]
struct Doomed;

impl Job for Doomed {
    const NAME: &'static str = "doomed";

    fn max_attempts(&self) -> u32 {
        1
    }

    async fn handle(&self, _ctx: JobCtx) -> Result<(), AgentError> {
        Err(AgentError::msg("never works"))
    }
}

fn register(w: &mut Watchfire) {
    w.run("worker", |ctx| async move {
        ctx.log().info("working");
        ctx.cancelled().await;
        Ok(())
    });
    w.run("idle", |ctx| async move {
        ctx.cancelled().await;
        Ok(())
    })
    .autostart(false);
    w.pool(2, "fetcher", |i| {
        agent_fn(format!("f{i}"), |ctx: AgentCtx| async move {
            ctx.cancelled().await;
            Ok(())
        })
    });
    w.job::<Doomed>();
    w.schedule()
        .call("cleanup", |_ctx| async move { Ok(()) })
        .every(5.mins());
    // User 1 is the admin.
    w.dashboard_gate(|auth, _app| async move { Ok(auth.id() == Some(1)) });
}

/// `APP_ENV=local` (TestApp forces `testing`): local development.
fn local_builder(b: AppBuilder) -> AppBuilder {
    let mut b = builder(b);
    b.settings_mut().env = "local".to_owned();
    b
}

fn header(name: &'static str, value: &'static str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(name, HeaderValue::from_static(value));
    h
}

fn builder(b: AppBuilder) -> AppBuilder {
    let mut b = b;
    b.settings_mut().key = KEY.to_owned();
    b.settings_mut().name = "Demo".to_owned();
    b.migrations(|m| {
        m.add(WatchfireTables);
    })
    .agents(register)
}

struct WatchfireTables;

impl smeltery_core::db::migration::Migration for WatchfireTables {
    fn name(&self) -> &'static str {
        "2026_10_03_000000_create_watchfire_tables"
    }

    async fn up(&self, schema: &smeltery_core::db::migration::Schema) -> smeltery_core::Result<()> {
        smeltery_watchfire::migrations::up(schema).await
    }

    async fn down(
        &self,
        schema: &smeltery_core::db::migration::Schema,
    ) -> smeltery_core::Result<()> {
        smeltery_watchfire::migrations::down(schema).await
    }
}

fn local() -> SocketAddr {
    "127.0.0.1:50000".parse().unwrap()
}

fn remote() -> SocketAddr {
    "203.0.113.9:50000".parse().unwrap()
}

fn bearer() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", api_token(KEY).unwrap())).unwrap(),
    );
    h
}

fn call(t: &TestApp, method: Method, path: &str, headers: HeaderMap) -> TestResponse {
    t.request(method, path, headers, Body::empty())
}

fn wait_until(t: &TestApp, mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        t.block_on(async { tokio::time::sleep(Duration::from_millis(10)).await });
    }
    panic!("condition not met");
}

#[test]
fn outside_local_development_the_api_needs_the_token_even_from_loopback() {
    // A same-host reverse proxy makes every request a loopback one: without `APP_ENV=local` that opens nothing.
    for env in ["production", "testing", "staging"] {
        let t = TestApp::new(|b| {
            let mut b = builder(b);
            b.settings_mut().env = env.to_owned();
            b
        })
        .with_agents();
        t.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
        t.from_addr(local());
        let res = t.post_json(
            "/_watchfire/api/agents/worker/pause",
            &serde_json::json!({}),
        );
        assert_eq!(res.status(), 401, "{env}");
        assert_eq!(t.get("/_watchfire/api/agents").status(), 401, "{env}");
        let agents = t.app().service::<Agents>().unwrap();
        assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);
        // The token still works.
        let mut h = bearer();
        h.insert("content-type", HeaderValue::from_static("application/json"));
        let res = t.request(
            Method::POST,
            "/_watchfire/api/agents/worker/pause",
            h,
            Body::from("{}"),
        );
        assert_eq!(res.status(), 200, "{env}");
    }
}

#[test]
fn a_public_app_url_closes_the_local_rule_behind_a_plain_proxy() {
    // nginx's plain `proxy_pass` adds no forwarding header: every request is a header-less loopback one. A server
    // that left APP_ENV at `local` but set its public APP_URL still asks for the token and a sign-in.
    let t = TestApp::new(|b| {
        let mut b = local_builder(b);
        b.settings_mut().url = "https://example.com".to_owned();
        b
    })
    .with_agents();
    t.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
    t.from_addr(local());
    assert_eq!(t.get("/_watchfire/api/agents").status(), 401);
    let res = t.post_json(
        "/_watchfire/api/agents/worker/pause",
        &serde_json::json!({}),
    );
    assert_eq!(res.status(), 401);
    let res = t.get("/_watchfire");
    assert_eq!(
        (res.status(), res.header("location")),
        (303, Some("/login"))
    );
    let agents = t.app().service::<Agents>().unwrap();
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);
}

#[test]
fn in_local_development_proxied_requests_need_the_token() {
    let t = TestApp::new(local_builder).with_agents();
    t.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
    t.from_addr(local());
    assert_eq!(t.get("/_watchfire/api/agents").status(), 200);
    for (name, value) in [
        ("x-forwarded-for", "203.0.113.9"),
        ("forwarded", "for=203.0.113.9"),
        ("x-real-ip", "203.0.113.9"),
        ("x-forwarded-host", "example.com"),
        ("x-forwarded-proto", "https"),
        ("via", "1.1 proxy"),
        ("cf-connecting-ip", "203.0.113.9"),
        ("true-client-ip", "203.0.113.9"),
    ] {
        let res = call(
            &t,
            Method::GET,
            "/_watchfire/api/agents",
            header(name, value),
        );
        assert_eq!(res.status(), 401, "{name}");
        let res = t.request(
            Method::POST,
            "/_watchfire/api/agents/worker/pause",
            header(name, value),
            Body::empty(),
        );
        assert_eq!(res.status(), 401, "{name}");
        // The dashboard treats it as a guest.
        let res = call(&t, Method::GET, "/_watchfire", header(name, value));
        assert_eq!(
            (res.status(), res.header("location")),
            (303, Some("/login")),
            "{name}"
        );
    }
    let agents = t.app().service::<Agents>().unwrap();
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);
    // With the loopback address in `TRUSTED_PROXIES`, every request from it came through the proxy: none is local,
    // even one that names a loopback client.
    let trusted = TestApp::new(|b| {
        let mut b = local_builder(b);
        b.settings_mut().trusted_proxies = "127.0.0.1".to_owned();
        b
    })
    .with_agents();
    trusted.from_addr(local());
    assert_eq!(trusted.get("/_watchfire/api/agents").status(), 401);
    let res = call(
        &trusted,
        Method::GET,
        "/_watchfire/api/agents",
        header("x-forwarded-for", "127.0.0.1"),
    );
    assert_eq!(res.status(), 401);
    // `auth` mode: no shortcut at all.
    let mut settings = WatchfireSettings::from_env();
    settings.dashboard = Access::Auth;
    t.app().insert_service(settings);
    assert_eq!(t.get("/_watchfire/api/agents").status(), 401);
    assert_eq!(t.get("/_watchfire").status(), 303);
}

fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (name, value) in pairs {
        h.insert(*name, HeaderValue::from_static(value));
    }
    h
}

/// S4-01: the token-less local rule is not ambient authority for other sites. A cross-site page cannot post to the
/// API or the dashboard (blind CSRF), and a DNS-rebinding page (an attacker's host name resolving to 127.0.0.1)
/// cannot read or control anything; same-origin requests, curl and links still work.
#[test]
fn the_local_rule_refuses_other_sites_and_foreign_hosts() {
    let t = TestApp::new(local_builder).with_agents();
    t.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
    t.from_addr(local());
    let agents = t.app().service::<Agents>().unwrap();

    // Cross-site writes: refused, the agent keeps running.
    for pairs in [
        &[("origin", "https://evil.example")][..],
        &[("origin", "null")][..],
        &[("sec-fetch-site", "cross-site")][..],
        &[("sec-fetch-site", "same-site")][..],
        &[
            ("origin", "https://evil.example"),
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-mode", "navigate"),
        ][..],
        &[
            ("host", "127.0.0.1:8000"),
            ("origin", "http://localhost.evil.example:8000"),
        ][..],
        // Another dev server on this machine is another site (R-1): host and port must match.
        &[
            ("host", "127.0.0.1:8000"),
            ("origin", "http://localhost:3000"),
        ][..],
        &[
            ("host", "127.0.0.1:8000"),
            ("origin", "http://127.0.0.1:3000"),
        ][..],
    ] {
        let res = t.request(
            Method::POST,
            "/_watchfire/api/agents/worker/stop",
            headers(pairs),
            Body::empty(),
        );
        assert_eq!(res.status(), 401, "{pairs:?}");
        let res = t.request(
            Method::POST,
            "/_watchfire/api/jobs/dead/1/retry",
            headers(pairs),
            Body::empty(),
        );
        assert_eq!(res.status(), 401, "{pairs:?}");
        // Cross-site reads through fetch are refused too.
        let res = call(&t, Method::GET, "/_watchfire/api/agents", headers(pairs));
        assert_eq!(res.status(), 401, "{pairs:?}");
    }
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);

    // DNS rebinding: the attacker's host name, however same-origin the browser thinks it is.
    for host in [
        "attacker.example:8000",
        "attacker.example",
        "127.0.0.1.attacker.example",
        "localhost.attacker.example:8000",
    ] {
        let h = headers(&[("host", host)]);
        assert_eq!(
            call(&t, Method::GET, "/_watchfire/api/agents", h.clone()).status(),
            401,
            "{host}"
        );
        let res = t.request(
            Method::POST,
            "/_watchfire/api/agents/worker/stop",
            h.clone(),
            Body::empty(),
        );
        assert_eq!(res.status(), 401, "{host}");
        // The dashboard treats it as a guest.
        let res = call(&t, Method::GET, "/_watchfire", h);
        assert_eq!(
            (res.status(), res.header("location")),
            (303, Some("/login")),
            "{host}"
        );
    }
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);

    // This machine, same origin, curl (no browser headers) and a followed link: admitted.
    for pairs in [
        &[][..],
        &[("host", "127.0.0.1:8000")][..],
        &[("host", "localhost:8000")][..],
        &[("host", "app.localhost")][..],
        &[("host", "[::1]:8000")][..],
        &[
            ("host", "127.0.0.1:8000"),
            ("origin", "http://127.0.0.1:8000"),
            ("sec-fetch-site", "same-origin"),
        ][..],
        &[("sec-fetch-site", "none")][..],
    ] {
        let res = call(&t, Method::GET, "/_watchfire/api/agents", headers(pairs));
        assert_eq!(res.status(), 200, "{pairs:?}");
        assert_eq!(
            call(&t, Method::GET, "/_watchfire", headers(pairs)).status(),
            200,
            "{pairs:?}"
        );
    }
    let link = headers(&[
        ("host", "127.0.0.1:8000"),
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-mode", "navigate"),
    ]);
    assert_eq!(call(&t, Method::GET, "/_watchfire", link).status(), 200);
    let res = t.request(
        Method::POST,
        "/_watchfire/api/agents/worker/pause",
        headers(&[
            ("host", "localhost:8000"),
            ("origin", "http://localhost:8000"),
            ("sec-fetch-site", "same-origin"),
        ]),
        Body::empty(),
    );
    assert_eq!(res.status(), 200);
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Paused);

    // Two `Origin` headers, or an absolute-form target whose authority disagrees with `Host` (R-2): refused.
    let mut two = headers(&[("host", "127.0.0.1:8000")]);
    two.append("origin", HeaderValue::from_static("http://127.0.0.1:8000"));
    two.append("origin", HeaderValue::from_static("https://evil.example"));
    let res = t.request(
        Method::POST,
        "/_watchfire/api/agents/worker/resume",
        two,
        Body::empty(),
    );
    assert_eq!(res.status(), 401);
    let res = t.request(
        Method::GET,
        "http://attacker.example/_watchfire/api/agents",
        headers(&[("host", "127.0.0.1:8000")]),
        Body::empty(),
    );
    assert_eq!(res.status(), 401);
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Paused);

    // The token works from anywhere, whatever the headers.
    let mut h = bearer();
    h.insert("host", HeaderValue::from_static("attacker.example"));
    h.insert("origin", HeaderValue::from_static("https://evil.example"));
    assert_eq!(
        call(&t, Method::GET, "/_watchfire/api/agents", h).status(),
        200
    );
}

/// The `APP_URL` host counts as this machine (e.g. `http://myapp.localhost:8000` or `http://127.0.0.2`).
#[test]
fn the_app_url_host_is_local() {
    let t = TestApp::new(|b| {
        let mut b = local_builder(b);
        b.settings_mut().url = "http://127.0.0.2:8000".to_owned();
        b
    })
    .with_agents();
    t.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
    t.from_addr(local());
    let h = headers(&[
        ("host", "127.0.0.2:8000"),
        ("origin", "http://127.0.0.2:8000"),
    ]);
    assert_eq!(
        call(&t, Method::GET, "/_watchfire/api/agents", h).status(),
        200
    );
}

/// Core's key rule (D-344): the public test key needs `APP_ENV=testing` AND an empty `APP_KEY`. A short or malformed
/// key in `testing` is no key: the dashboard (sessions) is left out instead of running on the public test key, and
/// no API token exists for it.
#[tokio::test]
async fn a_bad_app_key_never_falls_back_to_the_test_key() {
    let dashboard_mounted = |key: &str| {
        let key = key.to_owned();
        async move {
            // The settings before `.agents(…)`, which mounts the routes.
            let mut settings = Settings::from_env();
            settings.env = "testing".to_owned();
            settings.key = key;
            let app = AppBuilder::new(settings)
                .agents(register)
                .build()
                .await
                .unwrap()
                .app;
            app.routes().iter().any(|r| r.path == "/_watchfire")
        }
    };
    assert!(!dashboard_mounted("short").await);
    assert!(!dashboard_mounted("base64:not-base64!!").await);
    assert!(dashboard_mounted("").await, "the test key: TestApp's case");
    assert!(dashboard_mounted(KEY).await);
    assert!(api_token("").is_none());
    assert!(api_token("short").is_none());
}

#[test]
fn api_endpoints_status_codes_and_access() {
    let t = TestApp::new(local_builder).with_agents();
    t.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });

    // No address, no token: refused.
    let res = t.get("/_watchfire/api/agents");
    assert_eq!(res.status(), 401);
    assert!(res.json()["error"].as_str().unwrap().contains("Bearer"));
    // A remote client needs the token; a wrong one is refused.
    t.from_addr(remote());
    assert_eq!(t.get("/_watchfire/api/agents").status(), 401);
    let mut wrong = HeaderMap::new();
    wrong.insert("authorization", HeaderValue::from_static("Bearer nope"));
    let res = call(&t, Method::GET, "/_watchfire/api/agents", wrong);
    assert_eq!(res.status(), 401);
    assert_eq!(res.json()["error"], "invalid API token");
    let res = call(&t, Method::GET, "/_watchfire/api/agents", bearer());
    assert_eq!(res.status(), 200);
    let names: Vec<String> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "fetcher#0",
            "fetcher#1",
            "idle",
            "queue#0",
            "queue#1",
            "scheduler",
            "worker"
        ]
    );

    // In local development a loopback client needs no token.
    t.from_addr(local());
    let detail = t.get("/_watchfire/api/agents/worker").json();
    assert_eq!(detail["status"]["state"], "running");
    assert_eq!(detail["runs"][0]["outcome"], "running");
    assert_eq!(
        t.get("/_watchfire/api/agents/fetcher%230").json()["status"]["name"],
        "fetcher#0"
    );
    let res = t.get("/_watchfire/api/agents/ghost");
    assert_eq!(
        (res.status(), res.json()["error"].as_str().unwrap()),
        (404, "unknown agent \"ghost\"")
    );
    assert_eq!(
        t.get("/_watchfire/api/agents/worker/logs").json()[0]["message"],
        "working"
    );

    // Commands: 200 with the status, 409 for the wrong state, 404 for an unknown action.
    let res = t.post_json("/_watchfire/api/agents/worker/stop", &serde_json::json!({}));
    assert_eq!(
        (res.status(), res.json()["state"].clone()),
        (200, "stopped".into())
    );
    assert_eq!(
        t.post_json("/_watchfire/api/agents/worker/stop", &serde_json::json!({}))
            .status(),
        409
    );
    assert_eq!(
        t.post_json(
            "/_watchfire/api/agents/worker/resume",
            &serde_json::json!({})
        )
        .status(),
        409
    );
    assert_eq!(
        t.post_json(
            "/_watchfire/api/agents/worker/explode",
            &serde_json::json!({})
        )
        .status(),
        404
    );
    assert_eq!(
        t.post_json("/_watchfire/api/agents/idle/start", &serde_json::json!({}))
            .status(),
        200
    );
    assert_eq!(
        t.post_json("/_watchfire/api/agents/idle/start", &serde_json::json!({}))
            .status(),
        409
    );
    assert_eq!(
        t.post_json("/_watchfire/api/agents/idle/pause", &serde_json::json!({}))
            .json()["state"],
        "paused"
    );
    assert_eq!(
        t.post_json("/_watchfire/api/agents/idle/resume", &serde_json::json!({}))
            .status(),
        200
    );
    assert_eq!(
        t.post_json(
            "/_watchfire/api/agents/idle/restart",
            &serde_json::json!({})
        )
        .status(),
        200
    );

    let runs = t.get("/_watchfire/api/runs?agent=worker&limit=1").json();
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["outcome"], "stopped");
    assert!(
        t.get("/_watchfire/api/runs")
            .json()
            .as_array()
            .unwrap()
            .len()
            > 2
    );

    let schedule = t.get("/_watchfire/api/schedule").json();
    assert_eq!(schedule[0]["name"], "cleanup");
    assert_eq!(schedule[0]["expression"], "every 5m");

    // Jobs: a doomed job lands in the dead letters; retry it, then delete it.
    t.block_on(Doomed.dispatch(t.app())).unwrap();
    wait_until(&t, || t.get("/_watchfire/api/jobs").json()["dead"] == 1);
    let jobs = t.get("/_watchfire/api/jobs").json();
    assert_eq!(jobs["driver"], "database");
    let id = jobs["dead_letters"][0]["id"].as_i64().unwrap();
    assert_eq!(jobs["dead_letters"][0]["error"], "never works");
    let res = t.post_json(
        &format!("/_watchfire/api/jobs/dead/{id}/retry"),
        &serde_json::json!({}),
    );
    assert_eq!(res.status(), 200);
    assert!(res.json()["job_id"].is_i64());
    assert_eq!(
        t.post_json(
            &format!("/_watchfire/api/jobs/dead/{id}/retry"),
            &serde_json::json!({})
        )
        .status(),
        404
    );
    wait_until(&t, || t.get("/_watchfire/api/jobs").json()["dead"] == 1);
    let id = t.get("/_watchfire/api/jobs").json()["dead_letters"][0]["id"]
        .as_i64()
        .unwrap();
    let path = format!("/_watchfire/api/jobs/dead/{id}");
    assert_eq!(
        call(&t, Method::DELETE, &path, HeaderMap::new()).status(),
        200
    );
    assert_eq!(
        call(&t, Method::DELETE, &path, HeaderMap::new()).status(),
        404
    );
    assert_eq!(t.get("/_watchfire/api/jobs").json()["dead"], 0);

    // `off`: no token, no entry, even from loopback; the token still works.
    let mut settings = WatchfireSettings::from_env();
    settings.dashboard = Access::Off;
    t.app().insert_service(settings);
    assert_eq!(t.get("/_watchfire/api/agents").status(), 401);
    assert_eq!(
        call(&t, Method::GET, "/_watchfire/api/agents", bearer()).status(),
        200
    );
    assert_eq!(t.get("/_watchfire").status(), 404);
}

#[test]
fn api_without_running_watchfire_or_its_tables_is_503() {
    // No Watchfire in this process, and no Watchfire tables to read other processes' agents from.
    let t = TestApp::new(|b| {
        let mut b = b;
        b.settings_mut().key = KEY.to_owned();
        b.agents(register)
    });
    let res = call(&t, Method::GET, "/_watchfire/api/agents", bearer());
    assert_eq!(res.status(), 503);
    assert_eq!(
        res.json()["error"],
        "Watchfire is not running in this process"
    );
    // The dashboard says so (the gate works without Watchfire running in the process).
    t.acting_as(1);
    assert!(
        t.get("/_watchfire")
            .text()
            .contains("Watchfire is not running in this process")
    );
}

#[test]
fn without_watchfire_here_the_shared_tables_show_the_agents() {
    let t = TestApp::new(builder);
    let res = call(&t, Method::GET, "/_watchfire/api/agents", bearer());
    assert_eq!(res.status(), 200);
    let list = res.json();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    // The registered singleton agents (queue workers and the scheduler run in every process: not listed).
    assert_eq!(names, ["fetcher#0", "fetcher#1", "idle", "worker"]);
    assert!(list[0]["held_by"].is_null());
    // Without a shared lock store no process takes commands from here.
    let res = call(
        &t,
        Method::POST,
        "/_watchfire/api/agents/worker/stop",
        bearer(),
    );
    assert_eq!(res.status(), 503);
    // Log lines live in the process that runs an agent.
    assert_eq!(
        call(
            &t,
            Method::GET,
            "/_watchfire/api/agents/worker/logs",
            bearer()
        )
        .status(),
        503
    );
    let schedule = call(&t, Method::GET, "/_watchfire/api/schedule", bearer()).json();
    assert_eq!(schedule[0]["name"], "cleanup");
    // The queue exists without the agents.
    assert_eq!(
        call(&t, Method::GET, "/_watchfire/api/jobs", bearer()).json()["pending"],
        0
    );
    t.acting_as(1);
    let html = t.get("/_watchfire").text();
    assert!(
        html.contains("Watchfire does not run in this process"),
        "{html}"
    );
    assert!(html.contains("no process holds it"), "{html}");
}

fn token_from(html: &str) -> String {
    let at = html.find(r#"name="_token" value=""#).unwrap() + r#"name="_token" value=""#.len();
    html[at..].split('"').next().unwrap().to_owned()
}

#[test]
fn the_dashboard_brings_its_own_stylesheet() {
    let t = TestApp::new(builder).with_agents();
    t.acting_as(1);
    let html = t.get("/_watchfire").text();
    let at = html
        .find(r#"<link rel="stylesheet" href=""#)
        .expect("a stylesheet link")
        + r#"<link rel="stylesheet" href=""#.len();
    let href = html[at..].split('"').next().unwrap().to_owned();
    assert!(
        href.starts_with("/_watchfire/assets/watchfire.css?v="),
        "{href}"
    );
    // Anyone may fetch it (it holds no data), without a session cookie, cached for a year.
    t.clear_cookies();
    let res = t.get(&href);
    assert_eq!(res.status(), 200);
    assert_eq!(res.header("content-type"), Some("text/css; charset=utf-8"));
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert!(res.header("set-cookie").is_none());
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    let css = res.text();
    assert!(css.contains("--wf-molten-700") && css.contains("prefers-color-scheme: dark"));
    // `off`: no dashboard, no stylesheet.
    let mut settings = WatchfireSettings::from_env();
    settings.dashboard = Access::Off;
    t.app().insert_service(settings);
    assert_eq!(t.get(&href).status(), 404);
}

#[test]
fn dashboard_renders_and_its_forms_go_through_csrf_with_a_flash() {
    let t = TestApp::new(builder).with_agents().with_csrf();
    t.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
    // Outside local development: guests go to the login page (JSON clients get 401), wherever they come from.
    for addr in [remote(), local()] {
        t.from_addr(addr);
        let res = t.get("/_watchfire");
        assert_eq!(
            (res.status(), res.header("location")),
            (303, Some("/login"))
        );
        let res = call(
            &t,
            Method::GET,
            "/_watchfire",
            header("accept", "application/json"),
        );
        assert_eq!(res.status(), 401);
        // A form post without the session's CSRF token stops at CSRF.
        assert_eq!(
            t.post_form("/_watchfire/agents/worker/stop", &[]).status(),
            419
        );
    }
    // A signed-in user the gate refuses: 403, for the page and the forms.
    t.acting_as(2);
    assert_eq!(t.get("/_watchfire").status(), 403);
    t.acting_as(1);
    let page = t.get("/_watchfire");
    assert_eq!(page.status(), 200);
    assert_eq!(
        page.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    let html = page.text();
    assert!(html.contains(r#"<meta http-equiv="refresh" content="5">"#));
    assert!(html.contains("Demo"));
    for name in [
        "worker",
        "idle",
        "fetcher#0",
        "queue#1",
        "scheduler",
        "cleanup",
        "every 5m",
    ] {
        assert!(html.contains(name), "{name} missing");
    }
    assert!(html.contains(r#"href="/_watchfire?agent=fetcher%230&amp;confirm=stop#wf-confirm""#));
    assert!(!html.contains("<script"));
    // Stop asks first, without JavaScript: the confirmation page holds the form and does not reload itself.
    let confirm = t.get("/_watchfire?agent=fetcher%230&confirm=stop").text();
    assert!(confirm.contains(r#"action="/_watchfire/agents/fetcher%230/stop""#));
    assert!(confirm.contains(r#"<div class="wf-confirm-box" id="wf-confirm" tabindex="-1""#));
    assert!(!confirm.contains("http-equiv=\"refresh\""));
    assert!(confirm.contains(r#"href="/_watchfire" wire:click.prevent="cancel""#));
    // No other site may frame the page or the confirmation.
    for path in ["/_watchfire", "/_watchfire?agent=fetcher%230&confirm=stop"] {
        let res = t.get(path);
        assert_eq!(res.header("x-frame-options"), Some("DENY"), "{path}");
        assert_eq!(
            res.header("content-security-policy"),
            Some("frame-ancestors 'none'"),
            "{path}"
        );
    }
    for ignored in [
        "/_watchfire?agent=fetcher%230&confirm=pause",
        "/_watchfire?agent=ghost&confirm=stop",
        "/_watchfire?confirm=stop",
        // A query that does not decode is ignored too.
        "/_watchfire?agent=a&agent=b&confirm=stop",
        "/_watchfire?agent=%ZZ&confirm=stop",
    ] {
        let res = t.get(ignored);
        assert_eq!(res.status(), 200, "{ignored}");
        let html = res.text();
        assert!(!html.contains("wf-confirm-box"), "{ignored}");
        assert!(html.contains(r#"<meta http-equiv="refresh" content="5">"#));
    }

    // Without the token: 419; with it: 303 back to the dashboard and a flash.
    let res = t.post_form("/_watchfire/agents/worker/stop", &[]);
    assert_eq!(res.status(), 419);
    let token = token_from(&html);
    let res = t.post_form("/_watchfire/agents/worker/stop", &[("_token", &token)]);
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/_watchfire"));
    let html = t.get("/_watchfire").text();
    assert!(html.contains("worker: stopped."), "flash missing");
    let res = t.post_form("/_watchfire/agents/worker/stop", &[("_token", &token)]);
    assert_eq!(res.status(), 303);
    assert!(
        t.get("/_watchfire")
            .text()
            .contains("agent &quot;worker&quot; is not running.")
    );
    let res = t.post_form(
        "/_watchfire/agents/fetcher%230/pause",
        &[("_token", &token)],
    );
    assert_eq!(res.status(), 303);
    assert_eq!(
        t.app()
            .service::<Agents>()
            .unwrap()
            .status("fetcher#0")
            .unwrap()
            .state,
        AgentState::Paused
    );

    let agents = t.app().service::<Agents>().unwrap();
    // The refused user's form post changes nothing (with a valid CSRF token of the session).
    t.acting_as(2);
    let res = t.post_form(
        "/_watchfire/agents/fetcher%230/resume",
        &[("_token", &token)],
    );
    assert_eq!(res.status(), 403);
    assert_eq!(
        agents.status("fetcher#0").unwrap().state,
        AgentState::Paused
    );

    // `auth`: the same rule.
    let mut settings = WatchfireSettings::from_env();
    settings.dashboard = Access::Auth;
    t.app().insert_service(settings);
    t.clear_cookies();
    let res = t.get("/_watchfire");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
    t.acting_as(1);
    assert_eq!(t.get("/_watchfire").status(), 200);
}

#[test]
fn without_a_gate_nobody_signs_in_to_the_dashboard() {
    let t = TestApp::new(|b| {
        let mut b = b;
        b.settings_mut().key = KEY.to_owned();
        b.agents(|w| {
            w.run("worker", |ctx| async move {
                ctx.cancelled().await;
                Ok(())
            });
        })
    });
    t.acting_as(1);
    assert_eq!(t.get("/_watchfire").status(), 403);
    // `auth` mode no longer lets any signed-in user in either.
    let mut settings = WatchfireSettings::from_env();
    settings.dashboard = Access::Auth;
    t.app().insert_service(settings);
    assert_eq!(t.get("/_watchfire").status(), 403);
    // A failing gate is a server error, not an open door.
    let t = TestApp::new(|b| {
        let mut b = b;
        b.settings_mut().key = KEY.to_owned();
        b.agents(|w| {
            w.dashboard_gate(|_auth, _app| async move {
                Err(smeltery_core::Error::internal("the roles table is gone"))
            });
        })
    });
    t.acting_as(1);
    assert_eq!(t.get("/_watchfire").status(), 500);
}

/// Read from `stream` until `needle` shows up (or fail after 5 s).
async fn read_until(stream: &mut tokio::net::TcpStream, seen: &mut String, needle: &str) {
    let mut buf = [0_u8; 4096];
    let found = tokio::time::timeout(Duration::from_secs(5), async {
        while !seen.contains(needle) {
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "stream closed before `{needle}`: {seen}");
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    })
    .await;
    assert!(found.is_ok(), "`{needle}` never came: {seen}");
}

async fn serve(
    builder: AppBuilder,
) -> (smeltery_core::App, SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut builder = builder;
    builder.settings_mut().port = addr.port();
    builder.settings_mut().host = "127.0.0.1".to_owned();
    builder.settings_mut().shutdown_timeout = Duration::from_secs(5);
    // Local development (`APP_ENV` defaults to production): the stream test reads the API without the token.
    builder.settings_mut().env = "local".to_owned();
    let built = builder.build().await.unwrap();
    let app = built.app.clone();
    let server = tokio::spawn(async move {
        smeltery_core::serve_on(built.app, built.router, listener)
            .await
            .unwrap();
    });
    // Wait for the start hooks.
    for _ in 0..100 {
        if app.service::<Agents>().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (app, addr, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_sends_a_snapshot_then_changes_and_ends_on_shutdown() {
    let (app, addr, server) = serve(builder(AppBuilder::new(Settings::from_env()))).await;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /_watchfire/api/events HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut seen = String::new();
    read_until(&mut stream, &mut seen, "event: snapshot").await;
    read_until(&mut stream, &mut seen, "\"worker\"").await;
    assert!(seen.starts_with("HTTP/1.1 200"), "{seen}");
    assert!(seen.contains("text/event-stream"));

    let agents = app.service::<Agents>().unwrap();
    agents.stop("worker").await.unwrap();
    read_until(&mut stream, &mut seen, "event: status").await;
    read_until(&mut stream, &mut seen, "\"state\":\"stopped\"").await;
    agents
        .emit("post.created", serde_json::json!({"id": 7}))
        .unwrap();
    read_until(&mut stream, &mut seen, "event: event").await;
    read_until(&mut stream, &mut seen, "post.created").await;

    // Shutdown ends the stream and the server.
    app.shutdown();
    let mut rest = Vec::new();
    let ended = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
    assert!(ended.is_ok(), "the stream did not end on shutdown");
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap();
}

async fn console(addr: SocketAddr, args: &[&str]) -> Result<String, String> {
    let mut b = builder(AppBuilder::new(Settings::from_env()));
    b.settings_mut().port = addr.port();
    b.settings_mut().host = "0.0.0.0".to_owned();
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let mut out = Vec::new();
    match dispatch(b, &args, &mut out).await {
        Ok(code) => {
            assert_eq!(code, ExitCode::SUCCESS);
            Ok(String::from_utf8(out).unwrap())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn console_commands_call_the_running_app() {
    let (app, addr, server) = serve(builder(AppBuilder::new(Settings::from_env()))).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let list = console(addr, &["agents:list"]).await.unwrap();
    let lines: Vec<&str> = list.lines().collect();
    assert!(lines[0].starts_with("NAME"), "{list}");
    assert!(
        list.contains("worker") && list.contains("running"),
        "{list}"
    );
    assert!(list.contains("fetcher#1"), "{list}");

    assert_eq!(
        console(addr, &["agents:stop", "worker"])
            .await
            .unwrap()
            .trim(),
        "worker: stopped"
    );
    assert_eq!(
        console(addr, &["agents:start", "worker"])
            .await
            .unwrap()
            .trim(),
        "worker: starting"
    );
    assert_eq!(
        console(addr, &["agents:pause", "fetcher#1"])
            .await
            .unwrap()
            .trim(),
        "fetcher#1: paused"
    );
    assert_eq!(
        console(addr, &["agents:resume", "fetcher#1"])
            .await
            .unwrap()
            .trim(),
        "fetcher#1: starting"
    );
    assert!(
        console(addr, &["agents:restart", "idle"])
            .await
            .unwrap()
            .contains("idle: starting")
    );
    let err = console(addr, &["agents:resume", "worker"])
        .await
        .unwrap_err();
    assert_eq!(err, "agent \"worker\" is not paused");
    let err = console(addr, &["agents:stop", "ghost"]).await.unwrap_err();
    assert_eq!(err, "unknown agent \"ghost\"");
    let err = console(addr, &["agents:stop"]).await.unwrap_err();
    assert!(err.contains("usage"), "{err}");
    let logs = console(addr, &["agents:logs", "worker"]).await.unwrap();
    assert!(logs.contains("info") && logs.contains("working"), "{logs}");

    app.shutdown();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap();

    // Nothing is listening now.
    let err = console(addr, &["agents:list"]).await.unwrap_err();
    assert!(err.contains("is it running?"), "{err}");
}

/// S4-09: the console never sends the token over plain http to another machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn console_refuses_to_send_the_token_over_plain_http_to_another_machine() {
    for (host, api_addr) in [
        ("192.0.2.10", None),
        ("0.0.0.0", Some("http://192.0.2.10:8001")),
        ("0.0.0.0", Some("192.0.2.10:8001")),
        ("example.com", None),
    ] {
        let mut b = builder(AppBuilder::new(Settings::from_env()));
        b.settings_mut().host = host.to_owned();
        let api_addr = api_addr.map(str::to_owned);
        let b = b.on_boot(move |app| async move {
            let mut settings = WatchfireSettings::from_env();
            settings.api_addr = api_addr;
            app.insert_service(settings);
            Ok(())
        });
        let mut out = Vec::new();
        let err = dispatch(b, &["agents:list".to_owned()], &mut out)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("refusing to send the Watchfire API token over plain http to http://"),
            "{err}"
        );
        assert!(
            err.ends_with(
                "/_watchfire/api: set WATCHFIRE_API_ADDR to a loopback address (e.g. 127.0.0.1:8001) or an \
                 https:// URL"
            ),
            "{err}"
        );
    }
    // Loopback and https are fine (checked without sending anything).
    for (host, api_addr, expected) in [
        ("0.0.0.0", None, "http://127.0.0.1:8000/_watchfire/api"),
        ("::", None, "http://[::1]:8000/_watchfire/api"),
        ("localhost", None, "http://localhost:8000/_watchfire/api"),
        (
            "0.0.0.0",
            Some("127.0.0.1:8001"),
            "http://127.0.0.1:8001/_watchfire/api",
        ),
        (
            "0.0.0.0",
            Some("https://ops.example.com/"),
            "https://ops.example.com/_watchfire/api",
        ),
    ] {
        let mut b = builder(AppBuilder::new(Settings::from_env()));
        b.settings_mut().host = host.to_owned();
        b.settings_mut().port = 8000;
        let app = b.build().await.unwrap().app;
        let mut settings = WatchfireSettings::from_env();
        settings.api_addr = api_addr.map(str::to_owned);
        app.insert_service(settings);
        assert_eq!(
            smeltery_watchfire::web::api_base_url(&app).unwrap(),
            expected
        );
    }
}

/// S6-06: `agents:token` prints the token, so no recipe needs the key on a command line.
#[tokio::test]
async fn agents_token_prints_the_api_token() {
    let b = builder(AppBuilder::new(Settings::from_env()));
    let mut out = Vec::new();
    let code = dispatch(b, &["agents:token".to_owned()], &mut out)
        .await
        .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8(out).unwrap().trim(),
        api_token(KEY).unwrap()
    );
    let mut b = builder(AppBuilder::new(Settings::from_env()));
    b.settings_mut().key = "short".to_owned();
    let mut out = Vec::new();
    let err = dispatch(b, &["agents:token".to_owned()], &mut out)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("APP_KEY"), "{err}");
    assert!(out.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_work_serves_the_api_on_its_own_address() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let api = format!("127.0.0.1:{port}");
    let api_for_boot = api.clone();
    let mut b = AppBuilder::new(Settings::from_env());
    b.settings_mut().key = KEY.to_owned();
    b.settings_mut().shutdown_timeout = Duration::from_secs(5);
    // Local development (`APP_ENV` defaults to production): the token-less calls below rely on it.
    b.settings_mut().env = "local".to_owned();
    let b = b
        .agents(|w| {
            w.run("pinger", |ctx| async move {
                ctx.cancelled().await;
                Ok(())
            });
            w.run("stopper", |ctx| async move {
                // Ends the `work` command once the test is done.
                DONE.notified().await;
                ctx.app().shutdown();
                ctx.cancelled().await;
                Ok(())
            });
        })
        .on_boot(move |app| async move {
            let mut settings = WatchfireSettings::from_env();
            settings.api_addr = Some(api_for_boot);
            app.insert_service(settings);
            Ok(())
        });
    let checks = tokio::spawn(async move {
        /// Ends `work` even when a check fails.
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                DONE.notify_one();
            }
        }
        let _done = Done;
        let client = smeltery_watchfire::Http::new(
            smeltery_watchfire::http::ReqwestTransport::new().unwrap(),
            smeltery_watchfire::http::HttpOptions::default(),
            &[],
        );
        let url = format!("http://{api}/_watchfire/api/agents");
        let mut body = None;
        for _ in 0..200 {
            if let Ok(res) = client
                .request(smeltery_watchfire::http::Method::GET, &url)
                .bearer(&api_token(KEY).unwrap())
                .retries(0)
                .send()
                .await
            {
                assert_eq!(res.status(), 200);
                body = Some(res.json::<serde_json::Value>().await.unwrap());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let body = body.expect("the headless API answered");
        assert_eq!(body[0]["name"], "pinger");
        // From loopback in `local` mode the token is optional.
        assert_eq!(client.get(&url).await.unwrap().status(), 200);
        let res = client
            .request(
                smeltery_watchfire::http::Method::POST,
                &format!("http://{api}/_watchfire/api/agents/pinger/stop"),
            )
            .retries(0)
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.json::<serde_json::Value>().await.unwrap()["state"],
            "stopped"
        );
        // Only the API is served headless.
        let res = client
            .get(&format!("http://{api}/_watchfire"))
            .await
            .unwrap();
        assert_eq!(res.status(), 404);
        url
    });
    let mut out = Vec::new();
    let code = tokio::time::timeout(
        Duration::from_secs(30),
        dispatch(b, &["work".to_owned()], &mut out),
    )
    .await
    .expect("work stopped")
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    let url = checks.await.unwrap();
    // The API is gone with `work`.
    let client = smeltery_watchfire::Http::new(
        smeltery_watchfire::http::ReqwestTransport::new().unwrap(),
        smeltery_watchfire::http::HttpOptions::default(),
        &[],
    );
    assert!(
        client
            .request(smeltery_watchfire::http::Method::GET, &url)
            .retries(0)
            .send()
            .await
            .is_err()
    );
}
