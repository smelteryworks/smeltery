//! One socket's protocol state, without I/O: frames in, actions out, time passed in (ANVIL.md §6.1). The socket
//! task (`endpoint.rs`) and the test socket (`testing.rs`) drive it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use crate::channels::Channels;
use crate::presence::Member;
use crate::protocol::{self, ClientFrame, CloseCode, Kind, SubscribeData};
use crate::signature::{self, Grant, Refused};

/// What a session needs to know, shared by every socket of the process.
pub(crate) struct Config {
    pub(crate) app_key: String,
    pub(crate) secret: String,
    pub(crate) channels: Arc<Channels>,
    pub(crate) activity_timeout: Duration,
    pub(crate) ping_interval: Duration,
    pub(crate) pong_timeout: Duration,
    pub(crate) max_age: Duration,
    pub(crate) max_subscriptions: usize,
    /// The most presence channels one socket is in.
    pub(crate) max_presence_channels: usize,
    pub(crate) frames_per_second: u32,
    pub(crate) frame_burst: u32,
    /// Recent revocations: a grant made before one cannot subscribe.
    pub(crate) revocations: Arc<crate::revocation::Revocations>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("app_key", &self.app_key)
            .finish_non_exhaustive()
    }
}

/// The time now: monotonic for the timers, Unix seconds for grant expiry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Now {
    pub(crate) at: Instant,
    pub(crate) unix: u64,
}

impl Now {
    /// The real time.
    pub(crate) fn real() -> Self {
        Self {
            at: Instant::now(),
            unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }
}

/// What the socket task must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Send this text frame.
    Send(String),
    /// Send the close frame and end the socket.
    Close(CloseCode, &'static str),
    /// Deliver this channel's events to the socket from now on; who authorized it (private channels), for
    /// revocation. When the hub refuses the join (a revocation recorded since the check), the socket closes with
    /// 4200 instead of the actions that follow.
    Subscribe(String, Option<crate::revocation::Holder>),
    /// Stop delivering this channel's events.
    Unsubscribe(String),
    /// Join the presence channel as `member` (after the hub join, as [`Subscribe`](Self::Subscribe)): the socket
    /// task answers `subscription_succeeded` with the members, or `subscription_error` (then it calls
    /// [`Session::forget`]).
    JoinPresence {
        channel: String,
        member: Member,
        holder: Option<crate::revocation::Holder>,
    },
    /// Answer `subscription_succeeded` with the members of a presence channel this socket is in already.
    ListPresence(String),
    /// Leave the presence channel (after [`Unsubscribe`](Self::Unsubscribe)): `member_removed` when it was the
    /// user's last socket there.
    LeavePresence { channel: String, user_id: String },
    /// Send a client event to the channel's other subscribers (in every process), never back to this socket.
    Whisper {
        channel: String,
        event: String,
        data: serde_json::Value,
        user_id: Option<String>,
    },
}

/// The longest client event name.
const MAX_CLIENT_EVENT_NAME: usize = 200;

/// Client events one socket may send a second (Pusher's limit), and at once.
const CLIENT_EVENTS_PER_SECOND: u32 = 10;

/// Presence joins one socket may make a second on average, and at once: every join and leave costs store writes and
/// an announcement to every member, so a socket cannot churn a channel.
const PRESENCE_JOINS_PER_SECOND: u32 = 1;
const PRESENCE_JOIN_BURST: u32 = 5;

/// One subscription: the grant (private and presence channels) and the member (presence channels).
#[derive(Debug)]
struct Sub {
    grant: Option<Grant>,
    member: Option<Member>,
}

/// A token bucket for one socket's incoming frames (no map: one per socket).
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    burst: f64,
    per_sec: f64,
    last: Instant,
}

impl Bucket {
    fn new(per_sec: u32, burst: u32, now: Instant) -> Self {
        let burst = f64::from(burst.max(1));
        Self {
            tokens: burst,
            burst,
            per_sec: f64::from(per_sec.max(1)),
            last: now,
        }
    }

    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_sec).min(self.burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// `at + wait`, or a year after `at` when that would overflow (the settings bound the waits; this keeps a socket
/// task from panicking whatever a caller put in).
fn later(at: Instant, wait: Duration) -> Instant {
    at.checked_add(wait)
        .or_else(|| at.checked_add(Duration::from_secs(365 * 86_400)))
        .unwrap_or(at)
}

/// One socket's protocol state.
#[derive(Debug)]
pub(crate) struct Session {
    config: Arc<Config>,
    socket_id: String,
    connected_at: Instant,
    last_seen: Instant,
    pinged_at: Option<Instant>,
    bucket: Bucket,
    refused: u32,
    /// Client events (a bucket of their own, inside the frame budget).
    whispers: Bucket,
    /// Presence joins (a bucket of their own).
    joins: Bucket,
    /// Subscribed channels, with who subscribed to each private or presence one.
    subscriptions: BTreeMap<String, Sub>,
    closed: bool,
}

impl Session {
    pub(crate) fn new(config: Arc<Config>, socket_id: String, now: Instant) -> Self {
        let bucket = Bucket::new(config.frames_per_second, config.frame_burst, now);
        Self {
            config,
            socket_id,
            connected_at: now,
            last_seen: now,
            pinged_at: None,
            bucket,
            refused: 0,
            whispers: Bucket::new(CLIENT_EVENTS_PER_SECOND, CLIENT_EVENTS_PER_SECOND, now),
            joins: Bucket::new(PRESENCE_JOINS_PER_SECOND, PRESENCE_JOIN_BURST, now),
            subscriptions: BTreeMap::new(),
            closed: false,
        }
    }

    pub(crate) fn socket_id(&self) -> &str {
        &self.socket_id
    }

    /// Forget a subscription the socket task could not complete (a presence join that failed).
    pub(crate) fn forget(&mut self, channel: &str) {
        self.subscriptions.remove(channel);
    }

    /// The first frame: `pusher:connection_established`.
    pub(crate) fn open(&self) -> Vec<Action> {
        vec![Action::Send(protocol::connection_established(
            &self.socket_id,
            self.config.activity_timeout.as_secs(),
        ))]
    }

    fn close(&mut self, code: CloseCode, reason: &'static str) -> Vec<Action> {
        self.closed = true;
        vec![Action::Close(code, reason)]
    }

    /// Count an incoming frame against the bucket; `Err` holds what to do when it is refused.
    fn charge(&mut self, now: Instant) -> Result<(), Vec<Action>> {
        if self.bucket.take(now) {
            self.refused = 0;
            return Ok(());
        }
        self.refused = self.refused.saturating_add(1);
        if self.refused > self.config.frame_burst.max(1) {
            return Err(self.close(CloseCode::OverCapacity, "too many messages"));
        }
        if self.refused == 1 {
            // Once per streak of refusals: the client learns why its messages are dropped.
            return Err(vec![Action::Send(protocol::error(
                4301,
                "too many messages; slow down",
            ))]);
        }
        Err(Vec::new())
    }

    fn touch(&mut self, now: Instant) {
        self.last_seen = now;
        self.pinged_at = None;
    }

    /// A WebSocket ping or pong frame (tungstenite answers pings): it proves the peer alive and counts against the
    /// frame budget.
    pub(crate) fn on_control(&mut self, now: Instant) -> Vec<Action> {
        if self.closed {
            return Vec::new();
        }
        if let Err(actions) = self.charge(now) {
            return actions;
        }
        self.touch(now);
        Vec::new()
    }

    /// A binary frame: not part of the protocol.
    pub(crate) fn on_binary(&mut self) -> Vec<Action> {
        if self.closed {
            return Vec::new();
        }
        self.close(CloseCode::Unsupported, "binary frames are not supported")
    }

    /// A text frame.
    pub(crate) fn on_text(&mut self, text: &str, now: Now) -> Vec<Action> {
        if self.closed {
            return Vec::new();
        }
        if let Err(actions) = self.charge(now.at) {
            return actions;
        }
        self.touch(now.at);
        let Ok(frame) = serde_json::from_str::<ClientFrame>(text) else {
            // Not a protocol frame: dropped (it counted against the budget).
            return Vec::new();
        };
        match frame.event.as_str() {
            "pusher:ping" => vec![Action::Send(protocol::pong())],
            "pusher:pong" => Vec::new(),
            "pusher:subscribe" => match SubscribeData::from_value(frame.data) {
                Some(data) => self.subscribe(data, now),
                None => Vec::new(),
            },
            "pusher:unsubscribe" => match SubscribeData::from_value(frame.data) {
                Some(data) => match self.subscriptions.remove(&data.channel) {
                    Some(Sub {
                        member: Some(member),
                        ..
                    }) => vec![
                        Action::Unsubscribe(data.channel.clone()),
                        Action::LeavePresence {
                            channel: data.channel,
                            user_id: member.user_id().to_owned(),
                        },
                    ],
                    Some(_) => vec![Action::Unsubscribe(data.channel)],
                    None => Vec::new(),
                },
                None => Vec::new(),
            },
            "pusher:signin" => vec![Action::Send(protocol::error(
                4009,
                "user authentication is not supported",
            ))],
            event if event.starts_with("client-") => {
                let event = event.to_owned();
                self.client_event(event, frame.channel, frame.data, now.at)
            }
            _ => Vec::new(),
        }
    }

    /// A client event: allowed on a private or presence channel this socket is in, whose pattern opted in.
    fn client_event(
        &mut self,
        event: String,
        channel: Option<String>,
        data: serde_json::Value,
        now: Instant,
    ) -> Vec<Action> {
        let not_enabled = || {
            vec![Action::Send(protocol::error(
                4009,
                "client events are not enabled on this channel",
            ))]
        };
        let Some(channel) = channel else {
            return not_enabled();
        };
        let Some(sub) = self.subscriptions.get(&channel) else {
            return not_enabled();
        };
        if sub.grant.is_none() || !self.config.channels.whispers(&channel) {
            return not_enabled();
        }
        if event.len() > MAX_CLIENT_EVENT_NAME || event.chars().any(char::is_control) {
            return vec![Action::Send(protocol::error(
                4009,
                "invalid client event name",
            ))];
        }
        if !self.whispers.take(now) {
            return vec![Action::Send(protocol::error(
                4301,
                "client event rate limit reached",
            ))];
        }
        let user_id = sub.member.as_ref().map(|m| m.user_id().to_owned());
        vec![Action::Whisper {
            channel,
            event,
            data,
            user_id,
        }]
    }

    fn subscribe(&mut self, data: SubscribeData, now: Now) -> Vec<Action> {
        let channel = data.channel;
        let refuse = |status: u16, error: &str| {
            vec![Action::Send(protocol::subscription_error(
                &channel, status, error,
            ))]
        };
        if !protocol::valid_channel(&channel) {
            return refuse(400, "invalid channel name");
        }
        if let Some(sub) = self.subscriptions.get(&channel) {
            // pusher-js subscribes again after a reconnect of its own; the answer is the same. On a presence channel
            // the answer reads the member list from the store, so it costs a join from the same budget.
            if sub.member.is_some() {
                if !self.joins.take(now.at) {
                    return refuse(429, "too many presence joins; slow down");
                }
                return vec![Action::ListPresence(channel)];
            }
            return vec![Action::Send(protocol::subscription_succeeded(&channel))];
        }
        if self.subscriptions.len() >= self.config.max_subscriptions {
            return refuse(429, "too many subscriptions on this connection");
        }
        let (kind, bare) = Kind::of(&channel);
        let mut member = None;
        let grant = match kind {
            Kind::Unsupported => return refuse(400, "this channel type is not supported"),
            Kind::Public => {
                if !self.config.channels.is_public(bare) {
                    return refuse(403, "subscription refused");
                }
                None
            }
            Kind::Private | Kind::Presence => {
                let Some(auth) = data.auth.as_deref() else {
                    return refuse(401, "authorization required");
                };
                let channel_data = if kind == Kind::Presence {
                    let presence = self
                        .subscriptions
                        .values()
                        .filter(|s| s.member.is_some())
                        .count();
                    if presence >= self.config.max_presence_channels {
                        return refuse(429, "too many presence channels on this connection");
                    }
                    if !self.joins.take(now.at) {
                        return refuse(429, "too many presence joins; slow down");
                    }
                    let Some(text) = data.channel_data.as_deref() else {
                        return refuse(401, "channel_data required");
                    };
                    let Some(parsed) = Member::from_channel_data(text) else {
                        return refuse(400, "invalid channel_data");
                    };
                    member = Some(parsed);
                    Some(text)
                } else {
                    None
                };
                match signature::verify(
                    &self.config.app_key,
                    &self.config.secret,
                    &self.socket_id,
                    &channel,
                    auth,
                    channel_data,
                    now.unix,
                ) {
                    Ok(grant) if self.config.revocations.refuses(&grant) => {
                        return refuse(401, "authorization revoked");
                    }
                    Ok(grant) => Some(grant),
                    Err(Refused::Expired) => return refuse(401, "authorization expired"),
                    Err(Refused::Invalid) => return refuse(401, "invalid signature"),
                }
            }
        };
        let holder = grant.as_ref().and_then(crate::revocation::Holder::of);
        self.subscriptions.insert(
            channel.clone(),
            Sub {
                grant,
                member: member.clone(),
            },
        );
        match member {
            Some(member) => vec![Action::JoinPresence {
                channel,
                member,
                holder,
            }],
            None => vec![
                Action::Subscribe(channel.clone(), holder),
                Action::Send(protocol::subscription_succeeded(&channel)),
            ],
        }
    }

    /// When [`on_tick`](Self::on_tick) has something to do next.
    pub(crate) fn next_deadline(&self) -> Instant {
        let age = later(self.connected_at, self.config.max_age);
        let liveness = match self.pinged_at {
            Some(pinged) => later(pinged, self.config.pong_timeout),
            None => later(self.last_seen, self.config.ping_interval),
        };
        age.min(liveness)
    }

    /// Time passed: ping a silent socket, close an unanswered or too old one.
    pub(crate) fn on_tick(&mut self, now: Instant) -> Vec<Action> {
        if self.closed {
            return Vec::new();
        }
        if now.saturating_duration_since(self.connected_at) >= self.config.max_age {
            return self.close(CloseCode::Reconnect, "connection too old; reconnect");
        }
        match self.pinged_at {
            Some(pinged) if now.saturating_duration_since(pinged) >= self.config.pong_timeout => {
                self.close(CloseCode::PongMissing, "pong reply not received")
            }
            None if now.saturating_duration_since(self.last_seen) >= self.config.ping_interval => {
                self.pinged_at = Some(now);
                vec![Action::Send(protocol::ping())]
            }
            _ => Vec::new(),
        }
    }

    /// The server is shutting down.
    pub(crate) fn on_shutdown(&mut self) -> Vec<Action> {
        if self.closed {
            return Vec::new();
        }
        self.close(CloseCode::GoingAway, "server shutting down")
    }
}
