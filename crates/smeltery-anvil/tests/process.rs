//! The `anvil` process (D-416): the app binary's `anvil` command serves only the socket endpoint on its own
//! listener (127.0.0.1 here), while a separate web app with `ANVIL_IN_SERVE=false` answers the auth endpoints and
//! sends the events. Both run on one SQLite file with `PUBSUB_DRIVER=auto`: their process roles pick the shared
//! `database` driver. Grants cross (stateless), events and auth events cross (PubSub), shutdown closes with 1001
//! within the budget, and core's per-client connection cap holds on the `anvil` listener.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use smeltery_anvil::{Anvil, AnvilExt as _, Channel, ChannelCtx, Channels};
use smeltery_core::auth::{AuthEvent, Credential, CredentialKind, Guard, Principal, publish_event};
use smeltery_core::config::Settings;
use smeltery_core::http::request::Parts;
use smeltery_core::pubsub::{Driver, PubSub};
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
    c.public("news");
    // User n owns order n.
    c.private("orders.{order}", |ctx: ChannelCtx| async move {
        Ok(ctx.user_id() == Some(ctx.param::<i64>("order")?))
    });
}

fn core(database: &str) -> Settings {
    let mut s = Settings::from_env();
    s.env = "production".into();
    s.key = "anvil-process-key-0123456789abcdef0123".into();
    s.url = "http://127.0.0.1".into();
    s.database_url = database.into();
    s.cache_store = "array".into();
    s.session_driver = "cookie".into();
    // Under `auto` the process roles decide: both processes below share.
    s.pubsub_driver = "auto".into();
    s.pubsub_poll_interval = Duration::from_millis(20);
    s.shutdown_timeout = Duration::from_secs(3);
    s.log_level = "warn".into();
    s
}

/// An app with Anvil; `port` is the `anvil` process's (`ANVIL_SERVER_PORT`), `ANVIL_IN_SERVE=false`.
fn app(core: Settings, port: u16) -> AppBuilder {
    let mut settings = smeltery_anvil::Settings::from_env(&core);
    settings.in_serve = false;
    settings.server_host = "127.0.0.1".into();
    settings.server_port = port;
    AppBuilder::new(core)
        .guard(FakeTokens)
        .anvil_with(settings, channels)
}

async fn sqlite() -> (tempfile::TempDir, String) {
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
    (dir, url)
}

/// A port nobody listens on (bound and released).
async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

/// The `anvil` command, as `smeltery anvil` runs it, on its own task; its app (for the shutdown) and the command's
/// result.
async fn start_anvil_process(
    core: Settings,
    port: u16,
) -> (App, tokio::sync::oneshot::Receiver<Result<ExitCode>>) {
    let booted: Arc<Mutex<Option<App>>> = Arc::default();
    let keep = Arc::clone(&booted);
    let builder = app(core, port).on_boot(move |app| async move {
        *keep.lock().unwrap() = Some(app);
        Ok(())
    });
    let task = run_command(builder);
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let app = booted.lock().unwrap().clone().expect("the app booted");
    (app, task)
}

/// `dispatch(builder, ["anvil"])` on a thread of its own with its own runtime (as `smeltery anvil` runs it; the
/// console's future is not `Send`).
fn run_command(builder: AppBuilder) -> tokio::sync::oneshot::Receiver<Result<ExitCode>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mut out = Vec::new();
        let result = runtime.block_on(smeltery_core::console::dispatch(
            builder,
            &["anvil".into()],
            &mut out,
        ));
        let _ = tx.send(result);
    });
    rx
}

/// A plain HTTP/1.1 request; the status and the body.
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
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
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .expect("closed within 10 s")
}

async fn connect(addr: SocketAddr, key: &str) -> (Client, String) {
    let request = format!("ws://{addr}/app/{key}?protocol=7")
        .into_client_request()
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let first = next_json(&mut ws).await;
    assert_eq!(first["event"], "pusher:connection_established");
    let data: Value = serde_json::from_str(first["data"].as_str().unwrap()).unwrap();
    (ws, data["socket_id"].as_str().unwrap().to_owned())
}

async fn subscribe(ws: &mut Client, channel: &str, auth: Option<&str>) -> Value {
    let mut data = json!({ "channel": channel });
    if let Some(auth) = auth {
        data["auth"] = json!(auth);
    }
    let frame = json!({ "event": "pusher:subscribe", "data": data });
    ws.send(Message::text(frame.to_string())).await.unwrap();
    next_json(ws).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_anvil_process_serves_sockets_for_a_separate_web_app() {
    let (_dir, url) = sqlite().await;
    let port = free_port().await;

    // The web process: pages and the auth endpoints, no sockets (ANVIL_IN_SERVE=false).
    let web = app(core(&url), port).build().await.unwrap();
    let web_app = web.app.clone();
    let key = Anvil::of(&web_app).unwrap().app_key().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web_addr = listener.local_addr().unwrap();
    let web_server = tokio::spawn(smeltery_core::serve_on(web.app, web.router, listener));

    // The `anvil` process.
    let (anvil_app, anvil_task) = start_anvil_process(core(&url), port).await;
    let anvil_addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    assert_eq!(
        Anvil::of(&anvil_app).unwrap().app_key(),
        key,
        "both processes derive the same key"
    );

    // Each process serves its part only.
    let socket_path = format!("/app/{key}");
    assert_eq!(http(web_addr, "GET", &socket_path, &[], "").await.0, 404);
    assert_eq!(
        http(anvil_addr, "GET", &socket_path, &[], "").await.0,
        426,
        "the endpoint is here (it wants an upgrade)"
    );
    for (method, path) in [
        ("GET", "/"),
        ("POST", "/broadcasting/auth"),
        ("POST", "/api/broadcasting/auth"),
    ] {
        assert_eq!(
            http(anvil_addr, method, path, &[], "").await.0,
            404,
            "{method} {path}"
        );
    }
    assert_eq!(http(anvil_addr, "GET", "/up", &[], "").await.0, 200);
    // A3: by role, both share under `auto`.
    assert_eq!(
        PubSub::of(&web_app).unwrap().driver(),
        Some(Driver::Database)
    );
    assert_eq!(
        PubSub::of(&anvil_app).unwrap().driver(),
        Some(Driver::Database)
    );

    // A socket on the `anvil` process, authorized by a grant from the web process.
    let (mut private, socket_id) = connect(anvil_addr, &key).await;
    let (status, body) = http(
        web_addr,
        "POST",
        "/api/broadcasting/auth",
        &[
            ("X-Test-Token", "7:5".into()),
            ("Content-Type", "application/x-www-form-urlencoded".into()),
        ],
        &format!("socket_id={socket_id}&channel_name=private-orders.7"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let auth = json_in(&body)["auth"].as_str().unwrap().to_owned();
    let answer = subscribe(&mut private, "private-orders.7", Some(&auth)).await;
    assert_eq!(answer["event"], "pusher_internal:subscription_succeeded");
    let (mut public, _) = connect(anvil_addr, &key).await;
    let answer = subscribe(&mut public, "news", None).await;
    assert_eq!(answer["event"], "pusher_internal:subscription_succeeded");

    // An event the web process sends reaches the socket in the `anvil` process.
    let delivered = Anvil::of(&web_app)
        .unwrap()
        .to(Channel::private("orders.7"))
        .event("shipped")
        .with(&json!({ "id": 7 }))
        .await
        .unwrap();
    assert_eq!(delivered.local, 0, "no socket in the web process");
    let event = next_json(&mut private).await;
    assert_eq!(event["event"], "shipped");
    assert_eq!(event["data"], r#"{"id":7}"#);

    // Revocation in the web process closes the socket in the `anvil` process.
    publish_event(
        &web_app,
        &AuthEvent::RevokedAll {
            user_id: 7,
            kind: CredentialKind::Tokens,
            except: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(close_code(&mut private).await, Some(4200));

    // Shutdown: the remaining socket gets 1001 and the command returns within the budget.
    anvil_app.shutdown();
    assert_eq!(close_code(&mut public).await, Some(1001));
    let code = tokio::time::timeout(Duration::from_secs(10), anvil_task)
        .await
        .expect("the anvil process stops within its budget")
        .unwrap()
        .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(Anvil::of(&anvil_app).unwrap().connections(), 0);

    web_app.shutdown();
    tokio::time::timeout(Duration::from_secs(15), web_server)
        .await
        .expect("serve stops")
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_servers_per_client_cap_holds_on_the_anvil_listener() {
    let (_dir, url) = sqlite().await;
    let port = free_port().await;
    let mut settings = core(&url);
    settings.server_max_connections_per_ip = 2;
    let mut anvil_settings = smeltery_anvil::Settings::from_env(&settings);
    // Anvil's own per-client cap off: the server's is the one under test.
    anvil_settings.max_connections_per_ip = 0;
    anvil_settings.in_serve = false;
    anvil_settings.server_port = port;
    let booted: Arc<Mutex<Option<App>>> = Arc::default();
    let keep = Arc::clone(&booted);
    let builder = AppBuilder::new(settings)
        .anvil_with(anvil_settings, channels)
        .on_boot(move |app| async move {
            *keep.lock().unwrap() = Some(app);
            Ok(())
        });
    let task = run_command(builder);
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut key = None;
    for _ in 0..200 {
        if let Some(app) = booted.lock().unwrap().clone() {
            key = Some(Anvil::of(&app).unwrap().app_key().to_owned());
        }
        if key.is_some() && tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let key = key.unwrap();
    // The probe connection above is closed; give the server a moment to count it out.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_one, _) = connect(addr, &key).await;
    let (_two, _) = connect(addr, &key).await;
    let request = format!("ws://{addr}/app/{key}?protocol=7")
        .into_client_request()
        .unwrap();
    let third = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .expect("answered at once");
    assert!(
        third.is_err(),
        "a third connection from 127.0.0.1 is closed by the server's per-client cap"
    );

    let app = booted.lock().unwrap().clone().unwrap();
    app.shutdown();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn the_anvil_process_refuses_to_run_without_a_shared_driver() {
    let mut settings = core("");
    settings.pubsub_driver = "local".into();
    let port = free_port().await;
    let mut out = Vec::new();
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        smeltery_core::console::dispatch(app(settings, port), &["anvil".into()], &mut out),
    )
    .await
    .expect("refused at start, not served")
    .unwrap_err();
    assert!(err.to_string().contains("PUBSUB_DRIVER"), "{err}");
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "nothing listens"
    );
}
