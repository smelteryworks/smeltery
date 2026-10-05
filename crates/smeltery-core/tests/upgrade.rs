//! Upgraded connections (WebSockets and other HTTP/1.1 upgrades) and the server's limits
//! (D-402): a route that takes the connection's [`UpgradeHold`] keeps its
//! `SERVER_MAX_CONNECTIONS` permit and its `SERVER_MAX_CONNECTIONS_PER_IP` slot for the life
//! of the upgraded socket, the idle rule never closes it, the shutdown drain waits for it within
//! the budget, a hold on an upgrade that never completes is released when the connection
//! ends, and HTTP/2 extended CONNECT is refused. Real sockets on 127.0.0.1.
//!
//! One test connects from 127.0.0.2 as a second client: that needs the whole 127.0.0.0/8 on
//! the loopback interface, as on Linux and Windows; elsewhere (stock macOS) it prints a skip
//! line for that part.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use hyper_util::rt::TokioIo;
use smeltery_core::config::Settings;
use smeltery_core::http::header;
use smeltery_core::{App, AppBuilder, UpgradeHold};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Set by the polite socket's task when it has finished closing.
static POLITE_CLOSED: AtomicBool = AtomicBool::new(false);

/// The `101 Switching Protocols` answer to an upgrade.
fn switching() -> Response {
    Response::builder()
        .status(101)
        .header(header::UPGRADE, "echo")
        .header(header::CONNECTION, "upgrade")
        .body(Body::empty())
        .unwrap()
}

/// Echo bytes on the upgraded socket until the client closes it (or `stop` resolves).
async fn echo_until(upgraded: hyper::upgrade::Upgraded, stop: impl Future<Output = ()>) {
    let mut io = TokioIo::new(upgraded);
    let mut buf = [0u8; 1024];
    tokio::pin!(stop);
    loop {
        tokio::select! {
            read = io.read(&mut buf) => match read {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if io.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            },
            () = &mut stop => {
                let _ = io.write_all(b"bye").await;
                let _ = io.flush().await;
                return;
            }
        }
    }
}

/// An echo socket that takes the connection's hold into its task.
async fn held(mut req: Request) -> Response {
    let hold = UpgradeHold::take(req.extensions_mut());
    assert!(hold.is_some(), "an HTTP/1.1 upgrade request offers a hold");
    let on = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        let _hold = hold;
        if let Ok(upgraded) = on.await {
            echo_until(upgraded, std::future::pending()).await;
        }
    });
    switching()
}

/// An echo socket that never takes the hold (as every route did before D-402).
async fn free(mut req: Request) -> Response {
    let on = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        if let Ok(upgraded) = on.await {
            echo_until(upgraded, std::future::pending()).await;
        }
    });
    switching()
}

/// A held socket that closes itself (after a short goodbye) when the app shuts down.
async fn polite(State(app): State<App>, mut req: Request) -> Response {
    let hold = UpgradeHold::take(req.extensions_mut());
    let on = hyper::upgrade::on(&mut req);
    let token = app.shutdown_token().clone();
    tokio::spawn(async move {
        let _hold = hold;
        if let Ok(upgraded) = on.await {
            echo_until(upgraded, async move {
                token.cancelled().await;
                // The closing handshake of a real protocol takes a moment.
                tokio::time::sleep(Duration::from_millis(500)).await;
            })
            .await;
        }
        POLITE_CLOSED.store(true, Ordering::SeqCst);
    });
    switching()
}

/// Before the `101`: a check that takes a moment (an authorization, a database lookup).
const BEFORE_101: Duration = Duration::from_millis(500);

/// Takes the hold, moves it into the task awaiting the upgrade, then checks for a moment
/// before the `101`.
async fn slow_spawned(mut req: Request) -> Response {
    let hold = UpgradeHold::take(req.extensions_mut());
    let on = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        let _hold = hold;
        if let Ok(upgraded) = on.await {
            echo_until(upgraded, std::future::pending()).await;
        }
    });
    tokio::time::sleep(BEFORE_101).await;
    switching()
}

/// Takes the hold and keeps it in the handler while it checks for a moment before the `101`.
async fn slow_inline(mut req: Request) -> Response {
    let hold = UpgradeHold::take(req.extensions_mut());
    let on = hyper::upgrade::on(&mut req);
    tokio::time::sleep(BEFORE_101).await;
    tokio::spawn(async move {
        let _hold = hold;
        if let Ok(upgraded) = on.await {
            echo_until(upgraded, std::future::pending()).await;
        }
    });
    switching()
}

/// [`slow_spawned`] without the hold.
async fn slow_free(mut req: Request) -> Response {
    let on = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        if let Ok(upgraded) = on.await {
            echo_until(upgraded, std::future::pending()).await;
        }
    });
    tokio::time::sleep(BEFORE_101).await;
    switching()
}

/// Whether this request offered a hold.
async fn probe(mut req: Request) -> &'static str {
    if UpgradeHold::take(req.extensions_mut()).is_some() {
        "held"
    } else {
        "none"
    }
}

async fn ok() -> &'static str {
    "ok"
}

/// A server on 127.0.0.1 with `adjust`ed settings: its address, the app and the server task.
async fn serve(
    adjust: impl FnOnce(&mut Settings),
) -> (
    SocketAddr,
    App,
    tokio::task::JoinHandle<smeltery_core::Result<()>>,
) {
    let mut settings = Settings::from_env();
    settings.env = "local".into();
    settings.key = "0123456789abcdef0123456789abcdef".into();
    settings.database_url = String::new();
    settings.shutdown_timeout = Duration::from_secs(2);
    settings.server_header_timeout = Duration::from_secs(10);
    adjust(&mut settings);
    let built = AppBuilder::new(settings)
        .api_routes(|r| {
            r.get("/hello", ok);
            r.get("/held", held);
            r.get("/free", free);
            r.get("/polite", polite);
            r.get("/probe", probe);
            r.get("/slow-spawned", slow_spawned);
            r.get("/slow-inline", slow_inline);
            r.get("/slow-free", slow_free);
        })
        .build()
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = built.app.clone();
    let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));
    (addr, app, server)
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

/// A TCP connection to `addr` from the loopback address `from` (127.0.0.x).
async fn connect_from(from: [u8; 4], addr: SocketAddr) -> tokio::net::TcpStream {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind(SocketAddr::from((from, 0))).unwrap();
    socket.connect(addr).await.unwrap()
}

/// The response head (or everything until the server closed or `wait` passed); `None` when
/// nothing came.
async fn read_head(stream: &mut tokio::net::TcpStream, wait: Duration) -> Option<String> {
    let mut seen = Vec::new();
    let mut byte = [0u8; 1];
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, stream.read(&mut byte)).await {
            Ok(Ok(0) | Err(_)) => break,
            Ok(Ok(_)) => {
                seen.push(byte[0]);
                if seen.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    (!seen.is_empty()).then(|| String::from_utf8_lossy(&seen).into_owned())
}

/// Whether the server closed the connection within `wait`.
async fn closed_within(stream: &mut tokio::net::TcpStream, wait: Duration) -> bool {
    let mut buf = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, stream.read(&mut buf)).await {
            Ok(Ok(0) | Err(_)) => return true,
            Ok(Ok(_)) => {}
            Err(_) => return false,
        }
    }
}

/// Upgrade a fresh connection from `from` on `path`; the socket after the `101`, and the head.
async fn upgrade(from: [u8; 4], addr: SocketAddr, path: &str) -> (tokio::net::TcpStream, String) {
    let mut stream = connect_from(from, addr).await;
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: upgrade\r\nUpgrade: echo\r\n\
                 Accept-Encoding: gzip, br\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let head = read_head(&mut stream, Duration::from_secs(3))
        .await
        .unwrap_or_default();
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    (stream, head)
}

/// Send `text` on an upgraded socket and expect it back.
async fn echoes(stream: &mut tokio::net::TcpStream, text: &str) {
    stream.write_all(text.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; text.len()];
    tokio::time::timeout(Duration::from_secs(3), stream.read_exact(&mut buf))
        .await
        .expect("echo within 3 s")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&buf), text);
}

/// A plain `GET /api/hello` on `stream`: the response head (or `None` when not served within
/// `wait`).
async fn hello(stream: &mut tokio::net::TcpStream, wait: Duration) -> Option<String> {
    stream
        .write_all(b"GET /api/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    read_head(stream, wait).await
}

/// Whether the client `from` gets `GET /api/hello` answered with 200 within `wait`, trying a
/// fresh connection every 100 ms (the server needs a moment to see a closed socket end).
async fn served_within(from: [u8; 4], addr: SocketAddr, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        let mut stream = connect_from(from, addr).await;
        if hello(&mut stream, Duration::from_millis(500))
            .await
            .is_some_and(|reply| reply.starts_with("HTTP/1.1 200"))
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upgraded_socket_keeps_its_client_slot() {
    let (addr, app, _server) = serve(|s| {
        s.server_max_connections = 8;
        s.server_max_connections_per_ip = 1;
    })
    .await;
    let (mut socket, _) = upgrade([127, 0, 0, 1], addr, "/api/held").await;
    echoes(&mut socket, "one").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // The socket still counts for its client: a second connection is closed at once.
    let mut second = connect_from([127, 0, 0, 1], addr).await;
    assert!(
        closed_within(&mut second, Duration::from_secs(2)).await,
        "the upgraded socket gave its client slot back"
    );
    // Another client is served.
    if !second_loopback() {
        app.shutdown();
        return;
    }
    let mut other = connect_from([127, 0, 0, 2], addr).await;
    let reply = hello(&mut other, Duration::from_secs(3)).await;
    assert!(reply.unwrap_or_default().starts_with("HTTP/1.1 200"));
    // When the socket ends, the slot comes back.
    echoes(&mut socket, "two").await;
    drop(socket);
    assert!(served_within([127, 0, 0, 1], addr, Duration::from_secs(3)).await);
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgraded_sockets_count_against_the_connection_cap() {
    let (addr, app, _server) = serve(|s| {
        s.server_max_connections = 2;
        s.server_max_connections_per_ip = 0;
    })
    .await;
    let (mut first, _) = upgrade([127, 0, 0, 1], addr, "/api/held").await;
    let (mut second, _) = upgrade([127, 0, 0, 1], addr, "/api/held").await;
    echoes(&mut first, "a").await;
    echoes(&mut second, "b").await;
    let mut third = connect_from([127, 0, 0, 1], addr).await;
    assert_eq!(
        hello(&mut third, Duration::from_millis(800)).await,
        None,
        "not served while both permits are held by sockets"
    );
    drop(first);
    let reply = read_head(&mut third, Duration::from_secs(3)).await;
    assert!(
        reply
            .as_deref()
            .unwrap_or_default()
            .starts_with("HTTP/1.1 200"),
        "{reply:?}"
    );
    echoes(&mut second, "still open").await;
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_route_that_does_not_take_the_hold_gives_the_slots_back_at_the_upgrade() {
    let (addr, app, _server) = serve(|s| {
        s.server_max_connections = 8;
        s.server_max_connections_per_ip = 1;
    })
    .await;
    let (mut socket, _) = upgrade([127, 0, 0, 1], addr, "/api/free").await;
    echoes(&mut socket, "free").await;
    assert!(served_within([127, 0, 0, 1], addr, Duration::from_secs(3)).await);
    echoes(&mut socket, "still").await;
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upgraded_socket_is_not_closed_by_the_header_or_idle_timeout() {
    let (addr, app, _server) = serve(|s| s.server_header_timeout = Duration::from_secs(1)).await;
    let (mut socket, head) = upgrade([127, 0, 0, 1], addr, "/api/held").await;
    // The 101 passes every layer unchanged: not compressed, its upgrade headers kept.
    let head = head.to_ascii_lowercase();
    assert!(!head.contains("content-encoding"), "{head}");
    assert!(head.contains("upgrade: echo"), "{head}");
    assert!(head.contains("connection: upgrade"), "{head}");
    echoes(&mut socket, "before").await;
    // Longer than SERVER_HEADER_TIMEOUT plus the close grace, silent.
    tokio::time::sleep(Duration::from_millis(2600)).await;
    echoes(&mut socket, "after").await;
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shutdown_drain_waits_for_held_sockets() {
    let (addr, app, server) = serve(|s| s.shutdown_timeout = Duration::from_secs(5)).await;
    let (mut socket, _) = upgrade([127, 0, 0, 1], addr, "/api/polite").await;
    echoes(&mut socket, "hi").await;
    app.shutdown();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server stops within the budget")
        .unwrap()
        .unwrap();
    assert!(
        POLITE_CLOSED.load(Ordering::SeqCst),
        "the server returned before the socket had closed"
    );
    let mut rest = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.read_to_end(&mut rest)).await;
    assert_eq!(String::from_utf8_lossy(&rest), "bye");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_socket_that_ignores_the_shutdown_does_not_outlast_the_budget() {
    let (addr, app, server) = serve(|s| s.shutdown_timeout = Duration::from_secs(1)).await;
    let (mut socket, _) = upgrade([127, 0, 0, 1], addr, "/api/held").await;
    echoes(&mut socket, "hi").await;
    let started = Instant::now();
    app.shutdown();
    tokio::time::timeout(Duration::from_secs(4), server)
        .await
        .expect("the server stops after the budget")
        .unwrap()
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

type H2Sender = hyper::client::conn::http2::SendRequest<http_body_util::Empty<bytes::Bytes>>;

async fn h2c(stream: tokio::net::TcpStream) -> H2Sender {
    let (sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        TokioIo::new(stream),
    )
    .await
    .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    sender
}

async fn h2_send(
    sender: &mut H2Sender,
    req: http::Request<http_body_util::Empty<bytes::Bytes>>,
) -> (u16, String) {
    let (status, _, body) = h2_send_full(sender, req).await;
    (status, body)
}

/// [`h2_send`] with the response headers.
async fn h2_send_full(
    sender: &mut H2Sender,
    req: http::Request<http_body_util::Empty<bytes::Bytes>>,
) -> (u16, http::HeaderMap, String) {
    sender.ready().await.unwrap();
    let res = sender.send_request(req).await.unwrap();
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let body = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http2_extended_connect_is_refused() {
    let (addr, app, _server) = serve(|_| {}).await;
    let mut sender = h2c(tokio::net::TcpStream::connect(addr).await.unwrap()).await;
    let get = |path: &str| {
        http::Request::builder()
            .uri(format!("http://127.0.0.1{path}"))
            .body(http_body_util::Empty::new())
            .unwrap()
    };
    // An HTTP/2 request offers no hold.
    assert_eq!(
        h2_send(&mut sender, get("/api/probe")).await,
        (200, "none".into())
    );
    let mut connect = http::Request::builder()
        .method(http::Method::CONNECT)
        .uri(format!("http://{addr}/api/held"))
        .body(http_body_util::Empty::new())
        .unwrap();
    connect
        .extensions_mut()
        .insert(hyper::ext::Protocol::from_static("websocket"));
    // The server does not offer extended CONNECT (no SETTINGS_ENABLE_CONNECT_PROTOCOL), so a
    // client that sends `:protocol` anyway gets its stream reset, never an answer, and the route
    // never runs (W1-03). `refuse_extended_connect` stays behind it (unit test below).
    sender.ready().await.unwrap();
    let answer = sender.send_request(connect).await;
    assert!(
        answer.is_err(),
        "extended CONNECT was answered: {:?}",
        answer.map(|r| r.status())
    );
    // The connection still serves requests.
    assert_eq!(
        h2_send(&mut sender, get("/api/hello")).await,
        (200, "ok".into())
    );
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_upgrade_requests_offer_a_hold() {
    let (addr, app, _server) = serve(|_| {}).await;
    let mut plain = tokio::net::TcpStream::connect(addr).await.unwrap();
    plain
        .write_all(b"GET /api/probe HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    plain.read_to_string(&mut reply).await.unwrap();
    assert!(
        reply.starts_with("HTTP/1.1 200") && reply.ends_with("none"),
        "{reply}"
    );
    let mut asking = tokio::net::TcpStream::connect(addr).await.unwrap();
    asking
        .write_all(
            b"GET /api/probe HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: upgrade\r\nUpgrade: echo\r\n\r\n",
        )
        .await
        .unwrap();
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), asking.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let reply = String::from_utf8_lossy(&buf[..n]);
    assert!(
        reply.starts_with("HTTP/1.1 200") && reply.ends_with("held"),
        "{reply}"
    );
    app.shutdown();
}

/// A client asks for an upgrade on `path` and goes away (FIN, or RST when `reset`) while the
/// route is still checking, before the `101`; afterwards the same client must be served again
/// (`SERVER_MAX_CONNECTIONS_PER_IP` = 1): the hold of an upgrade that never completed was
/// released when the connection ended.
async fn vanish_before_the_101(path: &str, reset: bool) {
    let (addr, app, _server) = serve(|s| {
        s.server_max_connections = 8;
        s.server_max_connections_per_ip = 1;
        s.server_header_timeout = Duration::from_secs(2);
    })
    .await;
    assert!(served_within([127, 0, 0, 1], addr, Duration::from_secs(3)).await);
    let stream = held_upgrade(addr, path, Duration::from_secs(5)).await;
    if reset {
        // A zero linger makes the drop send RST; it does not block (only a non-zero one does).
        #[allow(deprecated)]
        stream.set_linger(Some(Duration::ZERO)).unwrap();
    }
    drop(stream);
    assert!(
        served_within([127, 0, 0, 1], addr, Duration::from_secs(5)).await,
        "{path} (reset: {reset}): the client slot was never given back"
    );
    app.shutdown();
}

/// A connection from 127.0.0.1 whose upgrade request on `path` the server is holding (the route is
/// still checking): the request was written and, 100 ms later, the connection is still open. The
/// server releases the probe's slot (`served_within`) only when its task sees that connection
/// end, so a connection it closes at accept (nothing readable, then EOF or a reset) is dropped
/// and tried again until `wait` passes. Without this the hold would never be taken, the test
/// would pass without proving anything, and on macOS a `setsockopt` on the reset socket fails.
async fn held_upgrade(addr: SocketAddr, path: &str, wait: Duration) -> tokio::net::TcpStream {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: upgrade\r\nUpgrade: echo\r\n\r\n"
    );
    let deadline = Instant::now() + wait;
    loop {
        let mut stream = connect_from([127, 0, 0, 1], addr).await;
        let written = stream.write_all(request.as_bytes()).await.is_ok();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut byte = [0u8; 1];
        let probe = tokio::time::timeout(Duration::from_millis(50), stream.peek(&mut byte)).await;
        // Still pending after 50 ms: open and unanswered, so the route holds it.
        if written && probe.is_err() {
            return stream;
        }
        assert!(
            Instant::now() < deadline,
            "{path}: the server never kept the upgrade connection open (last probe: {probe:?})"
        );
        drop(stream);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hold_in_the_upgrade_task_is_released_when_the_client_leaves_before_the_101() {
    vanish_before_the_101("/api/slow-spawned", false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hold_in_the_upgrade_task_is_released_when_the_client_resets_before_the_101() {
    vanish_before_the_101("/api/slow-spawned", true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hold_in_the_handler_is_released_when_the_client_leaves_before_the_101() {
    vanish_before_the_101("/api/slow-inline", false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hold_in_the_handler_is_released_when_the_client_resets_before_the_101() {
    vanish_before_the_101("/api/slow-inline", true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_hold_a_client_leaving_before_the_101_frees_its_slot() {
    vanish_before_the_101("/api/slow-free", false).await;
}
