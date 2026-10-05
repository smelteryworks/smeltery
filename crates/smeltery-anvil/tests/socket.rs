//! Real sockets on 127.0.0.1: the handshake checks, the Origin policy, the caps, delivery, the size and frame rules,
//! the server's per-client limit counting sockets, shutdown, slow readers, and payloads kept out of the log.
//!
//! Two tests connect from 127.0.0.2 as a second client: that needs the whole 127.0.0.0/8 on the loopback
//! interface, as on Linux and Windows; elsewhere (stock macOS) they print a skip line for that part.
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
use smeltery_anvil::{Anvil, AnvilExt as _, Channel};
use smeltery_core::config::Settings as CoreSettings;
use smeltery_core::{App, AppBuilder};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const ORIGIN: &str = "https://app.example.com";

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn core_settings() -> CoreSettings {
    let mut s = CoreSettings::from_env();
    // `serve_on` refuses APP_ENV=testing.
    s.env = "production".into();
    s.key = "anvil-socket-tests-key-0123456789abcdef".into();
    s.url = ORIGIN.into();
    s.cache_store = "array".into();
    s.database_url = String::new();
    s.pubsub_driver = "local".into();
    s.shutdown_timeout = Duration::from_secs(5);
    s.log_level = "warn".into();
    s
}

struct Server {
    app: App,
    addr: SocketAddr,
    key: String,
    task: JoinHandle<smeltery_core::Result<()>>,
}

impl Server {
    async fn start(core: CoreSettings, tune: impl FnOnce(&mut smeltery_anvil::Settings)) -> Self {
        let mut settings = smeltery_anvil::Settings::from_env(&core);
        tune(&mut settings);
        let built = AppBuilder::new(core)
            .anvil_with(settings, |c| {
                c.public("news");
            })
            .build()
            .await
            .unwrap();
        let app = built.app.clone();
        let key = Anvil::of(&app).unwrap().app_key().to_owned();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));
        Self {
            app,
            addr,
            key,
            task,
        }
    }

    fn url(&self, query: &str) -> String {
        format!("ws://{}/app/{}{query}", self.addr, self.key)
    }

    async fn stop(self) {
        self.app.shutdown();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("the server stops within its budget")
            .unwrap()
            .unwrap();
    }
}

async fn connect_with(url: &str, origin: Option<&str>) -> Result<Client, WsError> {
    let mut request = url.into_client_request().unwrap();
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(ws, _)| ws)
}

/// Connect with the app's origin and read `pusher:connection_established`; the socket id.
async fn connect(server: &Server) -> (Client, String) {
    let mut ws = connect_with(&server.url("?protocol=7&client=js"), Some(ORIGIN))
        .await
        .unwrap();
    let first = next_json(&mut ws).await;
    assert_eq!(first["event"], "pusher:connection_established", "{first}");
    let data: Value = serde_json::from_str(first["data"].as_str().unwrap()).unwrap();
    assert_eq!(data["activity_timeout"], 30);
    (ws, data["socket_id"].as_str().unwrap().to_owned())
}

/// The next text frame, parsed (5 s at most).
async fn next_json(ws: &mut Client) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a frame within 5 s")
            .expect("the socket is open")
            .expect("a frame");
        match message {
            Message::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

/// Read until the server closes; its close code.
async fn close_code(ws: &mut Client) -> Option<u16> {
    let wait = async {
        while let Some(message) = ws.next().await {
            match message {
                Ok(Message::Close(frame)) => return frame.map(|f| u16::from(f.code)),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    };
    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .expect("closed within 5 s")
}

async fn subscribe(ws: &mut Client, channel: &str) -> Value {
    let frame = json!({ "event": "pusher:subscribe", "data": { "channel": channel } });
    ws.send(Message::text(frame.to_string())).await.unwrap();
    next_json(ws).await
}

async fn wait_for(mut done: impl FnMut() -> bool) {
    for _ in 0..250 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the condition never held");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscribed_socket_receives_events_and_except_leaves_the_sender_out() {
    let server = Server::start(core_settings(), |_| {}).await;
    let (mut one, one_id) = connect(&server).await;
    let (mut two, _) = connect(&server).await;
    assert_eq!(
        subscribe(&mut one, "news").await["event"],
        "pusher_internal:subscription_succeeded"
    );
    assert_eq!(
        subscribe(&mut two, "news").await["event"],
        "pusher_internal:subscription_succeeded"
    );
    let refused = subscribe(&mut one, "undeclared").await;
    assert_eq!(refused["event"], "pusher:subscription_error");

    // pusher:ping is answered.
    one.send(Message::text(r#"{"event":"pusher:ping","data":{}}"#))
        .await
        .unwrap();
    assert_eq!(next_json(&mut one).await["event"], "pusher:pong");

    let anvil = Anvil::of(&server.app).unwrap();
    let delivered = anvil
        .to(Channel::public("news"))
        .event("posted")
        .with(&json!({ "id": 1 }))
        .except(smeltery_anvil::SocketId::parse(&one_id))
        .await
        .unwrap();
    assert_eq!(delivered.local, 1);
    let event = next_json(&mut two).await;
    assert_eq!(event["event"], "posted");
    assert_eq!(event["channel"], "news");
    assert_eq!(event["data"], r#"{"id":1}"#);
    // `one` got nothing: its next frame is the answer to a ping.
    one.send(Message::text(r#"{"event":"pusher:ping","data":{}}"#))
        .await
        .unwrap();
    assert_eq!(next_json(&mut one).await["event"], "pusher:pong");
    assert_eq!(anvil.connections(), 2);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_request_gets_426_and_handshakes_are_budgeted() {
    let server = Server::start(core_settings(), |s| s.handshakes_per_minute = 2).await;
    let plain = reqwest_free_get(server.addr, &format!("/app/{}", server.key)).await;
    assert!(plain.starts_with("HTTP/1.1 426"), "{plain}");
    let (_a, _) = connect(&server).await;
    let (_b, _) = connect(&server).await;
    match connect_with(&server.url("?protocol=7"), Some(ORIGIN)).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 429),
        other => panic!("expected 429, got {other:?}"),
    }
    server.stop().await;
}

/// A plain HTTP/1.1 GET; the response head.
async fn reqwest_free_get(addr: SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_origin_is_refused_and_a_missing_one_accepted() {
    let server = Server::start(core_settings(), |s| {
        s.allowed_origins = vec!["capacitor://localhost".into()];
    })
    .await;
    let mut foreign = connect_with(&server.url("?protocol=7"), Some("https://evil.example"))
        .await
        .unwrap();
    let error = next_json(&mut foreign).await;
    assert_eq!(error["event"], "pusher:error");
    assert_eq!(error["data"]["code"], 4009);
    assert_eq!(close_code(&mut foreign).await, Some(4009));

    let mut null = connect_with(&server.url("?protocol=7"), Some("null"))
        .await
        .unwrap();
    assert_eq!(next_json(&mut null).await["data"]["code"], 4009);

    for origin in [None, Some("capacitor://localhost"), Some(ORIGIN)] {
        let mut ws = connect_with(&server.url("?protocol=7"), origin)
            .await
            .unwrap();
        assert_eq!(
            next_json(&mut ws).await["event"],
            "pusher:connection_established",
            "{origin:?}"
        );
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_protocol_version_is_required() {
    let server = Server::start(core_settings(), |_| {}).await;
    let mut missing = connect_with(&server.url("?client=js"), Some(ORIGIN))
        .await
        .unwrap();
    assert_eq!(next_json(&mut missing).await["data"]["code"], 4008);
    assert_eq!(close_code(&mut missing).await, Some(4008));
    let mut old = connect_with(&server.url("?protocol=4"), Some(ORIGIN))
        .await
        .unwrap();
    assert_eq!(next_json(&mut old).await["data"]["code"], 4007);
    assert_eq!(close_code(&mut old).await, Some(4007));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_client_holds_at_most_its_share_of_sockets() {
    let server = Server::start(core_settings(), |s| s.max_connections_per_ip = 2).await;
    let (_a, _) = connect(&server).await;
    let (_b, _) = connect(&server).await;
    let mut third = connect_with(&server.url("?protocol=7"), Some(ORIGIN))
        .await
        .unwrap();
    let error = next_json(&mut third).await;
    assert_eq!(error["data"]["code"], 4100, "{error}");
    assert_eq!(close_code(&mut third).await, Some(4100));

    // Another client (127.0.0.2) still gets in.
    if !second_loopback() {
        server.stop().await;
        return;
    }
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.2:0".parse().unwrap()).unwrap();
    let stream = socket.connect(server.addr).await.unwrap();
    let mut request = server.url("?protocol=7").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", ORIGIN.parse().unwrap());
    let (mut other, _) = tokio_tungstenite::client_async(request, MaybeTlsStream::Plain(stream))
        .await
        .unwrap();
    assert_eq!(
        next_json(&mut other).await["event"],
        "pusher:connection_established"
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_process_holds_at_most_anvil_max_connections() {
    let server = Server::start(core_settings(), |s| s.max_connections = 2).await;
    let (_a, _) = connect(&server).await;
    let (_b, _) = connect(&server).await;
    let mut third = connect_with(&server.url("?protocol=7"), Some(ORIGIN))
        .await
        .unwrap();
    assert_eq!(next_json(&mut third).await["data"]["code"], 4100);
    assert_eq!(close_code(&mut third).await, Some(4100));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sockets_count_against_the_servers_per_client_limit() {
    // Two connections per client address: two open sockets leave no room for a third connection, which the
    // server closes right after accepting it. Without the upgrade hold the sockets would not count.
    let mut core = core_settings();
    core.server_max_connections_per_ip = 2;
    let server = Server::start(core, |s| s.max_connections_per_ip = 0).await;
    let (_a, _) = connect(&server).await;
    let (b, _) = connect(&server).await;
    let third = connect_with(&server.url("?protocol=7"), Some(ORIGIN)).await;
    assert!(third.is_err(), "the third connection is refused");
    drop(b);
    // The closed socket gives its place back.
    let mut ok = false;
    for _ in 0..100 {
        if connect_with(&server.url("?protocol=7"), Some(ORIGIN))
            .await
            .is_ok()
        {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ok, "a connection is accepted again once a socket closed");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn big_messages_and_binary_frames_close_the_socket() {
    let server = Server::start(core_settings(), |_| {}).await;
    let (mut big, _) = connect(&server).await;
    let text = format!(
        r#"{{"event":"pusher:ping","data":{{"pad":"{}"}}}}"#,
        "x".repeat(10_001)
    );
    let _ = big.send(Message::text(text)).await;
    assert_eq!(close_code(&mut big).await, Some(1009));

    let (mut binary, _) = connect(&server).await;
    binary.send(Message::binary(vec![1, 2, 3])).await.unwrap();
    assert_eq!(close_code(&mut binary).await, Some(1003));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_sockets_with_1001_within_the_budget() {
    let server = Server::start(core_settings(), |_| {}).await;
    let (mut ws, _) = connect(&server).await;
    subscribe(&mut ws, "news").await;
    let app = server.app.clone();
    let closing = tokio::spawn(async move { close_code(&mut ws).await });
    let started = std::time::Instant::now();
    server.stop().await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(closing.await.unwrap(), Some(1001));
    assert_eq!(Anvil::of(&app).unwrap().connections(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_socket_that_stops_reading_is_dropped() {
    let server = Server::start(core_settings(), |s| {
        s.outbox = 4;
        s.write_timeout = Duration::from_millis(500);
    })
    .await;
    let (mut ws, _) = connect(&server).await;
    subscribe(&mut ws, "news").await;
    let anvil = Anvil::of(&server.app).unwrap();
    assert_eq!(anvil.connections(), 1);
    // The client reads nothing more; the server's writes block once the TCP buffers are full.
    let data = json!({ "text": "y".repeat(30_000) });
    for _ in 0..2_000 {
        anvil
            .to(Channel::public("news"))
            .event("bulk")
            .with(&data)
            .await
            .unwrap();
        if anvil.connections() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let anvil2 = anvil.clone();
    wait_for(move || anvil2.connections() == 0).await;
    drop(ws);
    server.stop().await;
}

/// Whether 127.0.0.2 is a loopback address on this machine (the whole 127.0.0.0/8 is, on Linux
/// and Windows; stock macOS has only 127.0.0.1). A test that needs a second client prints a
/// skip line and returns when it is not, so `cargo test` stays honest there and the assertion
/// holds wherever the address exists.
fn second_loopback() -> bool {
    if std::net::TcpListener::bind("127.0.0.2:0").is_ok() {
        return true;
    }
    println!("skipped: 127.0.0.2 is not available on this machine");
    false
}

/// Connect from `local` (a loopback address) with these extra headers; the first frame.
async fn connect_from(server: &Server, local: &str, headers: &[(&str, &str)]) -> (Client, Value) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind(format!("{local}:0").parse().unwrap()).unwrap();
    let stream = socket.connect(server.addr).await.unwrap();
    let mut request = server.url("?protocol=7").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", ORIGIN.parse().unwrap());
    for (name, value) in headers {
        request.headers_mut().insert(
            http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    let (mut ws, _) = tokio_tungstenite::client_async(request, MaybeTlsStream::Plain(stream))
        .await
        .unwrap();
    let first = next_json(&mut ws).await;
    (ws, first)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn behind_a_trusted_proxy_each_forwarded_client_has_its_own_share() {
    let mut core = core_settings();
    core.trusted_proxies = "127.0.0.1".into();
    let server = Server::start(core, |s| s.max_connections_per_ip = 1).await;
    let established = "pusher:connection_established";
    let one = [("x-forwarded-for", "203.0.113.1")];
    let two = [("x-forwarded-for", "203.0.113.2")];
    let (_a, first) = connect_from(&server, "127.0.0.1", &one).await;
    assert_eq!(first["event"], established);
    let (_b, first) = connect_from(&server, "127.0.0.1", &two).await;
    assert_eq!(first["event"], established, "another forwarded client");
    let (mut again, first) = connect_from(&server, "127.0.0.1", &one).await;
    assert_eq!(first["data"]["code"], 4100, "{first}");
    assert_eq!(close_code(&mut again).await, Some(4100));

    // An untrusted peer's X-Forwarded-For is ignored: both sockets count for 127.0.0.2.
    if !second_loopback() {
        server.stop().await;
        return;
    }
    let (_c, first) =
        connect_from(&server, "127.0.0.2", &[("x-forwarded-for", "198.51.100.1")]).await;
    assert_eq!(first["event"], established);
    let (_d, first) =
        connect_from(&server, "127.0.0.2", &[("x-forwarded-for", "198.51.100.2")]).await;
    assert_eq!(first["data"]["code"], 4100, "{first}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn behind_a_trusted_proxy_handshakes_are_budgeted_per_forwarded_client() {
    let mut core = core_settings();
    core.trusted_proxies = "127.0.0.1".into();
    let server = Server::start(core, |s| s.handshakes_per_minute = 1).await;
    let one = [("x-forwarded-for", "203.0.113.1")];
    let (_a, first) = connect_from(&server, "127.0.0.1", &one).await;
    assert_eq!(first["event"], "pusher:connection_established");
    let (_b, first) =
        connect_from(&server, "127.0.0.1", &[("x-forwarded-for", "203.0.113.2")]).await;
    assert_eq!(first["event"], "pusher:connection_established");
    let mut request = server.url("?protocol=7").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("x-forwarded-for", "203.0.113.1".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 429),
        other => panic!("expected 429, got {other:?}"),
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_socket_is_pinged_and_closed_with_4201() {
    let server = Server::start(core_settings(), |s| {
        s.ping_interval = Duration::from_secs(1);
        s.pong_timeout = Duration::from_secs(1);
    })
    .await;
    // Answered: the socket stays.
    let (mut alive, _) = connect(&server).await;
    let ping = next_json(&mut alive).await;
    assert_eq!(ping["event"], "pusher:ping");
    alive
        .send(Message::text(r#"{"event":"pusher:pong","data":{}}"#))
        .await
        .unwrap();
    assert_eq!(
        next_json(&mut alive).await["event"],
        "pusher:ping",
        "pinged again, not closed"
    );
    // Silent: closed with 4201 one pong timeout after the ping.
    let (mut silent, _) = connect(&server).await;
    assert_eq!(next_json(&mut silent).await["event"], "pusher:ping");
    assert_eq!(close_code(&mut silent).await, Some(4201));
    server.stop().await;
}
