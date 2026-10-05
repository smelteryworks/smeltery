//! Alerts: hooks and the webhook, on paused time with fakes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use smeltery_watchfire::http::{FakeResponse, FakeTransport, Method};
use smeltery_watchfire::prelude::*;
use smeltery_watchfire::testing::Harness;
use smeltery_watchfire::{Alert, AlertKind};

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

#[tokio::test(start_paused = true)]
async fn hooks_receive_failed_stalled_and_dead_letter_alerts() {
    let seen: Arc<Mutex<Vec<Alert>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let mut w = Watchfire::new();
    w.on_alert(move |alert| {
        let sink = Arc::clone(&sink);
        async move { sink.lock().unwrap().push(alert) }
    });
    w.run("fragile", |ctx| async move {
        ctx.sleep(1.secs()).await;
        Err(AgentError::msg("broken"))
    })
    .restart(Restart::Never);
    w.run("sleepy", |ctx| async move {
        ctx.cancelled().await;
        Ok(())
    })
    .heartbeat_timeout(5.secs())
    .restart(Restart::Never);
    w.job::<Doomed>();
    let mut h = Harness::from_watchfire(w).workers(1);
    h.start().await.unwrap();
    Doomed.dispatch(h.app_handle()).await.unwrap();
    h.advance(10.secs()).await;
    h.shutdown().await;
    let alerts = seen.lock().unwrap().clone();
    let kinds: Vec<(AlertKind, &str)> = alerts.iter().map(|a| (a.kind, a.agent.as_str())).collect();
    assert!(kinds.contains(&(AlertKind::Failed, "fragile")), "{kinds:?}");
    assert!(kinds.contains(&(AlertKind::Stalled, "sleepy")), "{kinds:?}");
    assert!(
        kinds.contains(&(AlertKind::Failed, "sleepy")),
        "stalled under Never fails: {kinds:?}"
    );
    let dead = alerts
        .iter()
        .find(|a| a.kind == AlertKind::DeadLetter)
        .unwrap();
    assert_eq!(dead.agent, "queue#0");
    assert_eq!(dead.job.as_deref(), Some("doomed"));
    assert!(dead.message.contains("never works"), "{}", dead.message);
}

#[tokio::test(start_paused = true)]
async fn the_webhook_gets_json_with_retries_and_a_slow_hook_never_blocks_supervision() {
    let fake = FakeTransport::new();
    let url = "https://hooks.example.test/alerts";
    fake.on(Method::POST, url, FakeResponse::status(503)).on(
        Method::POST,
        url,
        FakeResponse::status(204),
    );
    let mut w = Watchfire::new();
    // A hook slower than its 10 s limit.
    w.on_alert(|_alert| async {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    });
    w.run("fragile", |ctx| async move {
        ctx.sleep(1.secs()).await;
        Err(AgentError::msg("broken"))
    })
    .backoff(1.secs()..=1.secs())
    .max_restarts(1, 1.hours());
    let mut h = Harness::from_watchfire(w)
        .http(fake.clone())
        .alert_webhook(url);
    h.start().await.unwrap();
    h.advance(5.secs()).await;
    // Supervision went on while the hook hung: the agent hit its limit and failed.
    assert_eq!(h.state(), AgentState::Failed);
    h.advance(30.secs()).await;
    let posts: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|r| r.url == url)
        .collect();
    assert_eq!(posts.len(), 2, "503 retried once");
    let body: serde_json::Value = serde_json::from_slice(&posts[1].body).unwrap();
    assert_eq!(body["kind"], "failed");
    assert_eq!(body["agent"], "fragile");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("restarted 1 times")
    );
    assert!(body["at_ms"].is_i64());
    h.shutdown().await;
}
