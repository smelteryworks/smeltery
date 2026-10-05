//! Server push: [`Broadcast`] and `GET /_sparks/stream` (server-sent events).

use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::Query;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use smeltery_core::auth::Auth;
use smeltery_core::pubsub::PubSub;
use smeltery_core::session::Session;
use smeltery_core::{App, Error};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;

/// How many messages wait for a slow stream before it is disconnected.
const CAPACITY: usize = 256;
/// The keep-alive comment interval.
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// The most targets one stream subscribes to.
const MAX_TARGETS: usize = 64;

/// Pushes to live components on open pages (components declared with `#[spark(stream)]`): a refresh (the
/// page re-fetches the component through its normal update request) or a browser event.
///
/// A cheap clone. Handlers take it as an argument; jobs and agents get it with `app.service::<Broadcast>()`.
///
/// ```
/// # use smeltery_sparks::Broadcast;
/// # use serde_json::json;
/// async fn tick(broadcast: Broadcast) {
///     broadcast.to("counter").refresh();
///     broadcast.to("counter").emit("tick", json!({ "n": 3 }));
/// }
/// ```
///
/// Messages carry no component state and reach every open page subscribed to the target, so a payload never
/// holds anything private.
#[derive(Clone)]
pub struct Broadcast {
    tx: broadcast::Sender<Arc<Message>>,
    /// The app's PubSub, set when the app builds: messages also go to its other processes.
    bus: Arc<OnceLock<PubSub>>,
}

/// The PubSub topic of Sparks pushes.
pub(crate) const TOPIC: &str = "sparks";

/// The longest target name a message from another process may carry.
const MAX_TARGET: usize = 512;

impl std::fmt::Debug for Broadcast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Broadcast")
            .field("streams", &self.tx.receiver_count())
            .finish()
    }
}

impl Default for Broadcast {
    fn default() -> Self {
        Self::new()
    }
}

/// One pushed message.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Message {
    pub(crate) target: String,
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) payload: Option<serde_json::Value>,
}

impl Broadcast {
    /// A broadcast with no listeners (the app's comes from `.sparks(…)`).
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self {
            tx,
            bus: Arc::new(OnceLock::new()),
        }
    }

    /// The broadcast of `app`, when Sparks are installed.
    pub fn of(app: &App) -> Option<Self> {
        app.service::<Self>().map(|b| (*b).clone())
    }

    /// Address the instances of component `target` (a name such as `counter`) or one instance (its `wire:id`).
    pub fn to(&self, target: impl Into<String>) -> Target<'_> {
        Target {
            broadcast: self,
            target: target.into(),
        }
    }

    /// How many pages are connected to this process.
    pub fn streams(&self) -> usize {
        self.tx.receiver_count()
    }

    fn send(&self, message: Message) -> usize {
        let message = Arc::new(message);
        // No connected page is not an error: the message has nobody to reach.
        let local = self.tx.send(Arc::clone(&message)).unwrap_or(0);
        // The pages of the app's other processes: queued for the PubSub (never blocks; a full queue is counted and
        // logged by the PubSub).
        if let Some(bus) = self.bus.get() {
            let _ = bus.forward_reserved(TOPIC, &*message);
        }
        local
    }

    /// Send through `bus` to the app's other processes too (the app's PubSub, set when the app builds).
    pub(crate) fn attach(&self, bus: PubSub) {
        let _ = self.bus.set(bus);
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Arc<Message>> {
        self.tx.subscribe()
    }
}

/// A [`Broadcast`] addressed to one target.
#[derive(Debug)]
pub struct Target<'a> {
    broadcast: &'a Broadcast,
    target: String,
}

impl Target<'_> {
    /// Make the subscribed pages re-fetch the component (a `$refresh` update). Returns how many pages were
    /// connected.
    pub fn refresh(self) -> usize {
        self.broadcast.send(Message {
            target: self.target,
            kind: "refresh",
            event: None,
            payload: None,
        })
    }

    /// Fire the browser event `event` with `payload` on the subscribed pages. Returns how many pages were
    /// connected. Names starting with `anvil:` are reserved for listener events (the page fires `anvil:<event>` when
    /// a listener's broadcast arrives): such an `emit` sends nothing, logs a warning and returns 0.
    pub fn emit(self, event: impl Into<String>, payload: impl Serialize) -> usize {
        let event = event.into();
        if event.starts_with("anvil:") {
            tracing::warn!(
                "Sparks: `emit` names starting with `anvil:` are reserved for listener events; nothing was sent"
            );
            return 0;
        }
        let payload = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        self.broadcast.send(Message {
            target: self.target,
            kind: "event",
            event: Some(event),
            payload: Some(payload),
        })
    }
}

/// A [`Message`] as another process sent it.
#[derive(Debug, Deserialize)]
struct Remote {
    target: String,
    kind: String,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    payload: Option<serde_json::Value>,
}

impl Message {
    /// The message another process published on the Sparks topic, when it has a known shape.
    fn from_remote(payload: &serde_json::Value) -> Option<Self> {
        let remote = Remote::deserialize(payload).ok()?;
        let kind = match remote.kind.as_str() {
            "refresh" => "refresh",
            "event" => "event",
            _ => return None,
        };
        // An event's name follows the local rules too: `anvil:` names belong to listener events (`Target::emit`).
        if remote
            .event
            .as_deref()
            .is_some_and(|e| e.len() > MAX_TARGET || e.starts_with("anvil:"))
        {
            return None;
        }
        (!remote.target.is_empty() && remote.target.len() <= MAX_TARGET).then_some(Self {
            target: remote.target,
            kind,
            event: remote.event,
            payload: remote.payload,
        })
    }
}

/// Hand what the app's other processes push to the streams of this process, until shutdown. Messages sent here
/// never come back (the PubSub skips its own), and relayed ones are not forwarded again.
pub(crate) async fn relay(broadcast: Broadcast, bus: PubSub, token: CancellationToken) {
    let mut messages = bus.subscribe(TOPIC);
    loop {
        let received = tokio::select! {
            biased;
            () = token.cancelled() => return,
            received = messages.recv() => received,
        };
        match received {
            Ok(message) if message.remote => match Message::from_remote(&message.payload) {
                Some(message) => {
                    let _ = broadcast.tx.send(Arc::new(message));
                }
                None => tracing::debug!("Sparks: a push from another process has an unknown shape"),
            },
            Ok(_) => {}
            Err(smeltery_core::pubsub::RecvError::Lagged(missed)) => {
                tracing::warn!(
                    missed,
                    "Sparks: pushes from other processes were skipped (too many at once)"
                );
            }
            Err(_) => return,
        }
    }
}

impl axum::extract::FromRequestParts<App> for Broadcast {
    type Rejection = Error;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> Result<Self, Self::Rejection> {
        Self::of(app).ok_or_else(|| {
            Error::internal("Sparks are not installed: call `.sparks(…)` in bootstrap/app.rs")
        })
    }
}

/// The query of `GET /_sparks/stream`: comma-separated stream tokens (`wire:stream` values).
#[derive(Debug, Deserialize)]
pub(crate) struct StreamQuery {
    #[serde(default)]
    t: String,
}

/// The signing purpose of stream tokens.
const STREAM_PURPOSE: &str = "sparks.stream";

/// The longest stream token the stream reads.
const MAX_TOKEN: usize = 4096;

/// What a stream token holds: the component name, the instance id, the expiry (Unix seconds), the session and user
/// it was issued to, and the `(channel, event)` pairs its listeners may receive.
#[derive(Debug, Serialize, Deserialize)]
struct StreamClaims {
    n: String,
    i: String,
    e: u64,
    /// The session binding (`Session::binding`) of the render that issued it; empty when the page had no session.
    /// Required: a token without it (from an older protocol) is refused.
    s: String,
    /// The signed-in user of that render.
    u: Option<i64>,
    /// The listener grants: `[channel, event]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    l: Vec<(String, String)>,
}

/// Who a render was for: its session and sign-in (both `None` on a page without a session).
#[derive(Clone, Copy, Default)]
pub(crate) struct Viewer<'a> {
    pub(crate) session: Option<&'a Session>,
    pub(crate) auth: Option<&'a Auth>,
}

/// A stream token for instance `id` of component `name`, valid for `ttl`, bound to `viewer`'s session and user, with
/// the listener grants `listens`: issued with a render the component's `can_stream` hook allowed, it is what lets a
/// page subscribe to that name and id.
pub(crate) fn stream_token(
    app: &App,
    name: &str,
    id: &str,
    ttl: Duration,
    viewer: Viewer<'_>,
    listens: Vec<(String, String)>,
) -> smeltery_core::Result<String> {
    use base64::Engine as _;
    let claims = StreamClaims {
        n: name.to_owned(),
        i: id.to_owned(),
        e: crate::upload::now_secs().saturating_add(ttl.as_secs()),
        s: viewer
            .session
            .map(crate::upload::binding)
            .unwrap_or_default(),
        u: viewer.auth.and_then(Auth::id),
        l: listens,
    };
    let body =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?);
    let signature = app.sign(STREAM_PURPOSE, body.as_bytes())?;
    let token = format!("{body}.{signature}");
    if token.len() > MAX_TOKEN {
        return Err(Error::internal(format!(
            "Spark `{name}`: its stream token is longer than {MAX_TOKEN} bytes (too many or too long listener \
             channels)"
        )));
    }
    Ok(token)
}

/// The claims of a stream token, when it is signed here and not expired.
fn open_stream_token(app: &App, token: &str) -> Option<StreamClaims> {
    use base64::Engine as _;
    let (body, signature) = token.split_once('.')?;
    if !app.verify_signature(STREAM_PURPOSE, body.as_bytes(), signature) {
        return None;
    }
    let claims: StreamClaims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    (crate::upload::now_secs() < claims.e).then_some(claims)
}

enum Next {
    Send(Event),
    Skip,
    End,
    /// Send this last message, then end.
    Last(Event),
}

/// The most distinct listener channels one stream request authorizes again (the rest are left out).
const MAX_CHANNELS: usize = 64;

/// The stream opens of each client address in the current minute (process memory).
#[derive(Default)]
pub(crate) struct OpenBudget(std::sync::Mutex<std::collections::HashMap<String, (u64, u32)>>);

/// The most client addresses the open budget tracks; past it (after dropping old windows) new addresses are not
/// budgeted, so a flood of addresses never locks every visitor out.
const MAX_BUDGETED: usize = 10_000;

impl OpenBudget {
    /// Count one open for `client`; `false` when it already made `max` this minute.
    pub(crate) fn hit(&self, client: &str, max: u32) -> bool {
        let minute = crate::upload::now_secs() / 60;
        let mut counts = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if counts.len() >= MAX_BUDGETED && !counts.contains_key(client) {
            counts.retain(|_, (window, _)| *window == minute);
            if counts.len() >= MAX_BUDGETED {
                return true;
            }
        }
        let entry = counts.entry(client.to_owned()).or_insert((minute, 0));
        if entry.0 != minute {
            *entry = (minute, 0);
        }
        if entry.1 >= max {
            return false;
        }
        entry.1 += 1;
        true
    }
}

/// One open stream connection, counted in the runtime's total while it lives.
struct StreamSlot(Arc<AtomicUsize>);

impl StreamSlot {
    /// A slot, unless `max` connections are open.
    fn take(open: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        open.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < max).then_some(n + 1)
        })
        .ok()
        .map(|_| Self(Arc::clone(open)))
    }
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What one stream connection receives: the targets of its tokens, its listeners and what ends it.
struct Subscribed {
    /// Component names and instance ids (refresh and `emit` pushes).
    targets: HashSet<String>,
    /// `(instance id, channel, event)` of the listeners.
    listens: Vec<(String, String, String)>,
    /// `(user, credential key)` of the signed-in sessions the tokens were issued to: an auth event ending one ends the
    /// stream.
    watch: Vec<(i64, String)>,
    /// The earliest expiry of the tokens (Unix seconds): the stream ends then.
    expires: u64,
}

/// Whether a token issued to session binding `s` and user `u` belongs to this request's session (`auth`, from
/// `Auth::peek`). A token issued without a session (an API page) has no session to match and never names a user.
fn belongs(claims: &StreamClaims, auth: Option<&Auth>, binding: Option<&str>) -> bool {
    if claims.s.is_empty() {
        return claims.u.is_none();
    }
    let (Some(auth), Some(binding)) = (auth, binding) else {
        return false;
    };
    crate::upload::same_secret(&claims.s, binding) && claims.u == auth.id()
}

/// Open the request's tokens: the valid ones that belong to this session, with their listener channels authorized
/// again for the session's viewer.
async fn subscribe(
    app: &App,
    headers: &http::HeaderMap,
    ip: String,
    query: &str,
) -> smeltery_core::Result<Option<Subscribed>> {
    let claims: Vec<StreamClaims> = query
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.len() <= MAX_TOKEN)
        .take(MAX_TARGETS)
        .filter_map(|token| open_stream_token(app, token))
        .collect();
    if claims.is_empty() {
        return Ok(None);
    }
    // The session is read only when a token names one.
    let session = if claims.iter().any(|c| !c.s.is_empty()) {
        smeltery_core::session::peek(app, headers).await?
    } else {
        None
    };
    let binding = session.as_ref().map(Session::binding);
    let auth = match session {
        Some(session) => Some(Auth::peek(app, session, ip).await?),
        None => None,
    };
    let mut out = Subscribed {
        targets: HashSet::new(),
        listens: Vec::new(),
        watch: Vec::new(),
        expires: u64::MAX,
    };
    let authorizer = app.channel_authorizer();
    let mut decided: Vec<(String, bool)> = Vec::new();
    for claim in claims {
        if !belongs(&claim, auth.as_ref(), binding.as_deref()) {
            tracing::warn!(
                component = %claim.n,
                "Sparks stream token refused: issued to another session or user"
            );
            continue;
        }
        for (channel, event) in &claim.l {
            let allowed = match decided.iter().find(|(c, _)| c == channel) {
                Some((_, allowed)) => *allowed,
                None if decided.len() >= MAX_CHANNELS => {
                    tracing::warn!(
                        component = %claim.n,
                        "Sparks stream: more than {MAX_CHANNELS} listener channels; the rest are left out"
                    );
                    continue;
                }
                None => {
                    let allowed = match &authorizer {
                        Some(authorizer) => matches!(
                            authorizer.authorize(app, channel, auth.as_ref()).await,
                            Ok(true)
                        ),
                        None => false,
                    };
                    decided.push((channel.clone(), allowed));
                    allowed
                }
            };
            if allowed {
                out.listens
                    .push((claim.i.clone(), channel.clone(), event.clone()));
            }
        }
        if let Some(user) = claim.u {
            let key = smeltery_core::auth::credential_key(
                smeltery_core::auth::WEB_GUARD,
                "session",
                &claim.s,
            );
            if !out.watch.contains(&(user, key.clone())) {
                out.watch.push((user, key));
            }
        }
        out.expires = out.expires.min(claim.e);
        out.targets.insert(claim.n);
        out.targets.insert(claim.i);
    }
    Ok((!out.targets.is_empty()).then_some(out))
}

/// Whether the auth event `payload` ends a session in `watch`. An event this version cannot read ends the stream when
/// it names a watched user (fail closed).
fn ends_a_session(payload: &serde_json::Value, watch: &[(i64, String)]) -> Ended {
    match serde_json::from_value::<smeltery_core::auth::AuthEvent>(payload.clone()) {
        Ok(event) if watch.iter().any(|(user, key)| event.ends(*user, key)) => Ended::SignedOut,
        Ok(_) => Ended::No,
        Err(_) => {
            let user = payload.get("user_id").and_then(serde_json::Value::as_i64);
            if watch.iter().any(|(u, _)| Some(*u) == user) {
                Ended::Unsure
            } else {
                Ended::No
            }
        }
    }
}

/// What an auth event means for a stream.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// It does not concern the stream's sessions.
    No,
    /// It ended a session of the stream (a parsed event matched): the page is told (`end`) and stops reopening.
    SignedOut,
    /// An event this version cannot read names a user of the stream: the stream closes without `end`, so the
    /// page reopens and the session is checked again (fail closed, without stopping a still-valid page).
    Unsure,
}

/// The listen messages one broadcast event makes for this stream's listeners.
fn listen_events(
    app: &App,
    listens: &[(String, String, String)],
    event: &smeltery_core::channels::ChannelEvent,
) -> Vec<Event> {
    if event.data.len() > crate::listen::MAX_DATA {
        tracing::debug!("Sparks stream: an event too large for a listener was not forwarded");
        return Vec::new();
    }
    listens
        .iter()
        .filter(|(_, channel, name)| *channel == event.channel && *name == event.event)
        .filter_map(|(id, channel, name)| {
            // The data's hash is computed once per event, shared by every stream it reaches.
            let (exp, seq, sig) =
                crate::listen::sign(app, id, channel, name, event.data_sha256()).ok()?;
            Event::default()
                .json_data(serde_json::json!({
                    "target": id,
                    "kind": "listen",
                    "channel": channel,
                    "event": name,
                    "data": event.data,
                    "exp": exp,
                    "seq": seq,
                    "sig": sig,
                }))
                .ok()
        })
        .collect()
}

/// The state of one stream connection while it runs.
struct Running {
    app: App,
    rx: broadcast::Receiver<Arc<Message>>,
    events: Option<smeltery_core::channels::ChannelEvents>,
    revocations: Option<smeltery_core::pubsub::Subscription>,
    token: CancellationToken,
    subscribed: Subscribed,
    pending: std::collections::VecDeque<Event>,
    _slot: StreamSlot,
}

impl Running {
    async fn next(&mut self) -> Next {
        if let Some(event) = self.pending.pop_front() {
            return Next::Send(event);
        }
        let now = crate::upload::now_secs();
        let until_expiry = Duration::from_secs(self.subscribed.expires.saturating_sub(now));
        let Self {
            app,
            rx,
            events,
            revocations,
            token,
            subscribed,
            pending,
            ..
        } = self;
        tokio::select! {
            () = token.cancelled() => Next::End,
            // The tokens expired: the page reopens the stream with the tokens of its latest renders.
            () = tokio::time::sleep(until_expiry) => Next::End,
            received = rx.recv() => match received {
                Ok(message) if subscribed.targets.contains(&message.target) => {
                    match Event::default().json_data(&*message) {
                        Ok(event) => Next::Send(event),
                        Err(_) => Next::Skip,
                    }
                }
                Ok(_) => Next::Skip,
                // Too slow: disconnect; the client reconnects and refreshes everything.
                Err(broadcast::error::RecvError::Lagged(_) | broadcast::error::RecvError::Closed) => Next::End,
            },
            received = async {
                match events.as_mut() {
                    Some(events) => events.recv().await,
                    None => std::future::pending().await,
                }
            } => match received {
                Ok(event) => {
                    pending.extend(listen_events(app, &subscribed.listens, &event));
                    Next::Skip
                }
                // Missed events: disconnect; the client reconnects and refreshes everything.
                Err(_) => Next::End,
            },
            received = async {
                match revocations.as_mut() {
                    Some(sub) => sub.recv().await,
                    None => std::future::pending().await,
                }
            } => match received {
                Ok(message) => match ends_a_session(&message.payload, &subscribed.watch) {
                    Ended::SignedOut => {
                        tracing::info!("Sparks stream closed: its session was signed out");
                        // The page stops reopening the stream: its tokens belong to a session that ended.
                        match Event::default().json_data(serde_json::json!({ "kind": "end" })) {
                            Ok(event) => Next::Last(event),
                            Err(_) => Next::End,
                        }
                    }
                    Ended::Unsure => {
                        tracing::info!("Sparks stream closed: an auth event of its user this version cannot read");
                        Next::End
                    }
                    Ended::No => Next::Skip,
                },
                // Auth events were missed (or the PubSub is gone): fail closed; the reconnect checks the session.
                Err(_) => Next::End,
            },
        }
    }
}

/// `GET /_sparks/stream?t=<token>,<token>`: server-sent events for the component names and instance ids the
/// tokens allow, until shutdown, the tokens' expiry or the end of the session they were issued to.
///
/// The stream is an API route (no session middleware, nothing stored, no cookie set), but it reads the session
/// cookie with `Auth::peek`: each `wire:stream` token was issued with a render of the component for this visitor,
/// after its `can_stream` hook allowed it, is bound to that render's session and user and expires with the
/// snapshot time to live. A token of another session or user, or without a session when it names one, is ignored;
/// without a valid token it answers 403. While it runs, an auth event that ends a session a token was issued to
/// (logout, `logout_other_devices`, a password change or reset, `end_credentials`) ends the stream, and so does a
/// gap in those events. Messages say "refresh" (the page then sends an ordinary update request with its own
/// session, guards and snapshot), carry `emit` payloads (public to the subscribers), or carry a listener's event
/// signed for its instance (`listen`). At most `Sparks::max_streams` connections are open per process; past that it
/// answers 503.
pub(crate) async fn stream(
    app: App,
    headers: http::HeaderMap,
    client: smeltery_core::http::ClientInfo,
    Query(query): Query<StreamQuery>,
) -> Response {
    let Ok(runtime) = Runtime::of(&app) else {
        return Error::not_found().into_response();
    };
    let ip = client
        .ip()
        .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    // Opens are budgeted per client address (an IPv6 client by its /64) before any work (session read, channel
    // callbacks); requests without a known address are not (as the upload address quota).
    if let Some(address) = client.ip().map(crate::upload::quota_ip)
        && !runtime
            .stream_opens
            .hit(&address, runtime.limits.stream_opens)
    {
        tracing::warn!("Sparks stream refused: too many opens from one client");
        return Error::http(
            http::StatusCode::TOO_MANY_REQUESTS,
            "Too many stream requests",
        )
        .into_response();
    }
    // The slot first: a process at its cap does no further work for the request.
    let Some(slot) = StreamSlot::take(&runtime.streams, runtime.limits.max_streams) else {
        tracing::warn!("Sparks stream refused: the connection cap is reached");
        return Error::http(
            http::StatusCode::SERVICE_UNAVAILABLE,
            "Too many open streams",
        )
        .into_response();
    };
    // Subscribed before the session is read: a logout between the two cannot be missed.
    let revocations = smeltery_core::pubsub::PubSub::of(&app)
        .map(|bus| bus.subscribe(smeltery_core::auth::EVENTS_TOPIC));
    let subscribed = match subscribe(&app, &headers, ip, &query.t).await {
        Ok(Some(subscribed)) => subscribed,
        Ok(None) => {
            tracing::warn!("Sparks stream refused: no valid stream token");
            return Error::forbidden().into_response();
        }
        Err(e) => return e.into_response(),
    };
    let events = if subscribed.listens.is_empty() {
        None
    } else {
        app.channel_authorizer().map(|a| a.events())
    };
    let revocations = if subscribed.watch.is_empty() {
        None
    } else {
        revocations
    };
    let running = Running {
        rx: runtime.broadcast.subscribe(),
        events,
        revocations,
        token: app.shutdown_token().clone(),
        app,
        subscribed,
        pending: std::collections::VecDeque::new(),
        _slot: slot,
    };
    // The slot lives in the stream's state: it is given back when the connection's stream is dropped.
    let events = futures_util::stream::unfold(Some(running), |running| async move {
        let mut running = running?;
        loop {
            match running.next().await {
                Next::Send(event) => return Some((Ok::<_, Infallible>(event), Some(running))),
                Next::Last(event) => return Some((Ok::<_, Infallible>(event), None)),
                Next::Skip => {}
                Next::End => return None,
            }
        }
    });
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts its decisions; allows everything.
    #[derive(Default, Clone)]
    struct Counting(
        Arc<AtomicUsize>,
        smeltery_core::channels::ChannelEventSender,
    );

    impl smeltery_core::channels::ChannelAuthorizer for Counting {
        fn authorize<'a>(
            &'a self,
            _app: &'a App,
            _channel: &'a str,
            _auth: Option<&'a Auth>,
        ) -> smeltery_core::BoxFuture<'a, smeltery_core::Result<bool>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(Ok(true)))
        }
        fn events(&self) -> smeltery_core::channels::ChannelEvents {
            self.1.subscribe()
        }
    }

    /// Review L-1: one stream request asks the channel rules about at most `MAX_CHANNELS` distinct channels.
    #[tokio::test]
    async fn a_stream_request_authorizes_a_bounded_number_of_channels() {
        let counting = Counting::default();
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.env = "testing".into();
        let app = smeltery_core::AppBuilder::new(settings)
            .channel_authorizer(counting.clone())
            .build()
            .await
            .unwrap()
            .app;
        let listens: Vec<(String, String)> = (0..100)
            .map(|i| (format!("c{i}"), "E".to_owned()))
            .collect();
        let token = stream_token(
            &app,
            "live",
            "abc",
            Duration::from_secs(60),
            Viewer::default(),
            listens,
        )
        .unwrap();
        let subscribed = subscribe(&app, &http::HeaderMap::new(), "unknown".into(), &token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(counting.0.load(Ordering::SeqCst), MAX_CHANNELS);
        assert_eq!(subscribed.listens.len(), MAX_CHANNELS);
    }

    /// Sweep W6-04: a relayed push obeys `emit`'s rule: no `anvil:` event names (reserved for listener events).
    #[test]
    fn relayed_pushes_cannot_name_listener_events() {
        let push = |event: &str| serde_json::json!({ "target": "live", "kind": "event", "event": event, "payload": 1 });
        assert!(Message::from_remote(&push("tick")).is_some());
        assert!(Message::from_remote(&push("anvil:OrderShipped")).is_none());
        assert!(Message::from_remote(&push(&"e".repeat(MAX_TARGET + 1))).is_none());
        let refresh = serde_json::json!({ "target": "live", "kind": "refresh" });
        assert!(Message::from_remote(&refresh).is_some());
    }

    #[test]
    fn only_a_parsed_matching_event_tells_the_page_to_stop() {
        let watch = vec![(7, "web:session:ab".to_owned())];
        let logout =
            serde_json::json!({ "type": "revoked", "user_id": 7, "key": "web:session:ab" });
        assert_eq!(ends_a_session(&logout, &watch), Ended::SignedOut);
        let other = serde_json::json!({ "type": "revoked", "user_id": 7, "key": "web:session:cd" });
        assert_eq!(ends_a_session(&other, &watch), Ended::No);
        let unknown = serde_json::json!({ "type": "sessions_expired", "user_id": 7 });
        assert_eq!(ends_a_session(&unknown, &watch), Ended::Unsure);
        let stranger = serde_json::json!({ "type": "sessions_expired", "user_id": 8 });
        assert_eq!(ends_a_session(&stranger, &watch), Ended::No);
    }

    #[test]
    fn the_open_budget_counts_per_client_and_never_grows_without_bound() {
        let budget = OpenBudget::default();
        assert!(budget.hit("a", 2) && budget.hit("a", 2));
        assert!(!budget.hit("a", 2));
        assert!(budget.hit("b", 2));
        for i in 0..MAX_BUDGETED + 10 {
            budget.hit(&format!("c{i}"), 1);
        }
        assert!(budget.0.lock().unwrap().len() <= MAX_BUDGETED);
        assert!(
            budget.hit("new", 1),
            "an unbudgeted client is let through, never locked out"
        );
    }
}
