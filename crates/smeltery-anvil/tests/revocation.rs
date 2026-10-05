//! Revocation on real sockets (127.0.0.1): core's auth events close the sockets whose private subscriptions an
//! ended credential authorized, with 4200, in every serving process; `except` keeps the caller's credential; a
//! browser's logout closes the sockets its session authorized.
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
use smeltery_anvil::{Anvil, AnvilExt as _, ChannelCtx, Channels};
use smeltery_core::auth::{
    Auth, AuthEvent, Authenticatable, Credential, CredentialKind, Guard, Principal, publish_event,
};
use smeltery_core::config::Settings;
use smeltery_core::http::request::Parts;
use smeltery_core::session::Session;
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

/// A user for `Auth::login` (no user model registered: the session needs none).
#[derive(Clone)]
struct DemoUser(i64);

impl Authenticatable for DemoUser {
    fn auth_id(&self) -> i64 {
        self.0
    }
    fn password_hash(&self) -> &str {
        "x"
    }
    fn remember_token(&self) -> Option<&str> {
        None
    }
}

fn channels(c: &mut Channels) {
    // In this test, user n owns order n.
    c.private("orders.{order}", |ctx: ChannelCtx| async move {
        Ok(ctx.user_id() == Some(ctx.param::<i64>("order")?))
    });
}

fn build(b: AppBuilder) -> AppBuilder {
    b.guard(FakeTokens).anvil(channels).routes(|r| {
        r.get(
            "/login/{id}",
            |auth: Auth, session: Session, axum::extract::Path(id): axum::extract::Path<i64>| async move {
                auth.login(&DemoUser(id), false).await.unwrap();
                session.csrf_token().unwrap()
            },
        );
        // The key core's auth events name this browser's session by.
        r.get("/key", |session: Session| async move {
            format!("web:session:{}", session.binding())
        });
        r.post("/logout", |auth: Auth| async move {
            auth.logout().await.unwrap();
            "bye"
        });
    })
}

fn settings(database: &str, driver: &str) -> Settings {
    let mut s = Settings::from_env();
    s.env = "production".into();
    s.key = "anvil-revocation-key-0123456789abcdef".into();
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

/// A plain HTTP/1.1 request; the status, the `Set-Cookie` pairs and the body.
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, Vec<String>, String) {
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
    let cookies = head
        .lines()
        .filter_map(|l| {
            l.strip_prefix("set-cookie: ")
                .or_else(|| l.strip_prefix("Set-Cookie: "))
        })
        .map(|c| c.split(';').next().unwrap().to_owned())
        .collect();
    (status, cookies, body.to_owned())
}

/// The body of a chunked or plain answer: the JSON object in it.
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
    close_frame(ws).await.map(|(code, _)| code)
}

/// The close code and reason the socket gets.
async fn close_frame(ws: &mut Client) -> Option<(u16, String)> {
    let wait = async {
        while let Some(message) = ws.next().await {
            match message {
                Ok(Message::Close(frame)) => {
                    return frame.map(|f| (u16::from(f.code), f.reason.as_str().to_owned()));
                }
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

/// A connected socket and its id.
async fn connect(addr: SocketAddr, key: &str) -> (Client, String) {
    let request = format!("ws://{addr}/app/{key}?protocol=7")
        .into_client_request()
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let first = next_json(&mut ws).await;
    let data: Value = serde_json::from_str(first["data"].as_str().unwrap()).unwrap();
    (ws, data["socket_id"].as_str().unwrap().to_owned())
}

async fn subscribe(ws: &mut Client, channel: &str, auth: &str) -> Value {
    let frame =
        json!({ "event": "pusher:subscribe", "data": { "channel": channel, "auth": auth } });
    ws.send(Message::text(frame.to_string())).await.unwrap();
    next_json(ws).await
}

/// A socket subscribed to `private-orders.<user>` with a grant from the token endpoint.
async fn token_socket(addr: SocketAddr, key: &str, user: i64, token: i64) -> Client {
    let (mut ws, socket_id) = connect(addr, key).await;
    let (status, _, body) = http(
        addr,
        "POST",
        "/api/broadcasting/auth",
        &[
            ("X-Test-Token", format!("{user}:{token}")),
            ("Content-Type", "application/x-www-form-urlencoded".into()),
        ],
        &format!("socket_id={socket_id}&channel_name=private-orders.{user}"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let auth = json_in(&body)["auth"].as_str().unwrap().to_owned();
    let answer = subscribe(&mut ws, &format!("private-orders.{user}"), &auth).await;
    assert_eq!(answer["event"], "pusher_internal:subscription_succeeded");
    ws
}

/// Whether the socket still answers a ping.
async fn alive(ws: &mut Client) -> bool {
    if ws
        .send(Message::text(r#"{"event":"pusher:ping","data":{}}"#))
        .await
        .is_err()
    {
        return false;
    }
    matches!(
        tokio::time::timeout(Duration::from_secs(5), next_json(ws)).await,
        Ok(frame) if frame["event"] == "pusher:pong"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_token_closes_its_sockets_in_another_process() {
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
    let web = build(AppBuilder::new(settings(&url, "database")))
        .build()
        .await
        .unwrap();
    let web_app = web.app.clone();
    let key = Anvil::of(&web_app).unwrap().app_key().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(smeltery_core::serve_on(web.app, web.router, listener));

    let mut revoked = token_socket(addr, &key, 7, 5).await;
    let mut kept = token_socket(addr, &key, 7, 6).await;
    let mut other_user = token_socket(addr, &key, 8, 9).await;

    // Another process ends every token of user 7 but the one making the request.
    let work = build(AppBuilder::new(settings(&url, "database")))
        .build()
        .await
        .unwrap()
        .app;
    let _background = work.start_background().await.unwrap();
    assert!(
        !work.serves_http(),
        "the publishing process holds no sockets (like `work`)"
    );
    publish_event(
        &work,
        &AuthEvent::RevokedAll {
            user_id: 7,
            kind: CredentialKind::Tokens,
            except: Some("fake:token:6".into()),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        close_frame(&mut revoked).await,
        Some((4200, "authorization revoked".to_owned())),
        "the reason names the revocation"
    );
    assert!(
        alive(&mut kept).await,
        "the credential named in `except` stays"
    );
    assert!(alive(&mut other_user).await, "another user's socket stays");

    work.shutdown();
    web_app.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .expect("serve stops")
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logout_closes_the_sockets_its_session_authorized() {
    let built = build(AppBuilder::new(settings("", "local")))
        .build()
        .await
        .unwrap();
    let app = built.app.clone();
    let key = Anvil::of(&app).unwrap().app_key().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));

    // Sign in as user 7 in a browser.
    let (status, cookies, csrf) = http(addr, "GET", "/login/7", &[], "").await;
    assert_eq!(status, 200);
    let csrf = csrf
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.trim().chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap()
        .trim()
        .to_owned();
    let cookie = cookies.join("; ");

    let (mut ws, socket_id) = connect(addr, &key).await;
    let (status, _, body) = http(
        addr,
        "POST",
        "/broadcasting/auth",
        &[
            ("Cookie", cookie.clone()),
            ("X-CSRF-TOKEN", csrf.clone()),
            ("Content-Type", "application/x-www-form-urlencoded".into()),
        ],
        &format!("socket_id={socket_id}&channel_name=private-orders.7"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let auth = json_in(&body)["auth"].as_str().unwrap().to_owned();
    assert_eq!(
        subscribe(&mut ws, "private-orders.7", &auth).await["event"],
        "pusher_internal:subscription_succeeded"
    );
    // A socket of another browser of the same user stays.
    let mut stranger = token_socket(addr, &key, 7, 1).await;
    // A grant made before the logout, used after it: refused.
    let (mut late, late_id) = connect(addr, &key).await;
    let (_, _, body) = http(
        addr,
        "POST",
        "/broadcasting/auth",
        &[
            ("Cookie", cookie.clone()),
            ("X-CSRF-TOKEN", csrf.clone()),
            ("Content-Type", "application/x-www-form-urlencoded".into()),
        ],
        &format!("socket_id={late_id}&channel_name=private-orders.7"),
    )
    .await;
    let late_auth = json_in(&body)["auth"].as_str().unwrap().to_owned();

    let (status, _, _) = http(
        addr,
        "POST",
        "/logout",
        &[("Cookie", cookie), ("X-CSRF-TOKEN", csrf)],
        "",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(close_code(&mut ws).await, Some(4200));
    assert!(alive(&mut stranger).await);

    let answer = subscribe(&mut late, "private-orders.7", &late_auth).await;
    assert_eq!(answer["event"], "pusher:subscription_error", "{answer}");
    assert!(
        answer["data"].as_str().unwrap().contains("revoked"),
        "{answer}"
    );

    app.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .expect("serve stops")
        .unwrap()
        .unwrap();
}

/// Sign in as `user` in a new browser; its cookies, CSRF token and session key.
async fn browser(addr: SocketAddr, user: i64) -> (String, String, String) {
    let (status, cookies, csrf) = http(addr, "GET", &format!("/login/{user}"), &[], "").await;
    assert_eq!(status, 200);
    let csrf = csrf
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.trim().chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap()
        .trim()
        .to_owned();
    let cookie = cookies.join("; ");
    let (status, _, body) = http(addr, "GET", "/key", &[("Cookie", cookie.clone())], "").await;
    assert_eq!(status, 200);
    let key = body
        .lines()
        .find(|l| l.starts_with("web:session:"))
        .unwrap()
        .trim()
        .to_owned();
    (cookie, csrf, key)
}

/// A socket subscribed to `private-orders.<user>` with a grant from the cookie endpoint.
async fn browser_socket(
    addr: SocketAddr,
    key: &str,
    user: i64,
    cookie: &str,
    csrf: &str,
) -> Client {
    let (mut ws, socket_id) = connect(addr, key).await;
    let (status, _, body) = http(
        addr,
        "POST",
        "/broadcasting/auth",
        &[
            ("Cookie", cookie.to_owned()),
            ("X-CSRF-TOKEN", csrf.to_owned()),
            ("Content-Type", "application/x-www-form-urlencoded".into()),
        ],
        &format!("socket_id={socket_id}&channel_name=private-orders.{user}"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let auth = json_in(&body)["auth"].as_str().unwrap().to_owned();
    let answer = subscribe(&mut ws, &format!("private-orders.{user}"), &auth).await;
    assert_eq!(answer["event"], "pusher_internal:subscription_succeeded");
    ws
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ending_the_other_sessions_keeps_the_callers_browser_and_tokens() {
    let built = build(AppBuilder::new(settings("", "local")))
        .build()
        .await
        .unwrap();
    let app = built.app.clone();
    let key = Anvil::of(&app).unwrap().app_key().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));

    let (cookie_a, csrf_a, key_a) = browser(addr, 7).await;
    let (cookie_b, csrf_b, key_b) = browser(addr, 7).await;
    assert_ne!(key_a, key_b, "two browsers, two sessions");
    let mut caller = browser_socket(addr, &key, 7, &cookie_a, &csrf_a).await;
    let mut other_browser = browser_socket(addr, &key, 7, &cookie_b, &csrf_b).await;
    let mut token = token_socket(addr, &key, 7, 3).await;
    let (cookie_c, csrf_c, _) = browser(addr, 8).await;
    let mut other_user = browser_socket(addr, &key, 8, &cookie_c, &csrf_c).await;

    publish_event(
        &app,
        &AuthEvent::RevokedAll {
            user_id: 7,
            kind: CredentialKind::Sessions,
            except: Some(key_a),
        },
    )
    .await
    .unwrap();

    assert_eq!(close_code(&mut other_browser).await, Some(4200));
    assert!(alive(&mut caller).await, "the caller's session stays");
    assert!(alive(&mut token).await, "tokens are another kind");
    assert!(alive(&mut other_user).await, "another user's session stays");

    app.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .expect("serve stops")
        .unwrap()
        .unwrap();
}
