//! Events cross processes: an event sent in one app (a `work`-like process) reaches a socket held by another app
//! (the web process) through the PubSub `database` driver on one SQLite file; `except` holds across them too.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use smeltery_anvil::{Anvil, AnvilExt as _, Channel, SocketId};
use smeltery_core::AppBuilder;
use smeltery_core::config::Settings;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn settings(url: &str) -> Settings {
    let mut s = Settings::from_env();
    s.env = "production".into();
    s.key = "anvil-cross-process-key-0123456789abcdef".into();
    s.url = "https://app.example.com".into();
    s.database_url = url.into();
    s.cache_store = "array".into();
    s.pubsub_driver = "database".into();
    s.pubsub_poll_interval = Duration::from_millis(20);
    s.shutdown_timeout = Duration::from_secs(5);
    s.log_level = "warn".into();
    s
}

fn build(b: AppBuilder) -> AppBuilder {
    b.anvil(|c| {
        c.public("news");
    })
}

async fn next_json(ws: &mut Client, wait: Duration) -> Option<Value> {
    let read = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    return Some(serde_json::from_str(text.as_str()).unwrap());
                }
                Some(Ok(_)) => {}
                _ => return None,
            }
        }
    };
    tokio::time::timeout(wait, read).await.ok().flatten()
}

async fn connect(addr: std::net::SocketAddr, key: &str) -> (Client, String) {
    let request = format!("ws://{addr}/app/{key}?protocol=7")
        .into_client_request()
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let first = next_json(&mut ws, Duration::from_secs(5)).await.unwrap();
    let data: Value = serde_json::from_str(first["data"].as_str().unwrap()).unwrap();
    let id = data["socket_id"].as_str().unwrap().to_owned();
    let subscribe = json!({ "event": "pusher:subscribe", "data": { "channel": "news" } });
    ws.send(Message::text(subscribe.to_string())).await.unwrap();
    let answer = next_json(&mut ws, Duration::from_secs(5)).await.unwrap();
    assert_eq!(answer["event"], "pusher_internal:subscription_succeeded");
    (ws, id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_event_sent_in_another_process_reaches_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path()
            .join("db.sqlite")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let db = smeltery_core::db::Db::connect(&url).await.unwrap();
    smeltery_core::pubsub::migrations::up(&smeltery_core::db::migration::Schema::new(&db))
        .await
        .unwrap();

    // The web process holds the sockets.
    let web = build(AppBuilder::new(settings(&url)))
        .build()
        .await
        .unwrap();
    let web_app = web.app.clone();
    let key = Anvil::of(&web_app).unwrap().app_key().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(smeltery_core::serve_on(web.app, web.router, listener));

    // The other process (what `work` is): no sockets, the shared PubSub driver.
    let work = build(AppBuilder::new(settings(&url)))
        .build()
        .await
        .unwrap()
        .app;
    let _background = work.start_background().await.unwrap();
    let work_anvil = Anvil::of(&work).unwrap();
    assert_eq!(
        work_anvil.app_key(),
        key,
        "both processes derive the same key"
    );

    let (mut one, one_id) = connect(addr, &key).await;
    let (mut two, _) = connect(addr, &key).await;

    let delivered = work_anvil
        .to(Channel::public("news"))
        .event("posted")
        .with(&json!({ "id": 9 }))
        .except(SocketId::parse(&one_id))
        .await
        .unwrap();
    assert_eq!(delivered.local, 0, "no socket in the sending process");

    let event = next_json(&mut two, Duration::from_secs(10))
        .await
        .expect("the event crossed the processes");
    assert_eq!(event["event"], "posted");
    assert_eq!(event["data"], r#"{"id":9}"#);
    assert!(
        next_json(&mut one, Duration::from_millis(500))
            .await
            .is_none(),
        "the excepted socket got nothing"
    );

    work.shutdown();
    web_app.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .expect("serve stops")
        .unwrap()
        .unwrap();
}
