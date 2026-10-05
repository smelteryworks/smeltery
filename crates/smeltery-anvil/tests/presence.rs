//! Presence channels and client events on real sockets (127.0.0.1): member lists, `member_added` /
//! `member_removed` across tabs, abrupt disconnects, revocation and processes; whispers to the others only, opt-in
//! per pattern, rate-limited.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use smeltery_anvil::{Anvil, AnvilExt as _, ChannelCtx, Channels, Member};
use smeltery_core::auth::{AuthEvent, Credential, Guard, Principal, publish_event};
use smeltery_core::config::Settings;
use smeltery_core::http::request::Parts;
use smeltery_core::{App, AppBuilder, BoxFuture, Result};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// `X-Test-Token: <user>:<token id>` with the `broadcasting` ability (a test double).
struct FakeTokens;

impl Guard for FakeTokens {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn stateless(&self) -> bool {
        true
    }

    fn authenticate<'a>(
        &'a self,
        _app: &'a App,
        parts: &'a mut Parts,
    ) -> BoxFuture<'a, Result<Option<Principal>>> {
        let header = parts
            .headers
            .get("x-test-token")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Box::pin(async move {
            Ok(header.map(|h| {
                let (user, id) = h.split_once(':').unwrap();
                Principal::new(
                    user.parse().unwrap(),
                    "fake",
                    Credential::token(id.parse::<i64>().unwrap(), ["broadcasting"]),
                )
            }))
        })
    }
}

fn channels(c: &mut Channels) {
    c.presence("room.{room}", |ctx: ChannelCtx| async move {
        Ok(ctx
            .user_id()
            .map(|id| Member::new(id).info(json!({ "name": format!("user {id}") }))))
    })
    .whispers();
    c.private("chat.{chat}", |_| async { Ok(true) }).whispers();
    c.private("quiet.{q}", |_| async { Ok(true) });
}

fn build(b: AppBuilder) -> AppBuilder {
    b.guard(FakeTokens).anvil(channels)
}

fn settings(database: &str, driver: &str) -> Settings {
    let mut s = Settings::from_env();
    s.env = "production".into();
    s.key = "anvil-presence-key-0123456789abcdef".into();
    s.url = "http://127.0.0.1".into();
    s.database_url = database.into();
    s.cache_store = "array".into();
    s.session_driver = "cookie".into();
    s.pubsub_driver = driver.into();
    s.pubsub_poll_interval = Duration::from_millis(20);
    s.shutdown_timeout = Duration::from_secs(5);
    s.log_level = "warn".into();
    s
}

/// A serving app on 127.0.0.1: its address, app key, app and server task.
async fn serve(
    settings: Settings,
) -> (SocketAddr, String, App, tokio::task::JoinHandle<Result<()>>) {
    let built = build(AppBuilder::new(settings)).build().await.unwrap();
    let app = built.app.clone();
    let key = Anvil::of(&app).unwrap().app_key().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));
    (addr, key, app, server)
}

async fn stop(app: App, server: tokio::task::JoinHandle<Result<()>>) {
    app.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .expect("serve stops")
        .unwrap()
        .unwrap();
}

/// A plain HTTP/1.1 POST; the status and the body.
async fn post(addr: SocketAddr, path: &str, token: &str, body: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nX-Test-Token: {token}\r\n\
         Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut out))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_owned())
}

/// The JSON object in a (possibly chunked) body.
fn json_in(body: &str) -> Value {
    let start = body.find('{').unwrap();
    let end = body.rfind('}').unwrap();
    serde_json::from_str(&body[start..=end]).unwrap()
}

async fn next_json(ws: &mut Client) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("a frame within 10 s")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

/// The next frame named `event`.
async fn next_event(ws: &mut Client, event: &str) -> Value {
    loop {
        let frame = next_json(ws).await;
        if frame["event"] == event {
            return frame;
        }
    }
}

/// Nothing arrives within `wait` (pings answered by the library are not frames here).
async fn quiet(ws: &mut Client, wait: Duration) -> Option<Value> {
    tokio::time::timeout(wait, next_json(ws)).await.ok()
}

/// `data` of a server frame (a JSON string), parsed.
fn data(frame: &Value) -> Value {
    serde_json::from_str(frame["data"].as_str().unwrap()).unwrap()
}

async fn connect(addr: SocketAddr, key: &str) -> (Client, String) {
    let request = format!("ws://{addr}/app/{key}?protocol=7")
        .into_client_request()
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let first = next_json(&mut ws).await;
    let socket_id = data(&first)["socket_id"].as_str().unwrap().to_owned();
    (ws, socket_id)
}

/// The auth endpoint's answer for `channel` as user `user` (token `token`).
async fn auth(addr: SocketAddr, user: i64, token: i64, socket: &str, channel: &str) -> Value {
    let (status, body) = post(
        addr,
        "/api/broadcasting/auth",
        &format!("{user}:{token}"),
        &format!("socket_id={socket}&channel_name={channel}"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    json_in(&body)
}

/// A socket of user `user` subscribed to `channel` (presence or private); the subscription answer.
async fn join(
    addr: SocketAddr,
    key: &str,
    user: i64,
    token: i64,
    channel: &str,
) -> (Client, Value) {
    let (mut ws, socket) = connect(addr, key).await;
    let answer = auth(addr, user, token, &socket, channel).await;
    let mut subscribe = json!({ "channel": channel, "auth": answer["auth"] });
    if let Some(data) = answer.get("channel_data") {
        subscribe["channel_data"] = data.clone();
    }
    ws.send(Message::text(
        json!({ "event": "pusher:subscribe", "data": subscribe }).to_string(),
    ))
    .await
    .unwrap();
    let frame = next_json(&mut ws).await;
    (ws, frame)
}

fn ids(succeeded: &Value) -> Vec<String> {
    let mut ids: Vec<String> = data(succeeded)["presence"]["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

const ROOM: &str = "presence-room.1";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_come_and_go_once_per_user() {
    let (addr, key, app, server) = serve(settings("", "local")).await;

    let (mut ada, first) = join(addr, &key, 7, 1, ROOM).await;
    assert_eq!(first["event"], "pusher_internal:subscription_succeeded");
    let presence = &data(&first)["presence"];
    assert_eq!(presence["ids"], json!(["7"]));
    assert_eq!(presence["hash"]["7"], json!({ "name": "user 7" }));
    assert_eq!(presence["count"], 1);

    // Bob joins: Ada sees him; Bob gets both.
    let (mut bob, second) = join(addr, &key, 8, 2, ROOM).await;
    assert_eq!(ids(&second), ["7", "8"]);
    let added = next_event(&mut ada, "pusher_internal:member_added").await;
    assert_eq!(added["channel"], ROOM);
    assert_eq!(
        data(&added),
        json!({ "user_id": "8", "user_info": { "name": "user 8" } })
    );
    assert!(
        quiet(&mut bob, Duration::from_millis(300)).await.is_none(),
        "the joining socket gets the list, not its own member_added"
    );

    // A second tab of Bob: no announcement; it leaving abruptly: none either.
    let (bob_tab, _) = join(addr, &key, 8, 2, ROOM).await;
    assert!(quiet(&mut ada, Duration::from_millis(300)).await.is_none());
    drop(bob_tab);
    assert!(quiet(&mut ada, Duration::from_millis(500)).await.is_none());
    assert_eq!(
        Anvil::of(&app).unwrap().members(ROOM).await.unwrap().len(),
        2
    );

    // Bob's last socket drops without a close frame: Ada sees him go.
    drop(bob);
    let removed = next_event(&mut ada, "pusher_internal:member_removed").await;
    assert_eq!(data(&removed), json!({ "user_id": "8" }));
    assert_eq!(
        Anvil::of(&app).unwrap().members(ROOM).await.unwrap(),
        vec![Member::new(7).info(json!({ "name": "user 7" }))]
    );

    // Unsubscribing leaves too.
    let (mut carl, _) = join(addr, &key, 9, 3, ROOM).await;
    next_event(&mut ada, "pusher_internal:member_added").await;
    carl.send(Message::text(
        json!({ "event": "pusher:unsubscribe", "data": { "channel": ROOM } }).to_string(),
    ))
    .await
    .unwrap();
    let removed = next_event(&mut ada, "pusher_internal:member_removed").await;
    assert_eq!(data(&removed)["user_id"], "9");

    stop(app, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whispers_reach_the_others_and_never_the_sender() {
    let (addr, key, app, server) = serve(settings("", "local")).await;

    let (mut a, _) = join(addr, &key, 7, 1, "private-chat.1").await;
    let (mut b, _) = join(addr, &key, 8, 2, "private-chat.1").await;
    a.send(Message::text(
        json!({ "event": "client-typing", "channel": "private-chat.1", "data": { "typing": true } }).to_string(),
    ))
    .await
    .unwrap();
    let got = next_event(&mut b, "client-typing").await;
    assert_eq!(got["channel"], "private-chat.1");
    assert_eq!(got["data"], json!({ "typing": true }), "as sent");
    assert!(
        got.get("user_id").is_none(),
        "no user id on private channels"
    );
    assert!(
        quiet(&mut a, Duration::from_millis(300)).await.is_none(),
        "never echoed"
    );

    // A pattern without `.whispers()`.
    let (mut q, _) = join(addr, &key, 7, 1, "private-quiet.1").await;
    q.send(Message::text(
        json!({ "event": "client-typing", "channel": "private-quiet.1", "data": {} }).to_string(),
    ))
    .await
    .unwrap();
    let refused = next_event(&mut q, "pusher:error").await;
    assert_eq!(refused["data"]["code"], 4009);

    // Presence: the sender's user id goes along.
    let (mut p1, _) = join(addr, &key, 7, 1, ROOM).await;
    let (mut p2, _) = join(addr, &key, 8, 2, ROOM).await;
    next_event(&mut p1, "pusher_internal:member_added").await;
    p2.send(Message::text(
        json!({ "event": "client-wave", "channel": ROOM, "data": "hi" }).to_string(),
    ))
    .await
    .unwrap();
    let wave = next_event(&mut p1, "client-wave").await;
    assert_eq!(
        (wave["data"].clone(), wave["user_id"].clone()),
        (json!("hi"), json!("8"))
    );

    // At most 10 a second per socket.
    for n in 0..11 {
        a.send(Message::text(
            json!({ "event": "client-n", "channel": "private-chat.1", "data": n }).to_string(),
        ))
        .await
        .unwrap();
    }
    let limited = next_event(&mut a, "pusher:error").await;
    assert_eq!(limited["data"]["code"], 4301);
    let mut received = 0;
    while let Some(frame) = quiet(&mut b, Duration::from_millis(500)).await {
        if frame["event"] == "client-n" {
            received += 1;
        }
    }
    assert_eq!(received, 10);

    stop(app, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_member_is_removed() {
    let (addr, key, app, server) = serve(settings("", "local")).await;
    let (mut ada, _) = join(addr, &key, 7, 1, ROOM).await;
    let (mut bob, _) = join(addr, &key, 8, 9, ROOM).await;
    next_event(&mut ada, "pusher_internal:member_added").await;

    publish_event(
        &app,
        &AuthEvent::Revoked {
            user_id: 8,
            key: "fake:token:9".into(),
        },
    )
    .await
    .unwrap();
    let closed = loop {
        match tokio::time::timeout(Duration::from_secs(10), bob.next())
            .await
            .unwrap()
        {
            Some(Ok(Message::Close(frame))) => break frame.map(|f| u16::from(f.code)),
            Some(Ok(_)) => {}
            _ => break None,
        }
    };
    assert_eq!(closed, Some(4200));
    let removed = next_event(&mut ada, "pusher_internal:member_removed").await;
    assert_eq!(data(&removed)["user_id"], "8");

    stop(app, server).await;
}

/// Two serving processes on one SQLite file with the `database` PubSub driver.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presence_crosses_processes_on_the_database_driver() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path()
            .join("db.sqlite")
            .display()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "/")
    );
    let db = smeltery_core::db::Db::connect(&url).await.unwrap();
    let schema = smeltery_core::db::migration::Schema::new(&db);
    smeltery_core::pubsub::migrations::up(&schema)
        .await
        .unwrap();
    smeltery_anvil::presence_migrations::up(&schema)
        .await
        .unwrap();
    cross_process(settings(&url, "database")).await;
}

/// The same with the `redis` PubSub driver and presence store.
#[cfg(feature = "redis")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs REDIS_URL"]
async fn presence_crosses_processes_on_redis() {
    let mut s = settings("", "redis");
    s.redis_url = std::env::var("REDIS_URL").expect("REDIS_URL");
    s.cache_prefix = format!("anvil_presence_test_{}_", std::process::id());
    cross_process(s).await;
}

async fn cross_process(settings: Settings) {
    let (addr_a, key, app_a, server_a) = serve(settings.clone()).await;
    let (addr_b, _, app_b, server_b) = serve(settings).await;

    let (mut ada, first) = join(addr_a, &key, 7, 1, ROOM).await;
    assert_eq!(ids(&first), ["7"]);
    let (mut bob, second) = join(addr_b, &key, 8, 2, ROOM).await;
    assert_eq!(ids(&second), ["7", "8"], "process B sees A's member");
    let added = next_event(&mut ada, "pusher_internal:member_added").await;
    assert_eq!(data(&added)["user_id"], "8");
    assert_eq!(
        Anvil::of(&app_a)
            .unwrap()
            .members(ROOM)
            .await
            .unwrap()
            .len(),
        2
    );

    // A whisper from B reaches A, not back to B.
    bob.send(Message::text(
        json!({ "event": "client-wave", "channel": ROOM, "data": { "hi": 1 } }).to_string(),
    ))
    .await
    .unwrap();
    let wave = next_event(&mut ada, "client-wave").await;
    assert_eq!(wave["user_id"], "8");
    // Never echoed. (A `member_added` for Ada may arrive late: process A announced her while Bob was joining on B,
    // and B's poll delivered it after Bob's join; it names a member already in Bob's list.)
    while let Some(frame) = quiet(&mut bob, Duration::from_millis(500)).await {
        assert_ne!(frame["event"], "client-wave", "{frame}");
    }

    // Bob's socket on B drops: A sees him go.
    drop(bob);
    let removed = next_event(&mut ada, "pusher_internal:member_removed").await;
    assert_eq!(data(&removed)["user_id"], "8");

    stop(app_b, server_b).await;
    stop(app_a, server_a).await;
}
