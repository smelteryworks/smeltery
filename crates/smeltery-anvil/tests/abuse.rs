//! Abuse through real sockets (127.0.0.1 only; SQLite temp files): client-event floods, presence re-subscribes and
//! large member lists. Guest grants are signed here as the auth endpoint signs them (`.guests()` patterns).
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use smeltery_anvil::{Anvil, AnvilExt as _, Member};
use smeltery_core::AppBuilder;
use smeltery_core::config::Settings;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn settings(url: &str) -> Settings {
    let mut s = Settings::from_env();
    s.env = "production".into();
    s.key = "anvil-abuse-tests-key-0123456789abcdef0123".into();
    s.url = "https://app.example.com".into();
    s.database_url = url.into();
    s.cache_store = "array".into();
    s.pubsub_driver = "database".into();
    s.pubsub_poll_interval = Duration::from_millis(50);
    s.shutdown_timeout = Duration::from_secs(5);
    s.log_level = "error".into();
    s
}

fn build(b: AppBuilder) -> AppBuilder {
    b.anvil(|c| {
        c.private("chat.{c}", |_| async { Ok(true) })
            .guests()
            .whispers();
        c.presence("room.{r}", |_| async { Ok(Some(Member::new("u1"))) })
            .guests();
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sign(secret: &str, msg: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(msg.as_bytes());
    hex(&mac.finalize().into_bytes())
}

/// What the auth endpoint answers a guest (`.guests()` pattern): `<key>:g.-.<issued>.<expires>:<hex>`.
fn guest_auth(key: &str, secret: &str, socket: &str, channel: &str, data: Option<&str>) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let grant = format!("g.-.{now}.{}", now + 300);
    let msg = match data {
        Some(d) => format!("{socket}:{channel}:{grant}:{d}"),
        None => format!("{socket}:{channel}:{grant}"),
    };
    format!("{key}:{grant}:{}", sign(secret, &msg))
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
    (ws, id)
}

struct Env {
    _dir: tempfile::TempDir,
    db: smeltery_core::db::Db,
    addr: std::net::SocketAddr,
    key: String,
    secret: String,
    app: smeltery_core::App,
}

async fn start() -> Env {
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
    let schema = smeltery_core::db::migration::Schema::new(&db);
    smeltery_core::pubsub::migrations::up(&schema)
        .await
        .unwrap();
    smeltery_anvil::presence_migrations::up(&schema)
        .await
        .unwrap();
    let web = build(AppBuilder::new(settings(&url)))
        .build()
        .await
        .unwrap();
    let app = web.app.clone();
    let key = Anvil::of(&app).unwrap().app_key().to_owned();
    let secret = hex(&app.derive_key("anvil.secret").unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(smeltery_core::serve_on(web.app, web.router, listener));
    tokio::time::sleep(Duration::from_millis(300)).await;
    Env {
        _dir: dir,
        db,
        addr,
        key,
        secret,
        app,
    }
}

async fn rows(db: &smeltery_core::db::Db) -> (i64, i64) {
    let r = db
        .query_with(
            "SELECT COUNT(*) AS n, COALESCE(SUM(LENGTH(payload)),0) AS b FROM pubsub_messages",
            [],
        )
        .await
        .unwrap();
    (
        r[0].try_get("", "n").unwrap(),
        r[0].try_get("", "b").unwrap(),
    )
}

/// Sweep W5-01: one client address with 50 sockets in 10 channels each sends 10 client events a second per socket
/// for 3 s. Every accepted one becomes a row in the app's database (the `database` driver); the address's budget
/// (`ANVIL_CLIENT_EVENTS_PER_CLIENT`, 50 a second) bounds them. Before the budget: about 900 rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_events_of_one_address_are_bounded() {
    let env = start().await;
    let sockets = 50;
    let channels = 10;
    let mut clients = Vec::new();
    for _ in 0..sockets {
        let (mut ws, id) = connect(env.addr, &env.key).await;
        for c in 0..channels {
            let channel = format!("private-chat.{}_{c}", clients.len());
            let auth = guest_auth(&env.key, &env.secret, &id, &channel, None);
            let sub = json!({"event":"pusher:subscribe","data":{"channel":channel,"auth":auth}});
            ws.send(Message::text(sub.to_string())).await.unwrap();
            let answer = next_json(&mut ws, Duration::from_secs(5)).await.unwrap();
            assert_eq!(
                answer["event"], "pusher_internal:subscription_succeeded",
                "{answer}"
            );
        }
        clients.push(ws);
    }
    let (before, _) = rows(&env.db).await;
    let blob = "x".repeat(9_000);
    let started = std::time::Instant::now();
    let mut sent = 0;
    for second in 0..3u64 {
        for (i, ws) in clients.iter_mut().enumerate() {
            for n in 0..10 {
                let channel = format!("private-chat.{i}_{}", (n + second as usize) % channels);
                let frame = json!({"event":"client-typing","channel":channel,"data":{"b":blob}});
                if ws.send(Message::text(frame.to_string())).await.is_ok() {
                    sent += 1;
                }
            }
        }
        let wait = Duration::from_secs(second + 1).saturating_sub(started.elapsed());
        tokio::time::sleep(wait + Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (after, bytes) = rows(&env.db).await;
    let dropped = smeltery_core::pubsub::PubSub::of(&env.app)
        .unwrap()
        .dropped();
    let rows = after - before;
    assert!(sent >= 1_000, "{sent}");
    assert!(
        (1..=250).contains(&rows),
        "{rows} rows ({bytes} bytes) from one address in ~3.5 s; {dropped} dropped"
    );
}

/// Sweep W5-02: re-subscribing to a joined presence channel answers the member list from the store, so it costs a
/// presence join (5 at once, then 1 a second). Before: 38 lists in 2 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resubscribing_a_presence_channel_costs_a_join() {
    let env = start().await;
    let (mut ws, id) = connect(env.addr, &env.key).await;
    let channel = "presence-room.1";
    let data = r#"{"user_id":"u1"}"#;
    let auth = guest_auth(&env.key, &env.secret, &id, channel, Some(data));
    let sub = json!({"event":"pusher:subscribe","data":{"channel":channel,"auth":auth,"channel_data":data}});
    let started = std::time::Instant::now();
    for _ in 0..38 {
        ws.send(Message::text(sub.to_string())).await.unwrap();
    }
    let mut lists = 0;
    let mut refused = 0;
    while let Some(frame) = next_json(&mut ws, Duration::from_secs(2)).await {
        match frame["event"].as_str() {
            Some("pusher_internal:subscription_succeeded") => lists += 1,
            Some("pusher:subscription_error") => refused += 1,
            _ => {}
        }
    }
    assert!(
        lists <= 7 && refused >= 30,
        "38 subscribes in {:?}: {lists} member lists, {refused} refused",
        started.elapsed()
    );
}

/// Sweep W5-03: a member list larger than the old write-buffer cap (event size × 6 + message size + 64 KiB) could
/// not be written: the joining socket ended without an answer. The cap now fits the largest list the settings allow.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_member_list_reaches_the_joining_socket() {
    let mut s = Settings::from_env();
    s.env = "production".into();
    s.key = "anvil-abuse-tests-key-0123456789abcdef0123".into();
    s.url = "https://app.example.com".into();
    s.cache_store = "array".into();
    s.pubsub_driver = "local".into();
    s.log_level = "error".into();
    let mut anvil_settings = smeltery_anvil::Settings::from_env(&s);
    anvil_settings.max_presence_members = 2_000;
    anvil_settings.max_connections_per_ip = 0;
    let web = AppBuilder::new(s)
        .anvil_with(anvil_settings, |c| {
            c.presence("room.{r}", |_| async { Ok(Some(Member::new("u1"))) })
                .guests();
        })
        .build()
        .await
        .unwrap();
    let app = web.app.clone();
    let key = Anvil::of(&app).unwrap().app_key().to_owned();
    let secret = hex(&app.derive_key("anvil.secret").unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(smeltery_core::serve_on(web.app, web.router, listener));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let channel = "presence-room.1";
    let info = "n".repeat(250);
    let mut held = Vec::new();
    for n in 0..1_000 {
        let mut socket = smeltery_anvil::testing::TestSocket::connect(&app);
        let data =
            json!({"user_id": format!("member-{n}"), "user_info": {"name": info}}).to_string();
        let auth = guest_auth(&key, &secret, socket.socket_id(), channel, Some(&data));
        let answer = socket.subscribe_presence(channel, &auth, &data);
        assert_eq!(
            answer["event"], "pusher_internal:subscription_succeeded",
            "{answer}"
        );
        held.push(socket);
    }
    let (mut ws, id) = connect(addr, &key).await;
    let data = r#"{"user_id":"late"}"#;
    let auth = guest_auth(&key, &secret, &id, channel, Some(data));
    let sub = json!({"event":"pusher:subscribe","data":{"channel":channel,"auth":auth,"channel_data":data}});
    ws.send(Message::text(sub.to_string())).await.unwrap();
    let answer = next_json(&mut ws, Duration::from_secs(5)).await;
    let answer = answer.expect("an answer, not a dropped connection");
    assert_eq!(answer["event"], "pusher_internal:subscription_succeeded");
    let data: Value = serde_json::from_str(answer["data"].as_str().unwrap()).unwrap();
    assert_eq!(data["presence"]["count"], 1_001);
}
