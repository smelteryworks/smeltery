//! Sparks listeners with the real Anvil, across processes: an event sent in one process (a `work`-like app) reaches a
//! page held by another (the web process, a real server on 127.0.0.1) through the PubSub `database` driver, as a
//! signed listen message; the page's `$listen` runs the listener after Anvil's channel callback allowed it again.
#![cfg(feature = "sqlite")]
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use smeltery::anvil::{Anvil, Channel, ChannelCtx};
use smeltery::config::Settings;
use smeltery::prelude::*;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(dir = "tests/app/resources/views", stream)]
pub struct OrderStatus {
    pub order_id: i64,
    pub status: String,
}

#[derive(Deserialize)]
pub struct Shipped {
    pub carrier: String,
}

#[actions]
impl OrderStatus {
    #[on("anvil:private-orders.{order_id}", "OrderShipped")]
    pub async fn shipped(&mut self, event: Shipped) -> Result<()> {
        self.status = format!("shipped by {}", event.carrier);
        Ok(())
    }
}

#[derive(Clone)]
struct User(i64);

impl smeltery::auth::Authenticatable for User {
    fn auth_id(&self) -> i64 {
        self.0
    }
    fn password_hash(&self) -> &str {
        "not a hash"
    }
    fn remember_token(&self) -> Option<&str> {
        None
    }
}

#[derive(Mold)]
#[mold("orders_page", dir = "tests/app/resources/views")]
struct OrdersPage {}

async fn orders() -> OrdersPage {
    OrdersPage {}
}

async fn login(auth: Auth, Path(id): Path<i64>) -> Result<&'static str> {
    auth.login(&User(id), false).await?;
    Ok("in")
}

fn settings(url: &str) -> Settings {
    let mut s = Settings::from_env();
    // `serve` refuses APP_ENV=testing.
    s.env = "local".into();
    s.key = "sparks-listeners-key-0123456789abcdef".into();
    s.url = "http://127.0.0.1".into();
    s.root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
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
        // User 1 owns order 7.
        c.private("orders.{order}", |ctx: ChannelCtx| async move {
            Ok(ctx.user_id() == Some(1) && ctx.param::<i64>("order")? == 7)
        });
    })
    .sparks(|s| {
        s.add::<OrderStatus>();
    })
    .routes(|r| {
        r.get("/orders", orders);
        r.get("/login/{id}", login);
    })
}

/// The `Set-Cookie` pair of the session in a raw head (case kept).
fn set_cookie(raw_head: &str) -> Option<String> {
    raw_head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .map(|l| {
            l["set-cookie:".len()..]
                .trim()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        })
}

async fn get(addr: std::net::SocketAddr, path: &str, cookie: &str) -> (String, String) {
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), socket.read_to_end(&mut raw))
        .await
        .expect("an answer")
        .unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut out = String::new();
        let mut rest = body;
        while let Some((size, after)) = rest.split_once("\r\n") {
            let n = usize::from_str_radix(size.trim(), 16).unwrap();
            if n == 0 {
                break;
            }
            out.push_str(&after[..n]);
            rest = &after[n + 2..];
        }
        out
    } else {
        body.to_owned()
    };
    (head.to_owned(), body)
}

/// The unescaped value of attribute `name` on the first element that has it.
fn attr(html: &str, name: &str) -> String {
    let needle = format!("{name}=\"");
    let from = html.find(&needle).unwrap() + needle.len();
    html[from..from + html[from..].find('"').unwrap()]
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Read the stream until a listen message arrives; `None` after `wait`.
async fn listen_message(
    addr: std::net::SocketAddr,
    token: &str,
    cookie: &str,
    ready: tokio::sync::oneshot::Sender<()>,
    wait: Duration,
) -> Option<Value> {
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /_sparks/stream?t={token} HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: {cookie}\r\nAccept: text/event-stream\r\n\r\n"
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut ready = Some(ready);
    let mut seen = String::new();
    let read = async {
        let mut buf = [0u8; 8192];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                return None;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if seen.contains("\r\n\r\n")
                && let Some(ready) = ready.take()
            {
                assert!(seen.starts_with("HTTP/1.1 200"), "{seen}");
                let _ = ready.send(());
            }
            if let Some(at) = seen.find("data: {") {
                let line = &seen[at + "data: ".len()..];
                if let Some(end) = line.find('\n') {
                    let message: Value = serde_json::from_str(line[..end].trim()).unwrap();
                    if message["kind"] == "listen" {
                        return Some(message);
                    }
                    seen.clear();
                }
            }
        }
    };
    tokio::time::timeout(wait, read).await.ok().flatten()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_event_from_another_process_runs_the_listener_of_an_open_page() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path()
            .join("db.sqlite")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let db = smeltery::db::Db::connect(&url).await.unwrap();
    smeltery::pubsub::migrations::up(&smeltery::db::migration::Schema::new(&db))
        .await
        .unwrap();

    // The web process: the server holding the page's stream.
    // A free port on 127.0.0.1 for `serve`.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let mut web_settings = settings(&url);
    web_settings.host = "127.0.0.1".into();
    web_settings.port = addr.port();
    let web = build(AppBuilder::new(web_settings)).build().await.unwrap();
    let web_app = web.app.clone();
    let server = tokio::spawn(smeltery::serve(web.app, web.router));
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The other process: no server, the shared PubSub driver.
    let work = build(AppBuilder::new(settings(&url)))
        .build()
        .await
        .unwrap()
        .app;
    let _background = work.start_background().await.unwrap();

    // Signed in as user 1, the page renders the listener with its channel authorized.
    let (head, _) = get(addr, "/login/1", "").await;
    let mut cookie = set_cookie(&head).expect("a session cookie");
    let (head, page) = get(addr, "/orders", &cookie).await;
    if let Some(newer) = set_cookie(&head) {
        cookie = newer;
    }
    let token = attr(&page, "wire:stream");
    let snapshot = attr(&page, "wire:snapshot");
    let csrf = attr(&page, "content");

    let (ready, opened) = tokio::sync::oneshot::channel();
    let stream = tokio::spawn({
        let (token, cookie) = (token.clone(), cookie.clone());
        async move { listen_message(addr, &token, &cookie, ready, Duration::from_secs(15)).await }
    });
    opened.await.expect("the stream opened");
    // Sent in the other process, again and again until the page has it (the poller needs a moment).
    let anvil = Anvil::of(&work).unwrap();
    let sender = tokio::spawn(async move {
        for _ in 0..100 {
            anvil
                .to(Channel::private("orders.7"))
                .event("OrderShipped")
                .with(&json!({ "carrier": "DHL" }))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let message = stream
        .await
        .unwrap()
        .expect("the event crossed the processes");
    sender.abort();
    assert_eq!(message["channel"], "private-orders.7");
    assert_eq!(message["data"], r#"{"carrier":"DHL"}"#);

    // The page's `$listen`: the server checks the message and asks Anvil's callback again, then runs the listener.
    let body = json!({
        "v": smeltery::sparks::PROTOCOL_VERSION,
        "components": [{
            "snapshot": snapshot,
            "calls": [{ "method": "$listen", "params": [
                message["channel"], message["event"], message["data"], message["exp"], message["seq"], message["sig"]
            ] }],
        }],
    })
    .to_string();
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST /_sparks/update HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: {cookie}\r\nX-CSRF-TOKEN: {csrf}\r\n\
         Content-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    socket.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8(raw).unwrap();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains("shipped by DHL"), "{text}");

    work.shutdown();
    web_app.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .expect("serve stops")
        .unwrap()
        .unwrap();
}
