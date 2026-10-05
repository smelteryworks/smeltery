//! What the server does with hostile clients: parallel password guesses, slow and silent
//! connections, a flood of connections, a multipart body that never ends, and a server
//! started under `APP_ENV=testing`. Real sockets on 127.0.0.1 where it matters.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use serde::Deserialize;
use smeltery_core::auth::Auth;
use smeltery_core::config::Settings;
use smeltery_core::db::migration::{Migration, Migrator, Schema};
use smeltery_core::http::{Form, HeaderMap, HeaderValue, Method, UploadedFile, header};
use smeltery_core::testing::TestApp;
use smeltery_core::validation::{Valid, Validate, ValidationContext, ValidationErrors};
use smeltery_core::{AppBuilder, Result};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tower::ServiceExt as _;

mod user {
    //! The `User` model (table `users`).
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub email: String,
        pub password: String,
        pub remember_token: Option<String>,
        pub created_at: Option<DateTimeUtc>,
        pub updated_at: Option<DateTimeUtc>,
    }

    impl ActiveModelBehavior for ActiveModel {}

    impl smeltery_core::auth::Authenticatable for Model {
        fn auth_id(&self) -> i64 {
            self.id
        }
        fn password_hash(&self) -> &str {
            &self.password
        }
        fn remember_token(&self) -> Option<&str> {
            self.remember_token.as_deref()
        }
    }
}

use user::Model as User;

struct CreateUsers;

impl Migration for CreateUsers {
    fn name(&self) -> &'static str {
        "2026_10_05_000001_create_users"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("users", |t| {
                t.id();
                t.string("email").unique();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("users").await
    }
}

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
}

async fn login(auth: Auth, Form(f): Form<Login>) -> Result<&'static str> {
    Ok(if auth.attempt(&f.email, &f.password, false).await? {
        "in"
    } else {
        "out"
    })
}

async fn ok() -> &'static str {
    "ok"
}

#[derive(Debug, Deserialize)]
struct UploadForm {
    #[allow(dead_code)]
    file: Option<UploadedFile>,
}

impl Validate for UploadForm {
    async fn validate(
        &self,
        _ctx: &ValidationContext<'_>,
    ) -> std::result::Result<(), ValidationErrors> {
        Ok(())
    }
}

async fn upload(Valid(_form): Valid<UploadForm>) -> &'static str {
    "read"
}

fn routes(app: AppBuilder) -> AppBuilder {
    app.migrations(|m: &mut Migrator| {
        m.add(CreateUsers);
    })
    .auth::<User>()
    .routes(|r| {
        r.post("/login", login);
        r.get("/page", ok);
    })
    .api_routes(|r| {
        r.post("/form", ok);
        r.post("/upload", upload);
    })
}

// ---- S1-02: parallel guesses ----------------------------------------------------------

/// The outcome of `count` concurrent wrong-password logins for `ada@example.com`, client `i`
/// coming from `addr(i)`: (`"out"` answers, 429 answers).
fn parallel_logins(count: usize, addr: fn(usize) -> SocketAddr) -> (usize, usize) {
    parallel_logins_as(count, addr, |_| "ada@example.com".to_owned(), None).0
}

/// [`parallel_logins`] with the address of attempt `i` from `email(i)`, then, when `last` is
/// given, one more login (after the burst) with the right password from that client: its
/// status and body.
fn parallel_logins_as(
    count: usize,
    addr: fn(usize) -> SocketAddr,
    email: fn(usize) -> String,
    last: Option<SocketAddr>,
) -> ((usize, usize), Option<(u16, String)>) {
    // The password-hash gate is process-wide (`HASH_QUEUE`): bursts of these tests running at
    // once would get 503s from each other.
    static ONE_BURST_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = ONE_BURST_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let mut settings = Settings::from_env();
        settings.env = "testing".into();
        settings.database_url = "sqlite::memory:".into();
        let built = routes(AppBuilder::new(settings)).build().await.unwrap();
        let db = built.app.db().unwrap();
        built.app.migrator().fresh(&db).await.unwrap();
        let hash = smeltery_core::auth::hash_password("correct horse")
            .await
            .unwrap();
        db.execute(&format!(
            "INSERT INTO users (email, password) VALUES ('ada@example.com', '{hash}')"
        ))
        .await
        .unwrap();
        let send = |router: axum::Router, from: SocketAddr, email: String, password: String| async move {
            let body = serde_urlencoded::to_string([("email", email), ("password", password)])
                .unwrap();
            let mut req = http::Request::builder()
                .method(Method::POST)
                .uri("/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ACCEPT, "application/json")
                .body(Body::from(body))
                .unwrap();
            req.extensions_mut()
                .insert(axum::extract::ConnectInfo(from));
            let res = router.oneshot(req).await.unwrap();
            let status = res.status().as_u16();
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, String::from_utf8_lossy(&body).into_owned())
        };
        let start = Arc::new(tokio::sync::Barrier::new(count));
        let mut tasks = tokio::task::JoinSet::new();
        for i in 0..count {
            let router = built.router.clone();
            let start = Arc::clone(&start);
            let attempt = send(router, addr(i), email(i), format!("guess{i}"));
            tasks.spawn(async move {
                start.wait().await;
                attempt.await
            });
        }
        let (mut out, mut throttled) = (0, 0);
        while let Some(result) = tasks.join_next().await {
            match result.unwrap() {
                (200, body) if body == "out" => out += 1,
                (429, _) => throttled += 1,
                other => panic!("unexpected answer {other:?}"),
            }
        }
        let last = match last {
            Some(from) => Some(
                send(
                    built.router.clone(),
                    from,
                    "ada@example.com".into(),
                    "correct horse".into(),
                )
                .await,
            ),
            None => None,
        };
        ((out, throttled), last)
    })
}

#[test]
fn parallel_guesses_from_one_client_get_exactly_five_password_checks() {
    let (checked, throttled) = parallel_logins(40, |i| {
        SocketAddr::from(([127, 0, 0, 1], 40_000 + u16::try_from(i).unwrap()))
    });
    assert_eq!((checked, throttled), (5, 35));
}

#[test]
fn parallel_guesses_from_many_clients_share_the_accounts_budget() {
    // Forty clients of one /24 network share the address's budget for that network.
    let (checked, throttled) = parallel_logins(40, |i| {
        SocketAddr::from(([10, 0, 0, u8::try_from(i + 1).unwrap()], 40_000))
    });
    assert_eq!((checked, throttled), (20, 20));
}

/// R-3: guesses from other networks never lock out the account's owner.
#[test]
fn guesses_from_other_networks_do_not_lock_the_owner_out() {
    let (counts, last) = parallel_logins_as(
        40,
        |i| SocketAddr::from(([10, 0, u8::try_from(i + 1).unwrap(), 1], 40_000)),
        |_| "ada@example.com".to_owned(),
        Some(SocketAddr::from(([192, 0, 2, 1], 40_000))),
    );
    assert_eq!(counts, (40, 0), "each network has its own budget");
    assert_eq!(
        last,
        Some((200, "in".to_owned())),
        "the owner still signs in"
    );
}

/// R-3: an address without an account is refused at exactly the same point as one with an
/// account, so the 429 tells nothing about which addresses have one.
#[test]
fn the_address_budget_does_not_reveal_whether_an_account_exists() {
    let from = |i: usize| SocketAddr::from(([10, 0, 0, u8::try_from(i + 1).unwrap()], 40_000));
    let known = parallel_logins_as(21, from, |_| "ada@example.com".to_owned(), None).0;
    let unknown = parallel_logins_as(21, from, |_| "nobody@example.com".to_owned(), None).0;
    assert_eq!(known, (20, 1));
    assert_eq!(unknown, known);
}

/// R-3: one client trying many addresses is limited too (30 a minute).
#[test]
fn one_client_is_limited_across_addresses() {
    let (counts, _) = parallel_logins_as(
        40,
        |_| SocketAddr::from(([10, 0, 0, 1], 40_000)),
        |i| format!("user{i}@example.com"),
        None,
    );
    assert_eq!(counts, (30, 10));
}

// ---- S1-04: a multipart body whose part header never ends -----------------------------

#[test]
fn an_endless_multipart_header_is_refused_without_reading_on() {
    let app = TestApp::new(|b| {
        let mut b = routes(b);
        b.settings_mut().upload_max_bytes = 64 * 1024;
        b.settings_mut().body_limit = 64 * 1024;
        b.settings_mut().request_timeout = Duration::from_secs(5);
        b
    });
    let pulled = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&pulled);
    let head = b"--XyZ\r\nContent-Disposition: form-data; name=\"title\"\r\nX-Junk: ".to_vec();
    let stream = futures_util::stream::unfold(Some(head), move |head| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let chunk = head.unwrap_or_else(|| vec![b'a'; 64 * 1024]);
            Some((Ok::<_, std::io::Error>(bytes::Bytes::from(chunk)), None))
        }
    });
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("multipart/form-data; boundary=XyZ"),
    );
    let started = Instant::now();
    let res = app.request(
        Method::POST,
        "/api/upload",
        headers,
        Body::from_stream(stream),
    );
    assert_eq!(res.status(), 413, "{}", res.text());
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    // At most the limit (64 KiB of data plus 1 MiB of headers) and what the peeks read first.
    let pulled = pulled.load(Ordering::SeqCst);
    assert!(pulled <= 24, "{pulled} chunks of 64 KiB");
}

// ---- S1-05: slow and silent clients, too many connections ----------------------------

/// A server on 127.0.0.1 with `adjust`ed settings; its address and the app.
async fn serve(adjust: impl FnOnce(&mut Settings)) -> (SocketAddr, smeltery_core::App) {
    let mut settings = Settings::from_env();
    settings.env = "local".into();
    settings.key = "0123456789abcdef0123456789abcdef".into();
    settings.database_url = String::new();
    settings.shutdown_timeout = Duration::from_secs(2);
    adjust(&mut settings);
    let built = AppBuilder::new(settings)
        .api_routes(|r| {
            r.get("/hello", ok);
            r.post("/form", ok);
            r.get("/slow", slow);
            r.get("/park", park);
        })
        .build()
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = built.app.clone();
    tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));
    (addr, app)
}

/// Everything the server sends until it closes or `wait` passes; `None` when nothing came.
async fn read_reply(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    wait: Duration,
) -> Option<String> {
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, stream.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => break,
            Ok(Ok(n)) => {
                seen.extend_from_slice(&buf[..n]);
                if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return (!seen.is_empty()).then(|| String::from_utf8_lossy(&seen).into()),
        }
    }
    Some(String::from_utf8_lossy(&seen).into_owned())
}

/// Whether the server closed the connection within `wait` (a read sees the end or an error).
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_head_sent_too_slowly_is_cut_off() {
    let (addr, app) = serve(|s| s.server_header_timeout = Duration::from_secs(1)).await;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /api/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let _ = stream.write_all(b"X-Late: 1\r\n\r\n").await;
    let reply = read_reply(&mut stream, Duration::from_secs(3))
        .await
        .unwrap_or_default();
    assert!(!reply.contains("200 OK"), "{reply}");
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_that_sends_nothing_is_closed() {
    let (addr, app) = serve(|s| s.server_header_timeout = Duration::from_secs(1)).await;
    let mut silent = tokio::net::TcpStream::connect(addr).await.unwrap();
    assert!(closed_within(&mut silent, Duration::from_secs(4)).await);
    // An idle keep-alive connection is closed the same way after a request.
    let mut kept = tokio::net::TcpStream::connect(addr).await.unwrap();
    kept.write_all(b"GET /api/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let reply = read_reply(&mut kept, Duration::from_secs(3)).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(closed_within(&mut kept, Duration::from_secs(4)).await);
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_form_body_gets_a_408_within_the_request_timeout() {
    let (addr, app) = serve(|s| s.request_timeout = Duration::from_secs(1)).await;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let started = Instant::now();
    stream
        .write_all(
            b"POST /api/form HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 20\r\n\r\n",
        )
        .await
        .unwrap();
    let (mut reader, mut writer) = stream.into_split();
    let trickle = tokio::spawn(async move {
        for _ in 0..20 {
            if writer.write_all(b"a").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
    let reply = read_reply(&mut reader, Duration::from_secs(3))
        .await
        .unwrap_or_default();
    trickle.abort();
    assert!(
        reply.starts_with("HTTP/1.1 408"),
        "{reply} after {:?}",
        started.elapsed()
    );
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connections_beyond_the_cap_wait_to_be_accepted() {
    let (addr, app) = serve(|s| {
        s.server_max_connections = 1;
        s.server_header_timeout = Duration::from_secs(10);
    })
    .await;
    let holder = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut waiting = tokio::net::TcpStream::connect(addr).await.unwrap();
    waiting
        .write_all(b"GET /api/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(
        read_reply(&mut waiting, Duration::from_millis(800)).await,
        None,
        "not served while the only slot is taken"
    );
    drop(holder);
    let reply = read_reply(&mut waiting, Duration::from_secs(3))
        .await
        .unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    app.shutdown();
}

// ---- R-1: idle HTTP/2 connections, connections per client ------------------------------

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(2500)).await;
    "slow"
}

type H2Sender = hyper::client::conn::http2::SendRequest<http_body_util::Empty<bytes::Bytes>>;

/// An HTTP/2 connection with prior knowledge (h2c) from `stream`: the request sender and the
/// task driving the connection (it ends when the server closes the connection).
async fn h2c(stream: tokio::net::TcpStream) -> (H2Sender, tokio::task::JoinHandle<()>) {
    let (sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(stream),
    )
    .await
    .unwrap();
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    (sender, driver)
}

/// GET `path` on an HTTP/2 connection: the status and body.
async fn h2_get(sender: &mut H2Sender, path: &str) -> (u16, String) {
    sender.ready().await.unwrap();
    let req = http::Request::builder()
        .uri(format!("http://127.0.0.1{path}"))
        .body(http_body_util::Empty::new())
        .unwrap();
    let res = sender.send_request(req).await.unwrap();
    let status = res.status().as_u16();
    let body = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_http2_connection_is_closed() {
    let (addr, app) = serve(|s| s.server_header_timeout = Duration::from_secs(1)).await;
    let (mut sender, mut driver) = h2c(tokio::net::TcpStream::connect(addr).await.unwrap()).await;
    assert_eq!(h2_get(&mut sender, "/api/hello").await, (200, "ok".into()));
    // A request running longer than the idle time is not cut off.
    assert_eq!(h2_get(&mut sender, "/api/slow").await, (200, "slow".into()));
    // Then the connection idles (answering the server's pings) and is closed.
    let closed = tokio::time::timeout(Duration::from_secs(4), &mut driver).await;
    assert!(closed.is_ok(), "the idle HTTP/2 connection is still open");
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_http2_connections_cannot_hold_every_slot() {
    let (addr, app) = serve(|s| {
        s.server_header_timeout = Duration::from_secs(1);
        s.server_max_connections = 8;
        s.server_max_connections_per_ip = 0;
    })
    .await;
    let mut held = Vec::new();
    for _ in 0..8 {
        let (mut sender, driver) = h2c(tokio::net::TcpStream::connect(addr).await.unwrap()).await;
        assert_eq!(h2_get(&mut sender, "/api/hello").await.0, 200);
        held.push((sender, driver));
    }
    let mut ninth = tokio::net::TcpStream::connect(addr).await.unwrap();
    ninth
        .write_all(b"GET /api/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let reply = read_reply(&mut ninth, Duration::from_secs(4))
        .await
        .unwrap_or_default();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply:?}");
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_client_holds_at_most_its_share_of_connections() {
    let (addr, app) = serve(|s| {
        s.server_header_timeout = Duration::from_secs(10);
        s.server_max_connections = 8;
        s.server_max_connections_per_ip = 4;
    })
    .await;
    let mut held = Vec::new();
    for _ in 0..4 {
        let (mut sender, driver) = h2c(connect_from([127, 0, 0, 1], addr).await).await;
        assert_eq!(h2_get(&mut sender, "/api/hello").await.0, 200);
        held.push((sender, driver));
    }
    // A fifth connection from the same client is closed at once.
    let mut fifth = connect_from([127, 0, 0, 1], addr).await;
    assert!(closed_within(&mut fifth, Duration::from_secs(2)).await);
    // Another client is served.
    if !second_loopback() {
        app.shutdown();
        return;
    }
    let mut other = connect_from([127, 0, 0, 2], addr).await;
    other
        .write_all(b"GET /api/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let reply = read_reply(&mut other, Duration::from_secs(3))
        .await
        .unwrap_or_default();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply:?}");
    // When one connection ends, the client may open another.
    let (sender, driver) = held.pop().unwrap();
    drop(sender);
    driver.abort();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (mut sender, _driver) = h2c(connect_from([127, 0, 0, 1], addr).await).await;
    assert_eq!(h2_get(&mut sender, "/api/hello").await.0, 200);
    app.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trusted_proxy_is_not_limited_per_client() {
    let (addr, app) = serve(|s| {
        s.server_header_timeout = Duration::from_secs(10);
        s.server_max_connections_per_ip = 2;
        s.trusted_proxies = "127.0.0.1".into();
    })
    .await;
    let mut held = Vec::new();
    for _ in 0..4 {
        let (mut sender, driver) = h2c(connect_from([127, 0, 0, 1], addr).await).await;
        assert_eq!(h2_get(&mut sender, "/api/hello").await.0, 200);
        held.push((sender, driver));
    }
    app.shutdown();
}

// ---- W1-01: requests per HTTP/2 connection and per client ------------------------------

/// Requests of `/park` running now, and the most that ever ran at once.
static PARKED: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

async fn park() -> &'static str {
    let now = PARKED.fetch_add(1, Ordering::SeqCst) + 1;
    PEAK.fetch_max(now, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    PARKED.fetch_sub(1, Ordering::SeqCst);
    "parked"
}

/// `count` GETs of `/park` at once on one HTTP/2 connection: their statuses (0 for a stream the
/// server reset) and the most that ran at once.
async fn park_many(sender: &H2Sender, count: usize) -> (Vec<u16>, usize) {
    PEAK.store(0, Ordering::SeqCst);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..count {
        let mut sender = sender.clone();
        tasks.spawn(async move {
            sender.ready().await.unwrap();
            let req = http::Request::builder()
                .uri("http://127.0.0.1/api/park")
                .body(http_body_util::Empty::new())
                .unwrap();
            // A stream the server refuses (`REFUSED_STREAM`) counts as 0.
            match sender.send_request(req).await {
                Ok(res) => res.status().as_u16(),
                Err(_) => 0,
            }
        });
    }
    let mut statuses = Vec::new();
    while let Some(status) = tasks.join_next().await {
        statuses.push(status.unwrap());
    }
    statuses.sort_unstable();
    (statuses, PEAK.load(Ordering::SeqCst))
}

/// One HTTP/2 connection must not run hundreds of requests (each may buffer `BODY_LIMIT` of
/// body): with hyper's default of 200 streams one connection held 411 MiB of form bodies. A
/// client runs at most `SERVER_MAX_CONNECTIONS_PER_IP` requests at once across its connections
/// (more get 429), and one connection at most `SERVER_MAX_STREAMS`. One test, because the
/// counters are shared.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_client_runs_at_most_its_share_of_requests_over_http2() {
    // The per-client request cap: 4 run, the other 8 get 429 at once.
    let (addr, app) = serve(|s| {
        s.server_header_timeout = Duration::from_secs(10);
        s.server_max_connections_per_ip = 4;
        s.server_max_streams = 100;
    })
    .await;
    let (sender, _driver) = h2c(connect_from([127, 0, 0, 1], addr).await).await;
    let (statuses, peak) = park_many(&sender, 12).await;
    assert!(peak <= 4, "{peak} requests ran at once");
    assert_eq!(
        statuses.iter().filter(|s| **s == 200).count(),
        4,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == 429).count(),
        8,
        "{statuses:?}"
    );
    // The slots come back: the client is served again.
    let (statuses, _) = park_many(&sender, 2).await;
    assert_eq!(statuses, [200, 200]);
    app.shutdown();

    // The stream cap (no per-client limit): at most 3 at once, the others wait their turn.
    let (addr, app) = serve(|s| {
        s.server_header_timeout = Duration::from_secs(10);
        s.server_max_connections_per_ip = 0;
        s.server_max_streams = 3;
    })
    .await;
    let (mut sender, _driver) = h2c(connect_from([127, 0, 0, 1], addr).await).await;
    // One request first, so the client has read the server's SETTINGS and queues past the limit.
    assert_eq!(h2_get(&mut sender, "/api/hello").await.0, 200);
    let (statuses, peak) = park_many(&sender, 9).await;
    assert!(peak <= 3, "{peak} streams ran at once on one connection");
    assert_eq!(statuses, [200; 9]);
    app.shutdown();
}

// ---- S1-11: no server under APP_ENV=testing -------------------------------------------

#[tokio::test]
async fn the_server_refuses_to_start_under_app_env_testing() {
    let mut settings = Settings::from_env();
    settings.env = "testing".into();
    settings.database_url = String::new();
    let built = AppBuilder::new(settings)
        .routes(|r| {
            r.get("/", ok);
        })
        .build()
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        smeltery_core::serve_on(built.app, built.router, listener),
    )
    .await
    .expect("returns at once");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("APP_ENV is `testing`"), "{err}");
}
