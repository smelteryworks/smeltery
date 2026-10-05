//! The live dashboard: Watchfire + Sparks.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::json;
use smeltery_core::AppBuilder;
use smeltery_core::config::Settings;
use smeltery_core::testing::TestApp;
use smeltery_sparks::SparksExt as _;
use smeltery_sparks::testing::TestSpark;
use smeltery_watchfire::prelude::*;
use smeltery_watchfire::web::Access;
use smeltery_watchfire::{Agents, WatchfireSettings};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const KEY: &str = "watchfire-live-key-0123456789abcdef";

fn register(w: &mut Watchfire) {
    w.run("worker", |ctx| async move {
        ctx.cancelled().await;
        Ok(())
    });
    w.schedule()
        .call("cleanup", |_ctx| async move { Ok(()) })
        .every(5.mins());
    // User 1 is the admin, unless a test withdrew the admission (`Withdrawn`).
    w.dashboard_gate(|auth, app| async move {
        let withdrawn = app
            .service::<Withdrawn>()
            .is_some_and(|w| w.0.load(std::sync::atomic::Ordering::SeqCst));
        Ok(auth.id() == Some(1) && !withdrawn)
    });
}

/// Set to make the gate refuse everybody (the gate is asked on every request, so a page already open is refused too).
#[derive(Clone, Default)]
struct Withdrawn(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Withdrawn {
    fn set(&self, on: bool) {
        self.0.store(on, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `APP_ENV=local` (TestApp forces `testing`): the local-development rule applies. `/token` hands out the
/// session's CSRF token without visiting the dashboard.
fn local_env(b: AppBuilder) -> AppBuilder {
    let mut b = sparks_first(b);
    b.settings_mut().env = "local".to_owned();
    b.routes(|r| {
        r.get(
            "/token",
            |session: smeltery_core::session::Session| async move {
                session.csrf_token().unwrap_or_default()
            },
        );
    })
}

fn headers(pairs: &[(&'static str, &'static str)]) -> http::HeaderMap {
    let mut h = http::HeaderMap::new();
    for (k, v) in pairs {
        h.insert(*k, http::HeaderValue::from_static(v));
    }
    h
}

fn sparks_first(b: AppBuilder) -> AppBuilder {
    let mut b = b;
    b.settings_mut().key = KEY.to_owned();
    b.sparks(|_| {}).agents(register)
}

fn agents_first(b: AppBuilder) -> AppBuilder {
    let mut b = b;
    b.settings_mut().key = KEY.to_owned();
    b.agents(register).sparks(|_| {})
}

fn local() -> SocketAddr {
    "127.0.0.1:50000".parse().unwrap()
}

fn settle(t: &TestApp) {
    t.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
}

#[test]
fn the_dashboard_is_made_of_live_components_in_either_order() {
    for build in [sparks_first as fn(AppBuilder) -> AppBuilder, agents_first] {
        let t = TestApp::new(build).with_agents();
        settle(&t);
        t.acting_as(1);
        let html = t.get("/_watchfire").text();
        assert!(html.contains(r#"wire:name="watchfire.agents""#), "{html}");
        assert!(html.contains(r#"wire:name="watchfire.queue""#));
        assert!(html.contains(r#"wire:stream=""#));
        assert!(html.contains("/_sparks/sparks.js"));
        assert!(html.contains(r#"<noscript><meta http-equiv="refresh" content="5"></noscript>"#));
        // Server-rendered first: the data is in the page without JavaScript.
        assert!(html.contains("<b>worker</b>"));
        assert!(html.contains("cleanup"));
        // The no-JS fallbacks stay: forms, and for stop / restart a link to the confirmation.
        assert!(html.contains(r#"action="/_watchfire/agents/worker/pause""#));
        assert!(html.contains(r#"href="/_watchfire?agent=worker&amp;confirm=stop#wf-confirm""#));
        assert!(html.contains(r#"wire:submit="act("#));
        // That link opens the confirmation on a page without the live panels and without the reload.
        let confirm = t.get("/_watchfire?agent=worker&confirm=stop").text();
        assert!(!confirm.contains("wire:name"), "{confirm}");
        assert!(!confirm.contains("http-equiv=\"refresh\""));
        assert!(confirm.contains(r#"<div class="wf-confirm-box" id="wf-confirm" tabindex="-1""#));
        assert!(confirm.contains(r#"action="/_watchfire/agents/worker/stop""#));
        // A confirmation the agent does not offer is ignored: the live page.
        let other = t.get("/_watchfire?agent=worker&confirm=pause").text();
        assert!(other.contains(r#"wire:name="watchfire.agents""#));
    }
}

#[test]
fn without_sparks_the_page_keeps_the_meta_refresh() {
    let t = TestApp::new(|b| {
        let mut b = b;
        b.settings_mut().key = KEY.to_owned();
        b.agents(register)
    })
    .with_agents();
    settle(&t);
    t.acting_as(1);
    let html = t.get("/_watchfire").text();
    assert!(!html.contains("wire:name"));
    assert!(!html.contains("<noscript>"));
    assert!(html.contains(r#"<meta http-equiv="refresh" content="5">"#));
}

/// Only a viewer the gate admits gets a stream token: a guest is sent to sign in and a refused user gets 403, so
/// neither page carries one; and the stream refuses a request without a valid token.
#[test]
fn only_admitted_viewers_get_stream_tokens() {
    let t = TestApp::new(sparks_first).with_agents();
    settle(&t);
    t.from_addr(local());
    let res = t.get("/_watchfire");
    assert_eq!(res.status(), 303);
    assert!(!res.text().contains("wire:stream"));
    t.acting_as(2);
    let res = t.get("/_watchfire");
    assert_eq!(res.status(), 403);
    assert!(!res.text().contains("wire:stream"));
    t.acting_as(1);
    let html = t.get("/_watchfire").text();
    assert!(html.contains(r#"wire:stream=""#));
    for query in ["c=watchfire.agents", "t=", "t=forged.token"] {
        assert_eq!(
            t.get(&format!("/_sparks/stream?{query}")).status(),
            403,
            "{query}"
        );
    }
}

#[test]
fn actions_through_the_update_endpoint_respect_the_access_gate() {
    // Outside local development: signed-in users the gate admits.
    let t = TestApp::new(sparks_first).with_agents();
    settle(&t);
    t.from_addr(local());
    // A loopback guest is not enough any more.
    let res = t.get("/_watchfire");
    assert_eq!(
        (res.status(), res.header("location")),
        (303, Some("/login"))
    );
    t.acting_as(1);
    let html = t.get("/_watchfire").text();
    let panel = TestSpark::from_html(&html, "watchfire.agents").expect("the agents panel");
    let agents = t.app().service::<Agents>().unwrap();

    // The snapshot replayed by a guest (another browser) or by another user is refused before anything runs: Sparks
    // binds snapshots to their session and user (419).
    t.clear_cookies();
    let mut replayed = panel.clone();
    let res = replayed.call("act", json!(["worker", "stop"])).send(&t);
    assert_eq!(res.status(), 419);
    let res = replayed.call("ask", json!(["worker", "stop"])).send(&t);
    assert_eq!(res.status(), 419);
    assert_eq!(replayed.call("cancel", json!([])).send(&t).status(), 419);
    let res = replayed.call("$refresh", json!([])).send(&t);
    assert_eq!(res.status(), 419);
    t.acting_as(2);
    let res = replayed.call("act", json!(["worker", "stop"])).send(&t);
    assert_eq!(res.status(), 419);
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);

    // The gate is asked on every update: once it refuses the user, the panel of this very browser may neither act
    // nor refresh, nor ask (403).
    t.clear_cookies();
    t.acting_as(1);
    let html = t.get("/_watchfire").text();
    let mut panel = TestSpark::from_html(&html, "watchfire.agents").expect("the agents panel");
    let withdrawn = Withdrawn::default();
    t.app().insert_service(withdrawn.clone());
    withdrawn.set(true);
    let res = panel.call("act", json!(["worker", "stop"])).send(&t);
    assert_eq!(res.status(), 403);
    assert_eq!(panel.call("$refresh", json!([])).send(&t).status(), 403);
    let res = panel.call("ask", json!(["worker", "stop"])).send(&t);
    assert_eq!(res.status(), 403);
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);
    withdrawn.set(false);

    // The admitted user: the action runs and the panel re-renders with fresh data.
    t.acting_as(1);
    let res = panel.call("act", json!(["worker", "stop"])).send(&t);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Stopped);
    assert!(panel.html().contains("worker: stopped."));
    assert!(
        panel
            .html()
            .contains(r#"<span class="wf-pill" data-state="stopped">stopped</span>"#)
    );
    let res = panel.call("act", json!(["worker", "explode"])).send(&t);
    assert_eq!(res.status(), 400);
    let res = panel.call("act", json!(["worker", "start"])).send(&t);
    assert_eq!(res.status(), 200);
    settle(&t);
    let res = panel.call("$refresh", json!([])).send(&t);
    assert_eq!(res.status(), 200);
    assert!(
        panel
            .html()
            .contains(r#"<span class="wf-pill" data-state="running">running</span>"#)
    );
    // Stop and restart ask first: `ask` opens the confirmation (and nothing happens to the agent), a pushed refresh
    // keeps it open, `cancel` or asking again closes it, and `act` runs the action and closes it.
    let open = r#"<div class="wf-confirm-box" id="wf-confirm" tabindex="-1""#;
    assert!(!panel.html().contains(open));
    let res = panel.call("ask", json!(["worker", "stop"])).send(&t);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert!(panel.html().contains(open), "{}", panel.html());
    assert_eq!(panel.html().matches(open).count(), 1);
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);
    assert_eq!(panel.call("$refresh", json!([])).send(&t).status(), 200);
    assert!(panel.html().contains(open));
    assert_eq!(panel.call("cancel", json!([])).send(&t).status(), 200);
    assert!(!panel.html().contains(open));
    assert_eq!(
        panel
            .call("ask", json!(["worker", "restart"]))
            .send(&t)
            .status(),
        200
    );
    assert!(panel.html().contains(open));
    assert_eq!(
        panel
            .call("ask", json!(["worker", "restart"]))
            .send(&t)
            .status(),
        200
    );
    assert!(!panel.html().contains(open));
    // Only stop and restart ask; a refused user may not ask.
    assert_eq!(
        panel
            .call("ask", json!(["worker", "pause"]))
            .send(&t)
            .status(),
        400
    );
    withdrawn.set(true);
    assert_eq!(
        panel
            .call("ask", json!(["worker", "stop"]))
            .send(&t)
            .status(),
        403
    );
    assert_eq!(panel.call("cancel", json!([])).send(&t).status(), 403);
    withdrawn.set(false);
    assert_eq!(
        panel
            .call("ask", json!(["worker", "stop"]))
            .send(&t)
            .status(),
        200
    );
    let res = panel.call("act", json!(["worker", "stop"])).send(&t);
    assert_eq!(res.status(), 200);
    assert!(!panel.html().contains(open));
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Stopped);
    assert_eq!(
        panel
            .call("act", json!(["worker", "start"]))
            .send(&t)
            .status(),
        200
    );
    settle(&t);
    // Only the panel's actions are callable.
    assert_eq!(panel.call("load", json!([])).send(&t).status(), 403);

    // `off`: 404.
    let mut settings = WatchfireSettings::from_env();
    settings.dashboard = Access::Off;
    t.app().insert_service(settings);
    assert_eq!(panel.call("$refresh", json!([])).send(&t).status(), 404);
    assert_eq!(
        panel
            .call("ask", json!(["worker", "stop"]))
            .send(&t)
            .status(),
        404
    );
    assert_eq!(panel.call("cancel", json!([])).send(&t).status(), 404);
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);
}

#[test]
fn local_development_opens_the_dashboard_to_unproxied_loopback_requests() {
    let t = TestApp::new(local_env).with_agents().with_csrf();
    settle(&t);
    let agents = t.app().service::<Agents>().unwrap();

    // A proxied request (loopback peer, a forwarding header) is a guest like any other.
    t.from_addr(local());
    let res = t.request(
        http::Method::GET,
        "/_watchfire",
        headers(&[("x-forwarded-for", "203.0.113.9")]),
        axum::body::Body::empty(),
    );
    assert_eq!(res.status(), 303);
    // The admitted browser, from loopback: works without signing in.
    let html = t.get("/_watchfire").text();
    let mut panel = TestSpark::from_html(&html, "watchfire.agents").expect("the agents panel");
    let res = panel.call("act", json!(["worker", "stop"])).send(&t);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Stopped);
    // The same browser from another address: a guest.
    t.from_addr("203.0.113.9:4000".parse().unwrap());
    assert_eq!(
        panel
            .call("act", json!(["worker", "start"]))
            .send(&t)
            .status(),
        401
    );
    // `auth`: no local shortcut even in local development.
    t.from_addr(local());
    let mut settings = WatchfireSettings::from_env();
    settings.dashboard = Access::Auth;
    t.app().insert_service(settings);
    assert_eq!(
        panel
            .call("act", json!(["worker", "start"]))
            .send(&t)
            .status(),
        401
    );
    assert_eq!(t.get("/_watchfire").status(), 303);
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Stopped);
}

/// `GET path` over a new connection (`Connection: close`): the whole answer as text.
async fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

/// The `wire:stream` token of component `name` in a page.
fn stream_token_of(html: &str, name: &str) -> Option<String> {
    let at = html.find(&format!(r#"wire:name="{name}""#))?;
    let rest = &html[at..];
    let tag_end = rest.find('>')?;
    let start = rest[..tag_end].find(r#"wire:stream=""#)? + r#"wire:stream=""#.len();
    let len = rest[start..].find('"')?;
    Some(rest[start..start + len].to_owned())
}

async fn read_for(stream: &mut tokio::net::TcpStream, seen: &mut String, window: Duration) {
    let mut buf = [0_u8; 4096];
    let _ = tokio::time::timeout(window, async {
        loop {
            let n = stream.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_changes_push_throttled_refreshes_to_stream_subscribers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut b = sparks_first(AppBuilder::new(Settings::from_env()));
    b.settings_mut().shutdown_timeout = Duration::from_secs(5);
    // Local development: the page opens to this loopback client without signing in.
    b.settings_mut().env = "local".to_owned();
    b.settings_mut().url = format!("http://{addr}");
    let built = b.build().await.unwrap();
    let app = built.app.clone();
    let server = tokio::spawn(async move {
        smeltery_core::serve_on(built.app, built.router, listener)
            .await
            .unwrap();
    });
    let mut agents = None;
    for _ in 0..100 {
        if let Some(a) = app.service::<Agents>() {
            agents = Some(a);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let agents = agents.unwrap();

    // The page an admitted viewer gets carries the panels' stream tokens.
    let page = http_get(addr, "/_watchfire").await;
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    let token =
        stream_token_of(&page, "watchfire.agents").expect("a stream token on the agents panel");
    assert!(stream_token_of(&page, "watchfire.queue").is_some());
    // Without a valid token there is no stream.
    for query in ["c=watchfire.agents", "t=watchfire.agents", "t=forged.token"] {
        let res = http_get(addr, &format!("/_sparks/stream?{query}")).await;
        assert!(res.starts_with("HTTP/1.1 403"), "{query}: {res}");
    }

    // The browser sends the page's session cookie with the stream: the token is bound to that session.
    let cookie = page
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|l| l["set-cookie:".len()..].trim().split(';').next())
        .expect("the page's session cookie")
        .to_owned();
    let without = http_get(addr, &format!("/_sparks/stream?t={token}")).await;
    assert!(
        without.starts_with("HTTP/1.1 403"),
        "a token without its session: {without}"
    );
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /_sparks/stream?t={token} HTTP/1.1\r\nHost: {addr}\r\nCookie: {cookie}\r\nAccept: text/event-stream\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut seen = String::new();
    // Let the subscription settle and any start-up change pass.
    read_for(&mut stream, &mut seen, Duration::from_millis(700)).await;
    assert!(seen.starts_with("HTTP/1.1 200"), "{seen}");
    seen.clear();

    // One change: one refresh within the throttle interval.
    agents.stop("worker").await.unwrap();
    read_for(&mut stream, &mut seen, Duration::from_millis(800)).await;
    let refresh = r#"{"target":"watchfire.agents","kind":"refresh"}"#;
    assert_eq!(seen.matches(refresh).count(), 1, "{seen}");
    seen.clear();

    // A burst of changes is coalesced: at most one refresh per 500 ms.
    for _ in 0..15 {
        agents.start("worker").await.unwrap();
        agents.stop("worker").await.unwrap();
    }
    read_for(&mut stream, &mut seen, Duration::from_millis(1100)).await;
    let count = seen.matches(refresh).count();
    assert!((1..=3).contains(&count), "{count} refreshes: {seen}");
    // The queue panel is not subscribed on this stream.
    assert!(!seen.contains("watchfire.queue"));

    app.shutdown();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn the_panels_open_locally_only_to_a_browser_the_page_let_in() {
    let t = TestApp::new(local_env).with_agents().with_csrf();
    settle(&t);
    t.from_addr(local());
    let agents = t.app().service::<Agents>().unwrap();
    // A snapshot from the page, then a new browser that never visited the dashboard: refused (Sparks binds the
    // snapshot to its session, 419).
    let html = t.get("/_watchfire").text();
    let panel = TestSpark::from_html(&html, "watchfire.agents").expect("the agents panel");
    let body = {
        let mut p = panel.clone();
        p.call("act", json!(["worker", "stop"]));
        p.request_body()
    };
    t.clear_cookies();
    let token = t.get("/token").text();
    let res = smeltery_sparks::testing::post_update(&t, &body, Some(&token));
    assert_eq!(res.status(), 419, "{}", res.text());
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Running);
    // It visits the page from loopback: with that page's panel it may act.
    let html = t.get("/_watchfire").text();
    let panel = TestSpark::from_html(&html, "watchfire.agents").expect("the agents panel");
    let body = {
        let mut p = panel.clone();
        p.call("act", json!(["worker", "stop"]));
        p.request_body()
    };
    let res = smeltery_sparks::testing::post_update(&t, &body, Some(&token));
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Stopped);
    // A dashboard request through a proxy removes the mark.
    let res = t.request(
        http::Method::GET,
        "/_watchfire",
        headers(&[("x-forwarded-for", "203.0.113.9")]),
        axum::body::Body::empty(),
    );
    assert_eq!(res.status(), 303);
    let mut again = panel.clone();
    again.call("act", json!(["worker", "start"]));
    let res = smeltery_sparks::testing::post_update(&t, &again.request_body(), Some(&token));
    assert_eq!(res.status(), 401, "{}", res.text());
    assert_eq!(agents.status("worker").unwrap().state, AgentState::Stopped);
}
