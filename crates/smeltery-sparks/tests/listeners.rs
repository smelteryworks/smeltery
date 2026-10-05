//! Listeners (`#[on("anvil:…", "…")]`): channels authorized at render and again at the stream and at `$listen`,
//! signed listen messages that run once, stream tokens bound to the session, streams that end at logout and at
//! revocation, and the boot checks. A fake channel authorizer stands in for Anvil (the facade's
//! `tests/sparks_listeners.rs` runs the real one across processes).
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use http_body_util::BodyExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use smeltery_core::auth::{Auth, AuthEvent, Authenticatable, CredentialKind};
use smeltery_core::channels::{ChannelAuthorizer, ChannelEvent, ChannelEventSender, ChannelEvents};
use smeltery_core::config::Settings;
use smeltery_core::view::view;
use smeltery_core::{App, AppBuilder, BoxFuture, Response, Result};
use smeltery_macros::{Spark, actions};
use smeltery_mold_macros::Mold;
use smeltery_sparks::{Broadcast, SparkCtx, Sparks, SparksExt};
use tower::ServiceExt as _;

// ---------------------------------------------------------------- the component

#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(
    name = "order-status",
    crate = "smeltery_sparks",
    dir = "tests/app/resources/views",
    stream
)]
pub struct OrderStatus {
    #[spark(model)]
    pub order_id: i64,
    pub status: String,
    #[spark(model)]
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct Shipped {
    pub carrier: String,
}

#[actions(crate = "smeltery_sparks")]
impl OrderStatus {
    #[on("anvil:private-orders.{order_id}", "OrderShipped")]
    pub async fn shipped(&mut self, ctx: &mut SparkCtx, event: Shipped) -> Result<()> {
        self.status = format!("shipped by {} for {:?}", event.carrier, ctx.user_id());
        Ok(())
    }

    #[on("anvil:news", "Posted")]
    async fn posted(&mut self) -> Result<()> {
        self.status = "news".into();
        Ok(())
    }

    pub async fn switch(&mut self, order: i64) -> Result<()> {
        self.order_id = order;
        Ok(())
    }

    /// Fails validation: the calls after it in the request do not run.
    pub async fn fail(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        Err(ctx.error("note", "The note is invalid."))
    }
}

/// A plain streamed component without listeners.
#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(crate = "smeltery_sparks", dir = "tests/app/resources/views", stream)]
pub struct Live {
    pub n: i64,
}

#[actions(crate = "smeltery_sparks")]
impl Live {}

// ---------------------------------------------------------------- the fake authorizer and the app

/// User 1 owns order 7; `news` is public; nothing else exists. Counts its decisions.
#[derive(Clone, Default)]
struct Fake {
    sender: ChannelEventSender,
    decisions: Arc<AtomicUsize>,
    deny: Arc<AtomicBool>,
}

impl ChannelAuthorizer for Fake {
    fn authorize<'a>(
        &'a self,
        _app: &'a App,
        channel: &'a str,
        auth: Option<&'a Auth>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            // A decision that waits (like a database query): renders drive it on their blocking thread.
            tokio::task::yield_now().await;
            self.decisions.fetch_add(1, Ordering::SeqCst);
            Ok(match channel {
                "news" => true,
                "private-orders.7" => {
                    !self.deny.load(Ordering::SeqCst) && auth.and_then(Auth::id) == Some(1)
                }
                _ => false,
            })
        })
    }

    fn events(&self) -> ChannelEvents {
        self.sender.subscribe()
    }
}

#[derive(Clone)]
struct User(i64);

impl Authenticatable for User {
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
#[mold(
    "pages/live",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
struct LivePage {}

async fn live() -> Response {
    view(LivePage {})
}

#[derive(Mold)]
#[mold(
    "pages/orders",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
struct OrdersPage {
    order: i64,
}

async fn orders(axum::extract::Path(order): axum::extract::Path<i64>) -> Response {
    view(OrdersPage { order })
}

async fn login(
    auth: Auth,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<&'static str> {
    auth.login(&User(id), false).await?;
    Ok("in")
}

async fn logout(auth: Auth) -> Result<&'static str> {
    auth.logout().await?;
    Ok("out")
}

fn settings() -> Settings {
    let mut settings = Settings::from_env();
    settings.env = "testing".into();
    settings.root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/app");
    settings.debug = true;
    // The listen seen-set lives in the cache.
    settings.cache_store = "array".into();
    settings
}

fn register(s: &mut Sparks) {
    s.add::<OrderStatus>().add::<Live>();
}

async fn app_with(ttl: Option<Duration>) -> (Client, Fake) {
    let fake = Fake::default();
    let built = AppBuilder::new(settings())
        .channel_authorizer(fake.clone())
        .sparks(move |s| {
            register(s);
            if let Some(ttl) = ttl {
                s.snapshot_ttl(ttl);
            }
        })
        .routes(|r| {
            r.get("/orders/{order}", orders);
            r.get("/live", live);
            r.get("/login/{id}", login);
            r.get("/logout", logout);
        })
        .build()
        .await
        .unwrap();
    (
        Client {
            app: built.app,
            router: built.router,
            cookies: std::sync::Mutex::new(BTreeMap::new()),
        },
        fake,
    )
}

async fn app() -> (Client, Fake) {
    app_with(None).await
}

// ---------------------------------------------------------------- a small browser

/// Requests through the router with a cookie jar (the stream is a long response: `TestApp` would wait for its end).
struct Client {
    app: App,
    router: axum::Router,
    cookies: std::sync::Mutex<BTreeMap<String, String>>,
}

impl Client {
    fn cookie_header(&self) -> String {
        self.cookies
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    async fn send(&self, req: http::Request<axum::body::Body>) -> http::Response<axum::body::Body> {
        let res = self.router.clone().oneshot(req).await.unwrap();
        let mut jar = self.cookies.lock().unwrap();
        for value in res.headers().get_all(http::header::SET_COOKIE) {
            let text = value.to_str().unwrap();
            let pair = text.split(';').next().unwrap();
            let (name, value) = pair.split_once('=').unwrap();
            if value.is_empty() || text.contains("Max-Age=0") {
                jar.remove(name);
            } else {
                jar.insert(name.to_owned(), value.to_owned());
            }
        }
        res
    }

    async fn get(&self, path: &str) -> String {
        let req = http::Request::get(path)
            .header(http::header::COOKIE, self.cookie_header())
            .body(axum::body::Body::empty())
            .unwrap();
        let res = self.send(req).await;
        assert_eq!(res.status(), 200, "{path}");
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
    }

    /// `POST /_sparks/update` for one component; the status and the JSON answer.
    async fn update(&self, snapshot: &str, calls: Value) -> (u16, Value) {
        self.update_with(snapshot, json!({}), calls).await
    }

    /// `POST /_sparks/update` with model `updates` and `calls`, as `sparks.js` batches them.
    async fn update_with(&self, snapshot: &str, updates: Value, calls: Value) -> (u16, Value) {
        let body = json!({
            "v": smeltery_sparks::PROTOCOL_VERSION,
            "components": [{ "snapshot": snapshot, "updates": updates, "calls": calls }],
        });
        let req = http::Request::post("/_sparks/update")
            .header(http::header::COOKIE, self.cookie_header())
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::ACCEPT, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let res = self.send(req).await;
        let status = res.status().as_u16();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// `GET /_sparks/stream?t=<tokens>` with `cookie` (the jar's when `None`).
    async fn stream(&self, tokens: &[&str], cookie: Option<&str>) -> (u16, axum::body::Body) {
        let cookie = cookie.map_or_else(|| self.cookie_header(), str::to_owned);
        let req = http::Request::get(format!("/_sparks/stream?t={}", tokens.join(",")))
            .header(http::header::COOKIE, cookie)
            .body(axum::body::Body::empty())
            .unwrap();
        let res = self.router.clone().oneshot(req).await.unwrap();
        (res.status().as_u16(), res.into_body())
    }
}

/// The next data frame as JSON (skipping keep-alive comments), or `None` when the stream ended or `wait` passed.
async fn next_data(body: &mut axum::body::Body, wait: Duration) -> Option<Value> {
    let read = async {
        loop {
            let frame = body.frame().await?.ok()?;
            if let Ok(data) = frame.into_data() {
                let text = String::from_utf8(data.to_vec()).unwrap();
                if let Some(json) = text.strip_prefix("data: ") {
                    return Some(serde_json::from_str(json.trim()).unwrap());
                }
            }
        }
    };
    tokio::time::timeout(wait, read).await.ok().flatten()
}

/// Whether the stream ended within `wait` (no more frames).
async fn ended(body: &mut axum::body::Body, wait: Duration) -> bool {
    let read = async {
        loop {
            match body.frame().await {
                None | Some(Err(_)) => return true,
                Some(Ok(_)) => {}
            }
        }
    };
    tokio::time::timeout(wait, read).await.unwrap_or(false)
}

/// The value of attribute `name` on the `wire:name="order-status"` root in `html`, unescaped.
fn attr(html: &str, name: &str) -> String {
    let at = html.find("wire:name=\"order-status\"").unwrap();
    let start = html[..at].rfind("<div ").unwrap();
    let tag = &html[start..start + html[start..].find('>').unwrap()];
    let needle = format!(" {name}=\"");
    let from = tag.find(&needle).unwrap() + needle.len();
    let value = &tag[from..from + tag[from..].find('"').unwrap()];
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The claims inside a stream token.
fn claims(token: &str) -> Value {
    use base64::Engine as _;
    let body = token.split('.').next().unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body)
            .unwrap(),
    )
    .unwrap()
}

/// The `$listen` call of a listen message, as `sparks.js` sends it back.
fn listen_call(m: &Value) -> Value {
    json!([{ "method": "$listen", "params": [m["channel"], m["event"], m["data"], m["exp"], m["seq"], m["sig"]] }])
}

/// A listen message signed as the protocol describes (docs/SPARKS-PROTOCOL.md §9): what only the server can make.
fn signed(
    app: &App,
    id: &str,
    channel: &str,
    event: &str,
    data: &str,
    exp: u64,
    seq: u64,
) -> Value {
    let text = json!({
        "c": channel,
        "d": smeltery_core::crypto::sha256_hex(data),
        "e": event,
        "i": id,
        "n": seq,
        "x": exp,
    })
    .to_string();
    let sig = app.sign("sparks.listen", text.as_bytes()).unwrap();
    json!({ "target": id, "kind": "listen", "channel": channel, "event": event, "data": data, "exp": exp, "seq": seq, "sig": sig })
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(300);

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn a_broadcast_reaches_the_listener_once_as_a_signed_message() {
    let (c, fake) = app().await;
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let token = attr(&page, "wire:stream");
    let snapshot = attr(&page, "wire:snapshot");
    let id = attr(&page, "wire:id");
    assert_eq!(
        claims(&token)["l"],
        json!([["private-orders.7", "OrderShipped"], ["news", "Posted"]]),
        "both channels were authorized at render"
    );
    let (status, mut stream) = c.stream(&[&token], None).await;
    assert_eq!(status, 200);

    // Only the declared pairs of the token's channels reach the page.
    fake.sender.send(ChannelEvent::new(
        "private-orders.8",
        "OrderShipped",
        r#"{"carrier":"UPS"}"#,
    ));
    fake.sender.send(ChannelEvent::new(
        "private-orders.7",
        "OrderCancelled",
        "{}",
    ));
    fake.sender.send(ChannelEvent::new(
        "private-orders.7",
        "OrderShipped",
        r#"{"carrier":"DHL"}"#,
    ));
    let m = next_data(&mut stream, WAIT).await.unwrap();
    assert_eq!(m["kind"], "listen");
    assert_eq!(m["target"], id.as_str());
    assert_eq!(m["channel"], "private-orders.7");
    assert_eq!(m["event"], "OrderShipped");
    assert_eq!(m["data"], r#"{"carrier":"DHL"}"#);
    assert!(m["sig"].is_string() && m["seq"].is_u64() && m["exp"].as_u64().unwrap() > now());

    // An edited payload no longer matches the signature (the review's B-1: the browser relays the data, so it must
    // not be able to choose it).
    let mut edited = m.clone();
    edited["data"] = json!(r#"{"carrier":"refunded"}"#);
    let (status, _) = c.update(&snapshot, listen_call(&edited)).await;
    assert_eq!(status, 403);
    let mut other = m.clone();
    other["event"] = json!("OrderShipped ");
    let (status, _) = c.update(&snapshot, listen_call(&other)).await;
    assert_eq!(status, 403, "an event the message was not signed for");

    // The page sends it back unchanged: the listener runs with the event's data.
    let (status, res) = c.update(&snapshot, listen_call(&m)).await;
    assert_eq!(status, 200, "{res}");
    let after = res["components"][0]["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    let data: Value = serde_json::from_str(&after).unwrap();
    assert_eq!(data["data"]["status"], "shipped by DHL for Some(1)");

    // The same message again, with the new snapshot or with the snapshot from before it ran: it already ran.
    let (status, _) = c.update(&after, listen_call(&m)).await;
    assert_eq!(status, 403);
    let (status, _) = c.update(&snapshot, listen_call(&m)).await;
    assert_eq!(
        status, 403,
        "an older snapshot does not run it again (review M-2)"
    );
    // The listener is not an action.
    let (status, _) = c
        .update(
            &after,
            json!([{ "method": "shipped", "params": [{ "carrier": "x" }] }]),
        )
        .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn listen_messages_are_bound_to_their_instance_and_expire() {
    let (c, _) = app().await;
    c.get("/login/1").await;
    let first = c.get("/orders/7").await;
    let second = c.get("/orders/7").await;
    let (one, two) = (attr(&first, "wire:id"), attr(&second, "wire:id"));
    assert_ne!(one, two);
    let data = r#"{"carrier":"DHL"}"#;
    let later = now() + 30;
    // Signed for the first instance, sent with the second's snapshot.
    let m = signed(
        &c.app,
        &one,
        "private-orders.7",
        "OrderShipped",
        data,
        later,
        5,
    );
    let (status, _) = c
        .update(&attr(&second, "wire:snapshot"), listen_call(&m))
        .await;
    assert_eq!(status, 403);
    // Expired.
    let m = signed(
        &c.app,
        &two,
        "private-orders.7",
        "OrderShipped",
        data,
        now() - 1,
        6,
    );
    let (status, _) = c
        .update(&attr(&second, "wire:snapshot"), listen_call(&m))
        .await;
    assert_eq!(status, 403);
    // Well formed and valid.
    let m = signed(
        &c.app,
        &two,
        "private-orders.7",
        "OrderShipped",
        data,
        later,
        7,
    );
    let (status, res) = c
        .update(&attr(&second, "wire:snapshot"), listen_call(&m))
        .await;
    assert_eq!(status, 200, "{res}");
    // Malformed parameters.
    let (status, _) = c
        .update(
            &attr(&second, "wire:snapshot"),
            json!([{ "method": "$listen", "params": ["private-orders.7", "OrderShipped"] }]),
        )
        .await;
    assert_eq!(status, 400);
    // Data that does not fit the listener's argument.
    let m = signed(
        &c.app,
        &two,
        "private-orders.7",
        "OrderShipped",
        r#"{"carrier":5}"#,
        later,
        8,
    );
    let (status, _) = c
        .update(&attr(&second, "wire:snapshot"), listen_call(&m))
        .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn undeclared_pairs_and_components_without_listeners_are_refused() {
    let (c, _) = app().await;
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let (id, snapshot) = (attr(&page, "wire:id"), attr(&page, "wire:snapshot"));
    let later = now() + 30;
    // Signed by the server, but no listener of the component takes this event or channel.
    for (channel, event) in [
        ("private-orders.7", "OrderCancelled"),
        ("private-users.1", "OrderShipped"),
        ("news", "OrderShipped"),
    ] {
        let m = signed(&c.app, &id, channel, event, "{}", later, 9);
        let (status, _) = c.update(&snapshot, listen_call(&m)).await;
        assert_eq!(status, 403, "{channel} {event}");
    }
    // A component without listeners has no `$listen`.
    let live = c.get("/live").await;
    let at = live.find("wire:snapshot=\"").unwrap() + "wire:snapshot=\"".len();
    let snapshot = live[at..at + live[at..].find('"').unwrap()].replace("&quot;", "\"");
    let m = signed(&c.app, "x", "news", "Posted", "{}", later, 10);
    let (status, _) = c.update(&snapshot, listen_call(&m)).await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn a_private_channel_is_refused_to_a_user_who_may_not_receive_it() {
    let (c, fake) = app().await;
    c.get("/login/2").await;
    let page = c.get("/orders/7").await;
    let token = attr(&page, "wire:stream");
    assert_eq!(
        claims(&token)["l"],
        json!([["news", "Posted"]]),
        "user 2 does not own order 7"
    );
    let (status, mut stream) = c.stream(&[&token], None).await;
    assert_eq!(status, 200);
    fake.sender.send(ChannelEvent::new(
        "private-orders.7",
        "OrderShipped",
        r#"{"carrier":"DHL"}"#,
    ));
    fake.sender
        .send(ChannelEvent::new("news", "Posted", r#"{"id":1}"#));
    let m = next_data(&mut stream, WAIT).await.unwrap();
    assert_eq!(
        (m["channel"].as_str(), m["data"].as_str()),
        (Some("news"), Some(r#"{"id":1}"#)),
        "the private event never reached the page"
    );

    // A message for the private channel, even signed, is refused when the listener would run: the channel is
    // authorized again for the request's user.
    let m = signed(
        &c.app,
        &attr(&page, "wire:id"),
        "private-orders.7",
        "OrderShipped",
        r#"{"carrier":"DHL"}"#,
        now() + 30,
        3,
    );
    let (status, _) = c
        .update(&attr(&page, "wire:snapshot"), listen_call(&m))
        .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn access_lost_after_render_stops_the_listener() {
    let (c, fake) = app().await;
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let (id, snapshot) = (attr(&page, "wire:id"), attr(&page, "wire:snapshot"));
    let m = signed(
        &c.app,
        &id,
        "private-orders.7",
        "OrderShipped",
        r#"{"carrier":"DHL"}"#,
        now() + 30,
        4,
    );
    fake.deny.store(true, Ordering::SeqCst);
    let (status, _) = c.update(&snapshot, listen_call(&m)).await;
    assert_eq!(status, 403, "re-authorized at $listen");
    // A stream opened now leaves the channel out (re-authorized when the stream opens).
    let (status, mut stream) = c.stream(&[&attr(&page, "wire:stream")], None).await;
    assert_eq!(status, 200);
    fake.sender
        .send(ChannelEvent::new("private-orders.7", "OrderShipped", "{}"));
    fake.sender.send(ChannelEvent::new("news", "Posted", "[]"));
    let m = next_data(&mut stream, WAIT).await.unwrap();
    assert_eq!(
        (m["channel"].as_str(), m["data"].as_str()),
        (Some("news"), Some("[]"))
    );
}

#[tokio::test]
async fn a_changed_state_skips_a_message_for_its_old_channel() {
    let (c, _) = app().await;
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let (id, snapshot) = (attr(&page, "wire:id"), attr(&page, "wire:snapshot"));
    let (status, res) = c
        .update(&snapshot, json!([{ "method": "switch", "params": [9] }]))
        .await;
    assert_eq!(status, 200);
    let switched = res["components"][0]["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    let m = signed(
        &c.app,
        &id,
        "private-orders.7",
        "OrderShipped",
        r#"{"carrier":"DHL"}"#,
        now() + 30,
        11,
    );
    let (status, res) = c.update(&switched, listen_call(&m)).await;
    assert_eq!(status, 200, "{res}");
    let data: Value =
        serde_json::from_str(res["components"][0]["snapshot"].as_str().unwrap()).unwrap();
    assert_eq!(
        data["data"]["status"], "",
        "the listener did not run for order 9"
    );
}

#[tokio::test]
async fn stream_tokens_belong_to_their_session_and_user() {
    let (c, _) = app().await;
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let token = attr(&page, "wire:stream");
    let mine = c.cookie_header();
    assert_eq!(claims(&token)["u"], 1);
    // Without the session cookie, or with another visitor's session, the token is refused.
    let (status, _) = c.stream(&[&token], Some("")).await;
    assert_eq!(status, 403);
    let stranger = {
        let req = http::Request::get("/login/1")
            .body(axum::body::Body::empty())
            .unwrap();
        let res = c.router.clone().oneshot(req).await.unwrap();
        let set = res
            .headers()
            .get(http::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        set.split(';').next().unwrap().to_owned()
    };
    let (status, _) = c.stream(&[&token], Some(&stranger)).await;
    assert_eq!(status, 403, "the same user in another session");
    let (status, _) = c.stream(&[&token], Some(&mine)).await;
    assert_eq!(status, 200);
    // A token minted for a page without a session names no user and works without a cookie.
    let unbound = smeltery_sparks::testing::stream_token(&c.app, "live", "abc").unwrap();
    assert_eq!(claims(&unbound)["s"], "");
    let (status, _) = c.stream(&[&unbound], Some("")).await;
    assert_eq!(status, 200);
    // Mixed: only the tokens of this session subscribe.
    let (status, _) = c.stream(&[&token, &unbound], Some("")).await;
    assert_eq!(status, 200);
}

/// The SECURITY.md §5 residual this closes: a stream kept past the logout went on receiving pushes.
#[tokio::test]
async fn logout_ends_the_stream_and_its_tokens() {
    let (c, _) = app().await;
    let broadcast = Broadcast::of(&c.app).unwrap();
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let token = attr(&page, "wire:stream");
    let (status, mut stream) = c.stream(&[&token], None).await;
    assert_eq!(status, 200);
    broadcast.to("order-status").refresh();
    assert_eq!(
        next_data(&mut stream, WAIT).await.unwrap()["kind"],
        "refresh"
    );
    c.get("/logout").await;
    assert!(
        ended(&mut stream, WAIT).await,
        "the logout ended the stream"
    );
    // The browser's session after the logout is another one: the old token is refused.
    let (status, _) = c.stream(&[&token], None).await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn revocations_end_the_streams_of_the_sessions_they_name() {
    let (c, _) = app().await;
    let broadcast = Broadcast::of(&c.app).unwrap();
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let token = attr(&page, "wire:stream");
    let key = format!("web:session:{}", claims(&token)["s"].as_str().unwrap());
    let (_, mut stream) = c.stream(&[&token], None).await;
    // Events that do not concern this session leave it open.
    for event in [
        AuthEvent::Revoked {
            user_id: 2,
            key: key.clone(),
        },
        AuthEvent::Revoked {
            user_id: 1,
            key: "web:session:other".into(),
        },
        AuthEvent::RevokedAll {
            user_id: 1,
            kind: CredentialKind::Every,
            except: Some(key.clone()),
        },
        AuthEvent::RevokedAll {
            user_id: 1,
            kind: CredentialKind::Tokens,
            except: None,
        },
    ] {
        smeltery_core::auth::publish_event(&c.app, &event)
            .await
            .unwrap();
    }
    broadcast.to("order-status").refresh();
    assert_eq!(
        next_data(&mut stream, WAIT).await.unwrap()["kind"],
        "refresh"
    );
    // `logout_other_devices` / a password change elsewhere: every other session of the user.
    smeltery_core::auth::publish_event(
        &c.app,
        &AuthEvent::RevokedAll {
            user_id: 1,
            kind: CredentialKind::Sessions,
            except: Some("web:session:x".into()),
        },
    )
    .await
    .unwrap();
    assert!(ended(&mut stream, WAIT).await);
    // An event kind this version cannot read, for the user: fail closed.
    let (_, mut stream) = c.stream(&[&token], None).await;
    let bus = smeltery_core::pubsub::PubSub::of(&c.app).unwrap();
    bus.publish_reserved("auth", &json!({ "type": "sessions_expired", "user_id": 1 }))
        .await
        .unwrap();
    assert!(ended(&mut stream, WAIT).await);
}

#[tokio::test]
async fn a_stream_ends_when_its_tokens_expire() {
    let (c, _) = app_with(Some(Duration::from_secs(1))).await;
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let (status, mut stream) = c.stream(&[&attr(&page, "wire:stream")], None).await;
    assert_eq!(status, 200);
    assert!(
        !ended(&mut stream, QUIET).await,
        "open while the token is valid"
    );
    assert!(ended(&mut stream, Duration::from_secs(4)).await);
}

#[tokio::test]
async fn the_render_authorizes_once_per_channel() {
    let (c, fake) = app().await;
    c.get("/login/1").await;
    let before = fake.decisions.load(Ordering::SeqCst);
    c.get("/orders/7").await;
    assert_eq!(
        fake.decisions.load(Ordering::SeqCst) - before,
        2,
        "two channels"
    );
}

// ---------------------------------------------------------------- boot checks

/// A listener component on an app without a channel authorizer (Anvil not installed).
#[tokio::test]
async fn listeners_without_a_broadcasting_crate_fail_the_boot() {
    let err = AppBuilder::new(settings())
        .sparks(register)
        .build()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Anvil is not installed"), "{err}");
}

#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(
    crate = "smeltery_sparks",
    dir = "tests/app/resources/views",
    view = "sparks/live"
)]
pub struct NoStream {
    pub n: i64,
}

#[actions(crate = "smeltery_sparks")]
impl NoStream {
    #[on("anvil:news", "Posted")]
    async fn posted(&mut self) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Spark)]
#[spark(
    crate = "smeltery_sparks",
    dir = "tests/app/resources/views",
    view = "sparks/live",
    stream
)]
pub struct Misnamed {
    pub n: i64,
}

#[actions(crate = "smeltery_sparks")]
impl Misnamed {
    #[on("anvil:private-orders.{order}", "Posted")]
    async fn posted(&mut self) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn listeners_need_stream_and_the_fields_they_name() {
    let err = AppBuilder::new(settings())
        .channel_authorizer(Fake::default())
        .sparks(|s| {
            s.add::<NoStream>();
        })
        .build()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("add `stream`"), "{err}");
    let err = AppBuilder::new(settings())
        .channel_authorizer(Fake::default())
        .sparks(|s| {
            s.add::<Misnamed>();
        })
        .build()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("`{order}`"), "{err}");
    // `extend` checks what it adds.
    let (c, _) = app().await;
    let err = smeltery_sparks::extend(&c.app, |s| {
        s.add::<Misnamed>();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("`{order}`"), "{err}");
}

fn status_of(res: &Value) -> Value {
    let snapshot: Value =
        serde_json::from_str(res["components"][0]["snapshot"].as_str().unwrap()).unwrap();
    snapshot["data"]["status"].clone()
}

fn snapshot_of(res: &Value) -> String {
    res["components"][0]["snapshot"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Review M-1: the channel is checked against the state the listener runs on, after the request's updates and
/// earlier calls; a batch that changes the state first is not refused, the listener is skipped.
#[tokio::test]
async fn the_listener_runs_only_if_the_state_names_the_channel_when_it_runs() {
    let (c, _) = app().await;
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let (id, snapshot) = (attr(&page, "wire:id"), attr(&page, "wire:snapshot"));
    let data = r#"{"carrier":"DHL"}"#;
    // An action that switches to order 9 first.
    let m = signed(
        &c.app,
        &id,
        "private-orders.7",
        "OrderShipped",
        data,
        now() + 30,
        21,
    );
    let mut calls = json!([{ "method": "switch", "params": [9] }]);
    calls
        .as_array_mut()
        .unwrap()
        .extend(listen_call(&m).as_array().unwrap().clone());
    let (status, res) = c.update(&snapshot, calls).await;
    assert_eq!(status, 200, "{res}");
    assert_eq!(status_of(&res), "", "the listener did not run on order 9");
    // A model update batched with the listen call (as sparks.js sends them).
    let m = signed(
        &c.app,
        &id,
        "private-orders.7",
        "OrderShipped",
        data,
        now() + 30,
        22,
    );
    let (status, res) = c
        .update_with(&snapshot, json!({ "order_id": 9 }), listen_call(&m))
        .await;
    assert_eq!(status, 200, "{res}");
    assert_eq!(status_of(&res), "");
    // An unrelated model update in the same batch: the listener runs.
    let m = signed(
        &c.app,
        &id,
        "private-orders.7",
        "OrderShipped",
        data,
        now() + 30,
        23,
    );
    let (status, res) = c
        .update_with(&snapshot, json!({ "note": "hi" }), listen_call(&m))
        .await;
    assert_eq!(status, 200, "{res}");
    assert_eq!(status_of(&res), "shipped by DHL for Some(1)");
}

/// Review L-4: a listen message that did not run because an earlier call failed validation is not consumed.
#[tokio::test]
async fn a_listen_message_skipped_by_a_failed_call_can_still_run() {
    let (c, _) = app().await;
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let (id, snapshot) = (attr(&page, "wire:id"), attr(&page, "wire:snapshot"));
    let m = signed(
        &c.app,
        &id,
        "private-orders.7",
        "OrderShipped",
        r#"{"carrier":"DHL"}"#,
        now() + 30,
        31,
    );
    let mut calls = json!([{ "method": "fail", "params": [] }]);
    calls
        .as_array_mut()
        .unwrap()
        .extend(listen_call(&m).as_array().unwrap().clone());
    let (status, res) = c.update(&snapshot, calls).await;
    assert_eq!(status, 200, "{res}");
    assert_eq!(status_of(&res), "");
    let (status, res) = c.update(&snapshot_of(&res), listen_call(&m)).await;
    assert_eq!(status, 200, "{res}");
    assert_eq!(status_of(&res), "shipped by DHL for Some(1)");
}

/// Review L-1: a process at its stream cap asks no channel rule; opens are budgeted per client address.
#[tokio::test]
async fn a_full_or_throttled_stream_does_no_channel_work() {
    let fake = Fake::default();
    let built = AppBuilder::new(settings())
        .channel_authorizer(fake.clone())
        .sparks(|s| {
            register(s);
            s.max_streams(1).stream_opens_per_minute(3);
        })
        .routes(|r| {
            r.get("/orders/{order}", orders);
            r.get("/login/{id}", login);
        })
        .build()
        .await
        .unwrap();
    let c = Client {
        app: built.app,
        router: built.router,
        cookies: std::sync::Mutex::new(BTreeMap::new()),
    };
    c.get("/login/1").await;
    let token = attr(&c.get("/orders/7").await, "wire:stream");
    let open = |addr: &str| {
        let req = http::Request::get(format!("/_sparks/stream?t={token}"))
            .header(http::header::COOKIE, c.cookie_header())
            .extension(axum::extract::ConnectInfo(
                addr.parse::<std::net::SocketAddr>().unwrap(),
            ))
            .body(axum::body::Body::empty())
            .unwrap();
        c.router.clone().oneshot(req)
    };
    let first = open("203.0.113.1:1000").await.unwrap();
    assert_eq!(first.status(), 200);
    let before = fake.decisions.load(Ordering::SeqCst);
    let full = open("203.0.113.2:1000").await.unwrap();
    assert_eq!(full.status(), 503);
    assert_eq!(
        fake.decisions.load(Ordering::SeqCst),
        before,
        "no callback at the cap"
    );
    drop(first);
    // Three opens a minute from one address: the fourth is refused before any work.
    for _ in 0..3 {
        assert_eq!(open("203.0.113.3:1000").await.unwrap().status(), 200);
    }
    let before = fake.decisions.load(Ordering::SeqCst);
    assert_eq!(open("203.0.113.3:1000").await.unwrap().status(), 429);
    assert_eq!(fake.decisions.load(Ordering::SeqCst), before);
    assert_eq!(
        open("203.0.113.4:1000").await.unwrap().status(),
        200,
        "another client"
    );
}

/// Review I-2: a guest token of the page before the sign-in, next to a token of the signed-in session: only the
/// signed-in one subscribes.
#[tokio::test]
async fn a_guest_token_next_to_a_signed_in_one_is_left_out() {
    let (c, _) = app().await;
    let broadcast = Broadcast::of(&c.app).unwrap();
    let guest_page = c.get("/orders/7").await;
    let guest = attr(&guest_page, "wire:stream");
    assert_eq!(claims(&guest)["u"], Value::Null);
    assert_ne!(claims(&guest)["s"], "");
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let (status, mut stream) = c.stream(&[&guest, &attr(&page, "wire:stream")], None).await;
    assert_eq!(status, 200);
    broadcast.to(attr(&guest_page, "wire:id")).refresh();
    broadcast.to(attr(&page, "wire:id")).refresh();
    let m = next_data(&mut stream, WAIT).await.unwrap();
    assert_eq!(
        m["target"],
        attr(&page, "wire:id").as_str(),
        "the guest token was left out"
    );
    // Alone, the guest token of the old session is refused.
    let (status, _) = c.stream(&[&guest], None).await;
    assert_eq!(status, 403);
}

/// Review L-3: the stream tells the page that its session ended before it closes.
#[tokio::test]
async fn a_signed_out_stream_says_so_before_it_ends() {
    let (c, _) = app().await;
    c.get("/login/1").await;
    let page = c.get("/orders/7").await;
    let (_, mut stream) = c.stream(&[&attr(&page, "wire:stream")], None).await;
    c.get("/logout").await;
    assert_eq!(
        next_data(&mut stream, WAIT).await.unwrap(),
        json!({ "kind": "end" })
    );
    assert!(ended(&mut stream, WAIT).await);
}

/// Re-review L-A: the `null` cache store keeps no "ran once" record, so listeners refuse to boot on it.
#[tokio::test]
async fn listeners_need_a_cache_store_that_keeps_values() {
    let mut settings = settings();
    settings.cache_store = "null".into();
    let err = AppBuilder::new(settings)
        .channel_authorizer(Fake::default())
        .sparks(register)
        .build()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("CACHE_STORE=null"), "{err}");
}

/// Re-review L-C: an IPv6 client is budgeted by its /64, so rotating addresses inside it gives no fresh budget.
#[tokio::test]
async fn the_stream_open_budget_counts_an_ipv6_network_once() {
    let fake = Fake::default();
    let built = AppBuilder::new(settings())
        .channel_authorizer(fake)
        .sparks(|s| {
            register(s);
            s.stream_opens_per_minute(2);
        })
        .routes(|r| {
            r.get("/orders/{order}", orders);
            r.get("/login/{id}", login);
        })
        .build()
        .await
        .unwrap();
    let c = Client {
        app: built.app,
        router: built.router,
        cookies: std::sync::Mutex::new(BTreeMap::new()),
    };
    c.get("/login/1").await;
    let token = attr(&c.get("/orders/7").await, "wire:stream");
    let open = |addr: &str| {
        let req = http::Request::get(format!("/_sparks/stream?t={token}"))
            .header(http::header::COOKIE, c.cookie_header())
            .extension(axum::extract::ConnectInfo(
                addr.parse::<std::net::SocketAddr>().unwrap(),
            ))
            .body(axum::body::Body::empty())
            .unwrap();
        c.router.clone().oneshot(req)
    };
    assert_eq!(open("[2001:db8:1:2::1]:1000").await.unwrap().status(), 200);
    assert_eq!(open("[2001:db8:1:2::2]:1000").await.unwrap().status(), 200);
    assert_eq!(
        open("[2001:db8:1:2:ffff::9]:1000").await.unwrap().status(),
        429,
        "the same /64"
    );
    assert_eq!(
        open("[2001:db8:1:3::1]:1000").await.unwrap().status(),
        200,
        "another /64"
    );
}
