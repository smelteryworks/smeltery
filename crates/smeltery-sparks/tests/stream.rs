//! `GET /_sparks/stream`: broadcast messages reach subscribed streams; streams end on shutdown.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use http_body_util::BodyExt as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use smeltery_core::config::Settings;
use smeltery_core::{App, AppBuilder};
use smeltery_macros::{Spark, actions};
use smeltery_sparks::{Broadcast, SparksExt};
use tower::ServiceExt as _;

#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views", stream)]
pub struct Live {
    pub n: i64,
}

#[actions(crate = "smeltery_sparks")]
impl Live {}

async fn app() -> (App, axum::Router) {
    app_with(1000).await
}

async fn app_with(max_streams: usize) -> (App, axum::Router) {
    let mut settings = Settings::from_env();
    settings.env = "testing".into();
    let built = AppBuilder::new(settings)
        .sparks(move |s| {
            s.add::<Live>().max_streams(max_streams);
        })
        .build()
        .await
        .unwrap();
    (built.app, built.router)
}

async fn status_of(router: &axum::Router, query: &str) -> (http::StatusCode, axum::body::Body) {
    let req = http::Request::get(format!("/_sparks/stream?{query}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    (res.status(), res.into_body())
}

/// The query subscribing to `(component name, instance id)` pairs: their stream tokens, as pages carry them.
fn tokens(app: &App, pairs: &[(&str, &str)]) -> String {
    let tokens: Vec<String> = pairs
        .iter()
        .map(|(name, id)| smeltery_sparks::testing::stream_token(app, name, id).unwrap())
        .collect();
    format!("t={}", tokens.join(","))
}

async fn open(router: &axum::Router, query: &str) -> axum::body::Body {
    let req = http::Request::get(format!("/_sparks/stream?{query}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    assert!(res.headers().get("set-cookie").is_none());
    res.into_body()
}

/// The next data frame as text (skipping keep-alive comments), or `None` when the stream ended.
async fn next_data(body: &mut axum::body::Body) -> Option<String> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("a frame within 5 s")?
            .unwrap();
        if let Ok(data) = frame.into_data() {
            let text = String::from_utf8(data.to_vec()).unwrap();
            if text.starts_with("data:") {
                return Some(text);
            }
        }
    }
}

#[tokio::test]
async fn broadcasts_reach_subscribed_streams_only() {
    let (app, router) = app().await;
    let broadcast = Broadcast::of(&app).unwrap();
    let mut live = open(&router, &tokens(&app, &[("live", "abc")])).await;
    let mut other = open(&router, &tokens(&app, &[("other", "def")])).await;
    // Wait until both streams subscribed (the handler runs when the response is created).
    assert_eq!(broadcast.streams(), 2);

    assert_eq!(broadcast.to("live").refresh(), 2);
    broadcast.to("abc").emit("tick", json!({ "n": 3 }));
    broadcast.to("other").emit("ping", json!(null));

    assert_eq!(
        next_data(&mut live).await.unwrap(),
        "data: {\"target\":\"live\",\"kind\":\"refresh\"}\n\n"
    );
    assert_eq!(
        next_data(&mut live).await.unwrap(),
        "data: {\"target\":\"abc\",\"kind\":\"event\",\"event\":\"tick\",\"payload\":{\"n\":3}}\n\n"
    );
    assert_eq!(
        next_data(&mut other).await.unwrap(),
        "data: {\"target\":\"other\",\"kind\":\"event\",\"event\":\"ping\",\"payload\":null}\n\n"
    );

    // Shutdown ends every stream.
    app.shutdown();
    assert_eq!(next_data(&mut live).await, None);
    assert_eq!(next_data(&mut other).await, None);
}

#[tokio::test]
async fn a_slow_stream_is_disconnected() {
    let (app, router) = app().await;
    let broadcast = Broadcast::of(&app).unwrap();
    let mut slow = open(&router, &tokens(&app, &[("live", "abc")])).await;
    for i in 0..300 {
        broadcast.to("live").emit("n", i);
    }
    // The stream lagged behind the bounded channel: it ends (the client reconnects and refreshes).
    assert_eq!(next_data(&mut slow).await, None);
    assert_eq!(broadcast.streams(), 0);
    // No listeners is not an error.
    assert_eq!(broadcast.to("live").refresh(), 0);
}

/// At most `max_streams` streams are open at once; a closed stream frees its slot (S3-13).
#[tokio::test]
async fn open_streams_are_capped() {
    let (app, router) = app_with(2).await;
    let live = tokens(&app, &[("live", "abc")]);
    let first = open(&router, &live).await;
    let second = open(&router, &live).await;
    let (status, _) = status_of(&router, &live).await;
    assert_eq!(status, http::StatusCode::SERVICE_UNAVAILABLE);
    drop(first);
    let (status, third) = status_of(&router, &live).await;
    assert_eq!(status, http::StatusCode::OK);
    drop((second, third));
}

/// A stream subscribes only to what valid stream tokens allow: names alone, forged or foreign tokens are refused
/// (the Watchfire I-1 timing leak: a guest subscribing to a panel's refreshes).
#[tokio::test]
async fn streams_need_valid_tokens() {
    let (app, router) = app().await;
    for query in ["c=live", "t=", "t=garbage", "t=e30.c2ln"] {
        let (status, _) = status_of(&router, query).await;
        assert_eq!(status, http::StatusCode::FORBIDDEN, "{query}");
    }
    // A token signed under another APP_KEY.
    let mut settings = Settings::from_env();
    settings.env = "testing".into();
    settings.key = "another-key-another-key-another-key!".into();
    let other = AppBuilder::new(settings)
        .sparks(|s| {
            s.add::<Live>();
        })
        .build()
        .await
        .unwrap();
    let foreign = tokens(&other.app, &[("live", "abc")]);
    let (status, _) = status_of(&router, &foreign).await;
    assert_eq!(status, http::StatusCode::FORBIDDEN);
    let (status, _) = status_of(&router, &tokens(&app, &[("live", "abc")])).await;
    assert_eq!(status, http::StatusCode::OK);
}

/// Review L-5: `anvil:` event names belong to listener events; `emit` refuses them.
#[tokio::test]
async fn emit_refuses_the_names_of_listener_events() {
    let (app, router) = app().await;
    let broadcast = Broadcast::of(&app).unwrap();
    let mut live = open(&router, &tokens(&app, &[("live", "abc")])).await;
    assert_eq!(
        broadcast.to("live").emit("anvil:OrderShipped", json!({})),
        0
    );
    assert_eq!(broadcast.to("live").emit("tick", json!(1)), 1);
    assert_eq!(
        next_data(&mut live).await.unwrap(),
        "data: {\"target\":\"live\",\"kind\":\"event\",\"event\":\"tick\",\"payload\":1}\n\n"
    );
}
