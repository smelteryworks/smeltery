//! `GET /app/<key>`: the handshake checks, the upgrade (answered here, D-401), then one task per socket that the
//! app owns and that closes on the app's shutdown token (D-402, D-407).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use futures_util::{SinkExt as _, StreamExt as _};
use http::header::{
    CONNECTION, ORIGIN, RETRY_AFTER, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY,
    SEC_WEBSOCKET_VERSION, UPGRADE,
};
use http::request::Parts;
use http::{HeaderValue, Method, StatusCode, Version};
use hyper::upgrade::{OnUpgrade, Upgraded};
use hyper_util::rt::TokioIo;
use smeltery_core::http::ClientInfo;
use smeltery_core::{App, Response, UpgradeHold};
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_util::sync::CancellationToken;
use tungstenite::protocol::frame::coding::CloseCode as WireCode;
use tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tungstenite::{Message, Utf8Bytes};

use crate::Anvil;
use crate::hub::{Refusal, Registered};
use crate::live::JoinAnswer;
use crate::origin::Verdict;
use crate::protocol::{self, CloseCode};
use crate::session::{Action, Now, Session};

/// The read buffer per socket (tungstenite's default is 128 KiB).
const READ_BUFFER: usize = 8 * 1024;

/// Room in the write buffer above the largest frame (pongs, the close frame).
const WRITE_HEADROOM: usize = 64 * 1024;

/// The cap of a socket's write buffer: room for the largest frame it can be sent (an event, a client event, or a
/// presence member list) and [`WRITE_HEADROOM`]. A frame above the cap cannot be written at all (the socket would
/// end), so the member list is part of it (`protocol::presence_list_bound`).
pub(crate) fn write_buffer_cap(settings: &crate::Settings) -> usize {
    let event = settings
        .max_event_size
        .saturating_mul(6)
        .saturating_add(settings.max_message_size);
    let members =
        protocol::presence_list_bound(settings.max_presence_members, settings.max_member_bytes);
    event.max(members).saturating_add(WRITE_HEADROOM)
}

/// How long a close waits for the peer's close frame.
const CLOSE_WAIT: Duration = Duration::from_secs(1);

/// How long a socket whose read failed stays open after its close frame: the rest of the refused message is
/// unread, and a TCP reset could overtake the close frame.
const LINGER_AFTER_ERROR: Duration = Duration::from_millis(500);

/// `Retry-After` while shutting down, in seconds.
const BUSY_RETRY_SECS: u64 = 5;

/// The most client addresses the handshake budget tracks one by one in a minute.
const MAX_TRACKED_CLIENTS: usize = 100_000;

/// The most client networks (IPv6 /48, IPv4 /16) counted together once [`MAX_TRACKED_CLIENTS`] is reached.
const MAX_TRACKED_NETWORKS: usize = 10_000;

/// Handshakes a client network may make a minute once it is counted as a whole, as a multiple of the per-client
/// budget.
const NETWORK_FACTOR: u32 = 16;

/// One minute's counts.
#[derive(Default)]
struct Window {
    start: Option<Instant>,
    /// Per client: IPv4 address, IPv6 /64.
    clients: HashMap<Option<IpAddr>, u32>,
    /// Per network, for clients that arrive once `clients` is full.
    networks: HashMap<Option<IpAddr>, u32>,
}

/// Handshakes per client address per minute (a fixed window; an IPv6 client by its /64).
///
/// The table of clients is bounded. When it is full, a client not in it is counted with its whole network (an IPv6
/// /48, an IPv4 /16) against a larger budget, so a flood from many /64s only uses up its own networks' budgets; when
/// that table is full too, the client is let through, bounded by the socket caps (`ANVIL_MAX_CONNECTIONS`,
/// `ANVIL_MAX_CONNECTIONS_PER_IP`) and the server's. Refusing unknown clients instead would let one attacker with
/// enough addresses lock every new visitor out.
pub(crate) struct Handshakes {
    per_minute: u32,
    max_clients: usize,
    max_networks: usize,
    state: Mutex<Window>,
}

impl std::fmt::Debug for Handshakes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handshakes")
            .field("per_minute", &self.per_minute)
            .finish_non_exhaustive()
    }
}

/// The network a client is counted with when the client table is full: an IPv4 /16, an IPv6 /48.
fn network_key(ip: Option<IpAddr>) -> Option<IpAddr> {
    ip.map(|ip| match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(std::net::Ipv6Addr::from(u128::from(v6) & (u128::MAX << 80))),
        IpAddr::V4(v4) => IpAddr::V4(std::net::Ipv4Addr::from(u32::from(v4) & 0xFFFF_0000)),
    })
}

impl Handshakes {
    pub(crate) fn new(per_minute: u32) -> Self {
        Self::with_capacity(per_minute, MAX_TRACKED_CLIENTS, MAX_TRACKED_NETWORKS)
    }

    fn with_capacity(per_minute: u32, max_clients: usize, max_networks: usize) -> Self {
        Self {
            per_minute: per_minute.max(1),
            max_clients,
            max_networks,
            state: Mutex::new(Window::default()),
        }
    }

    /// Count a handshake of `ip` at `now`; `false` when the client (or its network) is over its budget.
    pub(crate) fn allow(&self, ip: Option<IpAddr>, now: Instant) -> bool {
        let key = crate::hub::client_key(ip);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let window = &mut *state;
        if window
            .start
            .is_none_or(|s| now.saturating_duration_since(s) >= Duration::from_secs(60))
        {
            window.start = Some(now);
            window.clients.clear();
            window.networks.clear();
        }
        let (map, key, budget) = if window.clients.contains_key(&key)
            || window.clients.len() < self.max_clients
        {
            (&mut window.clients, key, self.per_minute)
        } else {
            let network = network_key(ip);
            if !window.networks.contains_key(&network) && window.networks.len() >= self.max_networks
            {
                return true;
            }
            (
                &mut window.networks,
                network,
                self.per_minute.saturating_mul(NETWORK_FACTOR),
            )
        };
        let count = map.entry(key).or_insert(0);
        if *count >= budget {
            return false;
        }
        *count += 1;
        true
    }
}

/// The request's `Sec-WebSocket-Key` when it is a WebSocket upgrade over HTTP/1.1 that hyper can hand over.
fn upgrade_key(parts: &Parts) -> Option<HeaderValue> {
    let header = |name| parts.headers.get(name).map(HeaderValue::as_bytes);
    let connection_upgrade = parts
        .headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
    let ok = parts.version == Version::HTTP_11
        && parts.method == Method::GET
        && connection_upgrade
        && header(UPGRADE).is_some_and(|v| v.eq_ignore_ascii_case(b"websocket"))
        && header(SEC_WEBSOCKET_VERSION).is_some_and(|v| v == b"13")
        && parts.extensions.get::<OnUpgrade>().is_some();
    if !ok {
        return None;
    }
    parts
        .headers
        .get(SEC_WEBSOCKET_KEY)
        .filter(|key| valid_key(key.as_bytes()))
        .cloned()
}

/// RFC 6455 §4.1: 16 bytes in base64 (22 characters and `==`).
fn valid_key(key: &[u8]) -> bool {
    key.len() == 24
        && key.ends_with(b"==")
        && key
            .iter()
            .take(22)
            .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
}

pub(crate) fn plain(status: StatusCode, text: &'static str) -> Response {
    let mut response = Response::new(Body::from(text));
    *response.status_mut() = status;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// The protocol version of `?protocol=`.
fn protocol_check(parts: &Parts) -> Option<(CloseCode, &'static str)> {
    let query = parts.uri.query().unwrap_or_default();
    let version = form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == "protocol")
        .map(|(_, v)| v.into_owned());
    match version {
        None => Some((CloseCode::ProtocolMissing, "no protocol version supplied")),
        Some(v) => match v.trim().parse::<u32>() {
            Ok(n) if protocol::PROTOCOLS.contains(&n) => None,
            _ => Some((
                CloseCode::ProtocolUnsupported,
                "unsupported protocol version",
            )),
        },
    }
}

/// `GET /app/<key>`.
pub(crate) async fn socket(app: App, request: Request) -> Response {
    let Some(anvil) = Anvil::of(&app) else {
        return plain(StatusCode::NOT_FOUND, "Not Found");
    };
    if !anvil.serves_sockets() {
        // ANVIL_IN_SERVE=false: the `anvil` process holds the sockets; a client sent here is misrouted.
        return plain(StatusCode::NOT_FOUND, "Not Found");
    }
    let (mut parts, _body) = request.into_parts();
    let token = app.shutdown_token().clone();
    let config = anvil.inner.session.get().cloned();
    let (Some(config), false) = (config, token.is_cancelled()) else {
        // Shutting down, or no channel secret (no usable APP_KEY): the client retries.
        let mut response = plain(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable");
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from(BUSY_RETRY_SECS));
        return response;
    };
    let Some(key) = upgrade_key(&parts) else {
        let mut response = plain(
            StatusCode::UPGRADE_REQUIRED,
            "this endpoint accepts WebSocket upgrades only",
        );
        response
            .headers_mut()
            .insert(UPGRADE, HeaderValue::from_static("websocket"));
        return response;
    };
    let client = ClientInfo::from_parts(&parts);
    if !anvil.inner.handshakes.allow(client.ip(), Instant::now()) {
        tracing::debug!(client = ?client.ip(), "anvil: handshake refused, over the per-client budget");
        let mut response = plain(StatusCode::TOO_MANY_REQUESTS, "Too Many Requests");
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from(60u64));
        return response;
    }
    let refusal = protocol_check(&parts).or_else(|| {
        let policy = anvil.inner.policy.get()?;
        let verdict = policy.check(
            parts
                .headers
                .get_all(ORIGIN)
                .iter()
                .map(HeaderValue::as_bytes),
        );
        (verdict == Verdict::Refused).then_some((CloseCode::OriginRefused, "origin not allowed"))
    });
    let Some(on_upgrade) = parts.extensions.remove::<OnUpgrade>() else {
        return plain(StatusCode::UPGRADE_REQUIRED, "Upgrade Required");
    };
    // Every check is done and the answer is 101: the socket keeps the connection's place in the server's limits. A
    // server that offers no hold (not Smeltery's) would let the socket outlive every connection limit: refused.
    let Some(hold) = UpgradeHold::take(&mut parts.extensions) else {
        tracing::error!(
            "anvil: the server offers no connection hold for this upgrade (it is not served by smeltery's server); refused"
        );
        return plain(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable");
    };
    let settings = &anvil.inner.settings;
    let ws_config = WebSocketConfig::default()
        .read_buffer_size(READ_BUFFER)
        .write_buffer_size(0)
        .max_write_buffer_size(write_buffer_cap(settings))
        .max_message_size(Some(settings.max_message_size))
        .max_frame_size(Some(settings.max_message_size));
    let task = SocketTask {
        anvil: anvil.clone(),
        app: app.clone(),
        config,
        refusal,
        ip: client.ip(),
        write_timeout: settings.write_timeout,
    };
    app.spawn_owned(async move {
        let _hold = hold;
        let upgraded = tokio::select! {
            upgraded = on_upgrade => match upgraded {
                Ok(upgraded) => upgraded,
                Err(error) => {
                    tracing::debug!(%error, "anvil: upgrade failed");
                    return;
                }
            },
            () = token.cancelled() => return,
        };
        let socket =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(ws_config))
                .await;
        task.run(socket, token).await;
    });
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let headers = response.headers_mut();
    headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    if let Ok(accept) =
        HeaderValue::from_str(&tungstenite::handshake::derive_accept_key(key.as_bytes()))
    {
        headers.insert(SEC_WEBSOCKET_ACCEPT, accept);
    }
    response
}

type Socket = WebSocketStream<TokioIo<Upgraded>>;

/// What a socket's task starts with.
struct SocketTask {
    anvil: Anvil,
    /// To hand the presence cleanup to a task of its own when this one ends without running it (a panic).
    app: App,
    config: Arc<crate::session::Config>,
    /// A refusal decided at the handshake, sent after the upgrade (pusher-js reads close codes, not statuses).
    refusal: Option<(CloseCode, &'static str)>,
    ip: Option<IpAddr>,
    write_timeout: Duration,
}

/// Whether the socket goes on.
#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Go,
    Stop,
}

struct Conn {
    socket: Socket,
    /// The client address (for the client-event budget).
    client: Option<IpAddr>,
    write_timeout: Duration,
    closed: bool,
    read_failed: bool,
}

impl Conn {
    async fn send(&mut self, message: Message) -> bool {
        match tokio::time::timeout(self.write_timeout, self.socket.send(message)).await {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                tracing::debug!(%error, "anvil: socket write failed");
                false
            }
            Err(_) => {
                tracing::debug!("anvil: socket write timed out; dropping it");
                false
            }
        }
    }

    async fn send_text(&mut self, text: Utf8Bytes) -> bool {
        self.send(Message::Text(text)).await
    }

    /// Send the close frame (once), then wait briefly for the peer's.
    async fn close(&mut self, code: CloseCode, reason: &'static str) {
        if self.closed {
            return;
        }
        self.closed = true;
        tracing::debug!(code = code.code(), reason, "anvil: closing socket");
        let frame = CloseFrame {
            code: WireCode::from(code.code()),
            reason: Utf8Bytes::from_static(reason),
        };
        if !self.send(Message::Close(Some(frame))).await {
            return;
        }
        if self.read_failed {
            tokio::time::sleep(LINGER_AFTER_ERROR).await;
        } else {
            let socket = &mut self.socket;
            let _ = tokio::time::timeout(CLOSE_WAIT, async {
                while let Some(Ok(_)) = socket.next().await {}
            })
            .await;
        }
    }

    async fn apply(
        &mut self,
        actions: Vec<Action>,
        registered: Option<&Registered>,
        anvil: &Anvil,
        session: &mut Session,
    ) -> Flow {
        for action in actions {
            match action {
                Action::Send(text) => {
                    if !self.send_text(Utf8Bytes::from(text)).await {
                        return Flow::Stop;
                    }
                }
                Action::Close(code, reason) => {
                    self.close(code, reason).await;
                    return Flow::Stop;
                }
                Action::Subscribe(channel, holder) => {
                    if let Some(r) = registered
                        && !r.registration.join(&channel, holder)
                    {
                        self.close(CloseCode::Reconnect, "authorization revoked")
                            .await;
                        return Flow::Stop;
                    }
                }
                Action::Unsubscribe(channel) => {
                    if let Some(r) = registered {
                        r.registration.leave(&channel);
                    }
                }
                Action::JoinPresence {
                    channel,
                    member,
                    holder,
                } => {
                    let Some(r) = registered else { continue };
                    // The hub first, so no member event after the store's list is missed.
                    if !r.registration.join(&channel, holder) {
                        self.close(CloseCode::Reconnect, "authorization revoked")
                            .await;
                        return Flow::Stop;
                    }
                    let answer = anvil
                        .presence_join(session.socket_id(), &channel, &member)
                        .await;
                    let frame = match answer {
                        Ok(JoinAnswer::In(frame)) => frame,
                        Ok(JoinAnswer::Full) => {
                            r.registration.leave(&channel);
                            session.forget(&channel);
                            protocol::subscription_error(&channel, 403, "presence channel full")
                        }
                        Err(error) => {
                            tracing::error!(%error, "anvil: a presence join failed");
                            r.registration.leave(&channel);
                            session.forget(&channel);
                            protocol::subscription_error(&channel, 500, "server error")
                        }
                    };
                    if !self.send_text(Utf8Bytes::from(frame)).await {
                        return Flow::Stop;
                    }
                }
                Action::ListPresence(channel) => {
                    let frame = match anvil.presence_list(&channel).await {
                        Ok(frame) => frame,
                        Err(error) => {
                            tracing::error!(%error, "anvil: a presence member list failed");
                            protocol::subscription_error(&channel, 500, "server error")
                        }
                    };
                    if !self.send_text(Utf8Bytes::from(frame)).await {
                        return Flow::Stop;
                    }
                }
                Action::LeavePresence { channel, user_id } => {
                    anvil
                        .presence_leave(session.socket_id(), &channel, &user_id)
                        .await;
                }
                Action::Whisper {
                    channel,
                    event,
                    data,
                    user_id,
                } => {
                    let sent = anvil.whisper(
                        session.socket_id(),
                        self.client,
                        &channel,
                        &event,
                        &data,
                        user_id.as_deref(),
                    );
                    if let Err(message) = sent
                        && !self
                            .send_text(Utf8Bytes::from(protocol::error(4301, message)))
                            .await
                    {
                        return Flow::Stop;
                    }
                }
            }
        }
        Flow::Go
    }
}

/// Leaves the socket's presence channels from a task of its own when the socket task ends without doing it itself.
struct LeaveOnDrop {
    anvil: Anvil,
    app: App,
    /// `None` once the socket task ran the cleanup.
    socket: Option<String>,
}

impl Drop for LeaveOnDrop {
    fn drop(&mut self) {
        let Some(socket) = self.socket.take() else {
            return;
        };
        if self.anvil.memberships_of(&socket).is_empty() {
            return;
        }
        let anvil = self.anvil.clone();
        self.app.spawn_owned(async move {
            anvil.presence_leave_socket(&socket).await;
        });
    }
}

impl SocketTask {
    async fn run(self, socket: Socket, token: CancellationToken) {
        let mut conn = Conn {
            socket,
            client: self.ip,
            write_timeout: self.write_timeout,
            closed: false,
            read_failed: false,
        };
        if let Some((code, reason)) = self.refusal {
            tracing::debug!(code = code.code(), "anvil: socket refused at the handshake");
            let _ = conn
                .send_text(Utf8Bytes::from(protocol::error(code.code(), reason)))
                .await;
            conn.close(code, reason).await;
            return;
        }
        let Ok(socket_id) = protocol::new_socket_id() else {
            conn.close(CloseCode::OverCapacity, "server error").await;
            return;
        };
        let mut registered = match self.anvil.inner.hub.register(&socket_id, self.ip) {
            Ok(registered) => registered,
            Err(refusal) => {
                let reason = match refusal {
                    Refusal::Full => "over capacity",
                    Refusal::PerClient => "too many connections from this address",
                };
                tracing::debug!(?refusal, "anvil: socket refused");
                let _ = conn
                    .send_text(Utf8Bytes::from(protocol::error(
                        CloseCode::OverCapacity.code(),
                        reason,
                    )))
                    .await;
                conn.close(CloseCode::OverCapacity, reason).await;
                return;
            }
        };
        // However this task ends, even by a panic, the socket leaves its presence channels.
        let mut leave = LeaveOnDrop {
            anvil: self.anvil.clone(),
            app: self.app.clone(),
            socket: Some(socket_id.clone()),
        };
        let mut session = Session::new(Arc::clone(&self.config), socket_id, Instant::now());
        if conn
            .apply(session.open(), Some(&registered), &self.anvil, &mut session)
            .await
            == Flow::Stop
        {
            return;
        }
        loop {
            let deadline = session.next_deadline();
            // Biased: shutdown and a close the hub asked for (a revocation) win over everything else, so a revoked
            // socket is sent nothing more; the client's frames and the timers come before queued events, so a busy
            // channel never delays a pong or a liveness check.
            let flow = tokio::select! {
                biased;
                () = token.cancelled() => {
                    let actions = session.on_shutdown();
                    conn.apply(actions, Some(&registered), &self.anvil, &mut session).await;
                    Flow::Stop
                }
                changed = registered.close.changed() => {
                    let code = *registered.close.borrow_and_update();
                    match (changed, code) {
                        (Ok(()), Some(code)) => {
                            conn.close(code, code.hub_reason()).await;
                            Flow::Stop
                        }
                        (Ok(()), None) => Flow::Go,
                        (Err(_), _) => Flow::Stop,
                    }
                }
                message = conn.socket.next() => match message {
                    None => Flow::Stop,
                    Some(Err(tungstenite::Error::Capacity(_))) => {
                        conn.read_failed = true;
                        conn.close(CloseCode::TooBig, "message too big").await;
                        Flow::Stop
                    }
                    Some(Err(error)) => {
                        tracing::debug!(%error, "anvil: socket read failed");
                        Flow::Stop
                    }
                    Some(Ok(message)) => {
                        let actions = match message {
                            Message::Text(text) => session.on_text(text.as_str(), Now::real()),
                            Message::Binary(_) => session.on_binary(),
                            Message::Ping(_) | Message::Pong(_) => session.on_control(Instant::now()),
                            // tungstenite answers the close; the next read ends the stream.
                            Message::Close(_) | Message::Frame(_) => Vec::new(),
                        };
                        conn.apply(actions, Some(&registered), &self.anvil, &mut session).await
                    }
                },
                () = tokio::time::sleep_until(deadline) => {
                    let actions = session.on_tick(Instant::now());
                    conn.apply(actions, Some(&registered), &self.anvil, &mut session).await
                }
                frame = registered.outbox.recv() => match frame {
                    Some(frame) => if conn.send_text(frame).await { Flow::Go } else { Flow::Stop },
                    None => Flow::Stop,
                },
            };
            if flow == Flow::Stop {
                break;
            }
        }
        // However the socket ended (a close, a dropped connection, a revocation, shutdown): it leaves its presence
        // channels, so the other members see it go. The hub forgets it when `registered` drops.
        drop(registered);
        leave.socket = None;
        self.anvil.presence_leave_socket(session.socket_id()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_socket_task_that_ends_without_its_cleanup_still_leaves_its_channels() {
        use crate::AnvilExt as _;
        let app = smeltery_core::AppBuilder::new(smeltery_core::config::Settings::from_env())
            .anvil(|c| {
                c.presence("room.{r}", |_| async { Ok(None) });
            })
            .build()
            .await
            .unwrap()
            .app;
        let anvil = Anvil::of(&app).unwrap();
        let member = crate::presence::Member::new(7);
        anvil
            .presence_join("1.1", "presence-room.1", &member)
            .await
            .unwrap();
        assert_eq!(
            anvil.members("presence-room.1").await.unwrap(),
            vec![member]
        );
        // The task panicked (or ended) before its cleanup: the guard drops with the socket still in.
        drop(LeaveOnDrop {
            anvil: anvil.clone(),
            app: app.clone(),
            socket: Some("1.1".into()),
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !anvil.memberships_of("1.1").is_empty()
                || !anvil.members("presence-room.1").await.unwrap().is_empty()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the guard's task left the channel");
    }

    /// Sweep W5-03: the write-buffer cap fits the largest member list the settings allow (a frame above the cap
    /// cannot be written: the joining socket ended without an answer).
    #[test]
    fn the_write_buffer_fits_the_largest_member_list() {
        let core = smeltery_core::config::Settings::from_env();
        let mut settings = crate::Settings::from_env(&core);
        for (members, bytes) in [(100, 1024), (1_000, 1024), (2_000, 2048)] {
            settings.max_presence_members = members;
            settings.max_member_bytes = bytes;
            assert!(
                write_buffer_cap(&settings) > protocol::presence_list_bound(members, bytes),
                "{members} members of {bytes} bytes"
            );
        }
    }

    /// Sweep W1-06: an upgrade the server offers no connection hold for (not Smeltery's server) is refused, never
    /// served outside the connection limits.
    #[tokio::test]
    async fn an_upgrade_without_a_hold_is_refused() {
        use crate::AnvilExt as _;
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.key = "anvil-endpoint-tests-key-0123456789abcdef".into();
        let app = smeltery_core::AppBuilder::new(settings)
            .anvil(|c| {
                c.public("news");
            })
            .build()
            .await
            .unwrap()
            .app;
        let key = Anvil::of(&app).unwrap().app_key().to_owned();
        let mut request = http::Request::builder()
            .method(Method::GET)
            .uri(format!("/app/{key}?protocol=7"))
            .header(CONNECTION, "Upgrade")
            .header(UPGRADE, "websocket")
            .header(SEC_WEBSOCKET_VERSION, "13")
            .header(SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Body::empty())
            .unwrap();
        // hyper's upgrade handle without the server's hold (what another server would hand over).
        let mut bare = http::Request::new(());
        let on_upgrade = hyper::upgrade::on(&mut bare);
        request.extensions_mut().insert(on_upgrade);
        let response = socket(app, request).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn websocket_keys_are_checked() {
        assert!(valid_key(b"dGhlIHNhbXBsZSBub25jZQ=="));
        assert!(!valid_key(b"dGhlIHNhbXBsZSBub25jZQ="));
        assert!(!valid_key(b"dGhlIHNhbXBsZSBub25jZ!=="));
    }

    #[test]
    fn handshakes_are_budgeted_per_client_and_minute() {
        let budget = Handshakes::new(2);
        let now = Instant::now();
        let a: IpAddr = "203.0.113.1".parse().unwrap();
        let b: IpAddr = "203.0.113.2".parse().unwrap();
        assert!(budget.allow(Some(a), now));
        assert!(budget.allow(Some(a), now));
        assert!(!budget.allow(Some(a), now));
        assert!(
            budget.allow(Some(b), now),
            "another client has its own budget"
        );
        assert!(
            budget.allow(Some(a), now + Duration::from_secs(60)),
            "a new minute"
        );
    }

    #[test]
    fn a_full_client_table_never_locks_new_clients_out() {
        let budget = Handshakes::with_capacity(2, 100, 2);
        let now = Instant::now();
        // An attacker fills the client table with 100 /64s of one /48.
        for n in 0..100u128 {
            let ip = IpAddr::V6(std::net::Ipv6Addr::from(
                (0x2001_0db8_0001_u128 << 80) | (n << 64),
            ));
            assert!(budget.allow(Some(ip), now));
        }
        // Further /64s of that /48 share one network budget (2 × 16) ...
        let mut allowed = 0;
        for n in 100..200u128 {
            let ip = IpAddr::V6(std::net::Ipv6Addr::from(
                (0x2001_0db8_0001_u128 << 80) | (n << 64),
            ));
            allowed += usize::from(budget.allow(Some(ip), now));
        }
        assert_eq!(allowed, 32);
        // ... while a visitor from elsewhere still gets in.
        let visitor: IpAddr = "198.51.100.7".parse().unwrap();
        assert!(budget.allow(Some(visitor), now));
        // With the network table full as well, a new client is let through (bounded by the socket caps).
        let other: IpAddr = "203.0.113.9".parse().unwrap();
        assert!(budget.allow(Some(other), now));
    }

    #[test]
    fn the_protocol_version_is_checked() {
        let parts = |uri: &str| {
            http::Request::builder()
                .uri(uri)
                .body(())
                .unwrap()
                .into_parts()
                .0
        };
        assert_eq!(protocol_check(&parts("/app/k?protocol=7&client=js")), None);
        assert_eq!(protocol_check(&parts("/app/k?protocol=5")), None);
        assert_eq!(
            protocol_check(&parts("/app/k?protocol=4")).map(|r| r.0),
            Some(CloseCode::ProtocolUnsupported)
        );
        assert_eq!(
            protocol_check(&parts("/app/k?client=js")).map(|r| r.0),
            Some(CloseCode::ProtocolMissing)
        );
    }
}
