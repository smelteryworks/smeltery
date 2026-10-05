//! Push across processes: an agent's `refresh()` in a `work` process reaches a page held by a `serve --no-agents`
//! process (the README's live counter in the two-process layout). Two apps in one test share a SQLite file; the web
//! app runs the real server on 127.0.0.1, the work app the real `work` command.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smeltery_core::config::Settings;
use smeltery_core::{App, AppBuilder, Background};
use smeltery_macros::{Spark, actions};
use smeltery_sparks::{Broadcast, SparksExt};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views", stream)]
pub struct Live {
    pub n: i64,
}

#[actions(crate = "smeltery_sparks")]
impl Live {}

fn settings(url: &str) -> Settings {
    let mut s = Settings::from_env();
    // `serve` and `work` refuse APP_ENV=testing.
    s.env = "local".into();
    s.key = "cross-process-key-0123456789abcdef".into();
    s.database_url = url.into();
    s.cache_store = "array".into();
    s.pubsub_driver = "auto".into();
    s.pubsub_poll_interval = Duration::from_millis(20);
    s.shutdown_timeout = Duration::from_secs(5);
    s.log_level = "warn".into();
    s
}

/// Read the event stream until a refresh of `live` arrives; `false` after `wait`.
async fn sees_refresh(addr: std::net::SocketAddr, token: &str, wait: Duration) -> bool {
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /_sparks/stream?t={token} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\n\r\n"
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut seen = Vec::new();
    let read = async {
        let mut buf = [0u8; 4096];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                return false;
            }
            seen.extend_from_slice(&buf[..n]);
            if String::from_utf8_lossy(&seen).contains(r#"{"target":"live","kind":"refresh"}"#) {
                return true;
            }
        }
    };
    tokio::time::timeout(wait, read).await.unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_in_work_refreshes_a_page_held_by_a_web_only_serve() {
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

    // The web process: `serve --no-agents`.
    let built = AppBuilder::new(settings(&url))
        .sparks(|s| {
            s.add::<Live>();
        })
        .build()
        .await
        .unwrap();
    let web = built.app.clone();
    web.skip_background();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));

    // The work process: a background loop (what a Watchfire agent is) refreshing the counter.
    let work_app: Arc<Mutex<Option<App>>> = Arc::default();
    let slot = Arc::clone(&work_app);
    let builder = AppBuilder::new(settings(&url))
        .sparks(|s| {
            s.add::<Live>();
        })
        .on_start(move |app: App| async move {
            *slot.lock().unwrap() = Some(app.clone());
            let token = app.shutdown_token().clone();
            let ticker = tokio::spawn(async move {
                let broadcast = Broadcast::of(&app).unwrap();
                loop {
                    tokio::select! {
                        () = token.cancelled() => return,
                        () = tokio::time::sleep(Duration::from_millis(50)) => {}
                    }
                    // No page is connected to this process: the count here is 0.
                    assert_eq!(broadcast.to("live").refresh(), 0);
                }
            });
            Ok(Background::new(async move {
                let _ = ticker.await;
            }))
        });
    let token = smeltery_sparks::stream_token(&web, "live", "abc", None, None).unwrap();
    let mut out = Vec::new();
    let args = ["work".to_owned()];
    let work = smeltery_core::console::dispatch(builder, &args, &mut out);
    let client = async {
        let arrived = sees_refresh(addr, &token, Duration::from_secs(10)).await;
        for _ in 0..100 {
            if work_app.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if let Some(app) = work_app.lock().unwrap().take() {
            app.shutdown();
        }
        arrived
    };
    // `dispatch` runs here (its future is not `Send`), next to the page.
    let (worked, arrived) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(work, client)
    })
    .await
    .expect("work stops");
    worked.unwrap();
    web.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .expect("serve stops")
        .unwrap()
        .unwrap();
    assert!(
        arrived,
        "the refresh sent in the work process never reached the page on the web process"
    );
}
