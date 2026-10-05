//! Socket payloads never reach the log, at any level (tungstenite logs frames through `log`, which is not bridged;
//! Anvil logs no frame). Its own test binary: a scoped log subscriber must not race other tests' callsites.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use smeltery_anvil::{Anvil, AnvilExt as _, Channel};
use smeltery_core::AppBuilder;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

async fn next_json(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

/// A log writer that keeps everything.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A stateless guard that refuses every bearer credential with 429 (a credential guesser over its budget) or fails
/// with 500 (`X-Test-Token: limited` / `broken`).
struct RefusingGuard;

impl smeltery_core::auth::Guard for RefusingGuard {
    fn name(&self) -> &'static str {
        "refusing"
    }

    fn stateless(&self) -> bool {
        true
    }

    fn authenticate<'a>(
        &'a self,
        _app: &'a smeltery_core::App,
        parts: &'a mut smeltery_core::http::request::Parts,
    ) -> smeltery_core::BoxFuture<'a, smeltery_core::Result<Option<smeltery_core::auth::Principal>>>
    {
        let header = parts
            .headers
            .get("x-test-token")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Box::pin(async move {
            match header.as_deref() {
                Some("limited") => Err(smeltery_core::Error::http(
                    http::StatusCode::TOO_MANY_REQUESTS,
                    "Too Many Requests",
                )),
                Some(_) => Err(smeltery_core::Error::internal("the token store is down")),
                None => Ok(None),
            }
        })
    }
}

/// Sweep W4-05: a guard's expected refusal (429 for a guesser over its budget) on `POST /api/broadcasting/auth` is
/// answered with its status and never logged as an error; a real failure still is. (`TestApp` runs requests on this
/// thread, so the scoped subscriber sees them.)
#[test]
fn expected_guard_refusals_are_not_logged_as_errors() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let app = smeltery_core::testing::TestApp::new(|b| {
        b.guard(RefusingGuard).anvil(|c| {
            c.private("orders.{o}", |_| async { Ok(true) });
        })
    });
    let ask = |token: &'static str| {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "content-type",
            http::HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        headers.insert("x-test-token", http::HeaderValue::from_static(token));
        app.request(
            http::Method::POST,
            "/api/broadcasting/auth",
            headers,
            axum::body::Body::from("socket_id=1.2&channel_name=private-orders.1"),
        )
    };
    assert_eq!(ask("limited").status(), 429);
    let quiet = String::from_utf8_lossy(&captured.0.lock().unwrap()).into_owned();
    assert!(
        !quiet.contains("ERROR"),
        "a 429 was logged as an error:\n{quiet}"
    );
    assert_eq!(ask("broken").status(), 500);
    let log = String::from_utf8_lossy(&captured.0.lock().unwrap()).into_owned();
    assert!(
        log.contains("ERROR"),
        "a real failure is still an error:\n{log}"
    );
}

// One thread: the subscriber set below sees the server's tasks too.
#[tokio::test(flavor = "current_thread")]
async fn payloads_never_reach_the_log() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut core = smeltery_core::config::Settings::from_env();
    core.env = "production".into();
    core.key = "anvil-log-tests-key-0123456789abcdef".into();
    core.url = "https://app.example.com".into();
    core.cache_store = "array".into();
    core.database_url = String::new();
    core.pubsub_driver = "local".into();
    let built = AppBuilder::new(core)
        .anvil(|c| {
            c.public("news");
        })
        .build()
        .await
        .unwrap();
    let app = built.app.clone();
    let key = Anvil::of(&app).unwrap().app_key().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));
    let request = format!("ws://{addr}/app/{key}?protocol=7")
        .into_client_request()
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(
        next_json(&mut ws).await["event"],
        "pusher:connection_established"
    );
    ws.send(Message::text(
        r#"{"event":"pusher:subscribe","data":{"channel":"news","auth":"SECRET-FROM-CLIENT"}}"#,
    ))
    .await
    .unwrap();
    next_json(&mut ws).await;
    Anvil::of(&app)
        .unwrap()
        .to(Channel::public("news"))
        .event("posted")
        .with(&json!({ "secret": "SECRET-EVENT-PAYLOAD" }))
        .await
        .unwrap();
    assert_eq!(next_json(&mut ws).await["event"], "posted");
    ws.send(Message::binary(b"SECRET-BINARY".to_vec()))
        .await
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(message)) = ws.next().await {
            if let Message::Close(frame) = message {
                return frame.map(|f| u16::from(f.code));
            }
        }
        None
    })
    .await
    .unwrap();
    assert_eq!(closed, Some(1003));
    app.shutdown();
    server.await.unwrap().unwrap();

    let log = String::from_utf8_lossy(&captured.0.lock().unwrap()).into_owned();
    assert!(log.contains("anvil"), "the capture works: {log}");
    for secret in [
        "SECRET-FROM-CLIENT",
        "SECRET-EVENT-PAYLOAD",
        "SECRET-BINARY",
    ] {
        assert!(!log.contains(secret), "{secret} was logged:\n{log}");
    }
}
