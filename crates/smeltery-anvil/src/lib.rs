#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod auth_route;
mod channels;
mod endpoint;
mod event;
mod hub;
mod listeners;
mod live;
mod origin;
mod presence;
mod process;
mod protocol;
mod revocation;
mod session;
mod settings;
mod signature;
pub mod testing;
#[cfg(test)]
mod transcripts;

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde::Serialize;
use smeltery_core::pubsub::{PubSub, RecvError, Subscription};
use smeltery_core::{App, AppBuilder, Error, Result};
use tungstenite::Utf8Bytes;

pub use channels::{ChannelCtx, Channels, PrivateChannel};
pub use event::{BroadcastEvent, Channel, SocketId};
pub use presence::{Member, migrations as presence_migrations};
pub use protocol::{CloseCode, valid_channel, valid_socket_id};
pub use settings::Settings;
/// `#[derive(BroadcastEvent)]`: implements [`BroadcastEvent`] from `#[broadcast(...)]` (see the trait).
pub use smeltery_macros::BroadcastEvent;

/// The PubSub topic events travel on between the app's processes.
const TOPIC: &str = "anvil";

/// The signature purpose of the derived channel secret (`ANVIL_APP_SECRET` unset).
const SECRET_PURPOSE: &str = "anvil.secret";

/// The items a `routes/channels.rs` and event files use: `use smeltery::anvil::prelude::*;`.
pub mod prelude {
    pub use crate::{
        Anvil, AnvilExt as _, BroadcastEvent, Channel, ChannelCtx, Channels, SocketId,
    };
}

/// The shared state of an app's Anvil.
pub(crate) struct Inner {
    pub(crate) settings: Settings,
    pub(crate) channels: Arc<Channels>,
    pub(crate) hub: Arc<hub::Hub>,
    /// Set at boot, once the secret is known.
    pub(crate) session: OnceLock<Arc<session::Config>>,
    pub(crate) policy: OnceLock<origin::Policy>,
    pub(crate) handshakes: endpoint::Handshakes,
    pubsub: OnceLock<PubSub>,
    spy: Mutex<Option<Vec<testing::Sent>>>,
    /// Recent revocations (core's auth events).
    pub(crate) revocations: Arc<revocation::Revocations>,
    /// This process's id on its presence rows (random).
    pub(crate) process: String,
    /// The presence store of a process whose PubSub driver is `local`.
    pub(crate) memory: Arc<presence::memory::MemoryStore>,
    /// The presence store, once the PubSub driver is known.
    pub(crate) store: OnceLock<Arc<dyn presence::Store>>,
    /// What a shared presence store is built from.
    pub(crate) env: OnceLock<live::StoreEnv>,
    /// The presence memberships of this process's sockets: (channel, socket) → member.
    pub(crate) memberships: presence::Memberships,
    /// Client events per channel this second (`live::CHANNEL_CLIENT_EVENTS_PER_SECOND`).
    pub(crate) whisper_budget: Mutex<live::WhisperBudgets>,
    /// The events this process delivers, for core's channel seam (Sparks listeners).
    pub(crate) listeners: smeltery_core::channels::ChannelEventSender,
    /// This process is `smeltery anvil`: it serves the socket endpoint whatever `ANVIL_IN_SERVE` says.
    pub(crate) socket_process: std::sync::atomic::AtomicBool,
}

/// The app's real-time hub: sends events to the sockets subscribed to their channels, in this process and (through
/// the app's [PubSub](smeltery_core::pubsub)) in its other processes. A cheap clone; a handler takes it as an
/// argument (`anvil: Anvil`), jobs and agents get it with [`Anvil::of`].
#[derive(Clone)]
pub struct Anvil {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Anvil {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Anvil")
            .field("app_key", &self.inner.settings.app_key)
            .field("connections", &self.connections())
            .finish_non_exhaustive()
    }
}

/// What a send reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Delivered {
    /// Sockets of this process the event was queued for (other processes are not counted).
    pub local: usize,
}

/// An event ready to send: await it (`anvil.send(&event).await?`), optionally after
/// [`except`](Self::except).
#[must_use = "an event is sent only when awaited"]
pub struct PendingEvent {
    anvil: Anvil,
    prepared: Result<Prepared>,
    except: Option<SocketId>,
}

impl std::fmt::Debug for PendingEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingEvent")
            .field("except", &self.except)
            .finish_non_exhaustive()
    }
}

/// `channels` without repeats, in their first order (one event reaches a socket once per channel).
fn distinct(channels: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for channel in channels {
        if !out.contains(&channel) {
            out.push(channel);
        }
    }
    out
}

/// The most sends [`testing::AnvilSpy`] keeps (the oldest are dropped).
const MAX_RECORDED: usize = 10_000;

/// An event's channels, name and data (its JSON as text).
#[derive(Debug, Clone)]
struct Prepared {
    channels: Vec<String>,
    name: String,
    data: String,
}

impl PendingEvent {
    /// Leave out the socket that made this request (its `X-Socket-ID`), so the client that caused the event does
    /// not receive it again. `None` leaves nobody out.
    pub fn except(mut self, socket: impl Into<Option<SocketId>>) -> Self {
        self.except = socket.into();
        self
    }
}

impl IntoFuture for PendingEvent {
    type Output = Result<Delivered>;
    type IntoFuture = Pin<Box<dyn Future<Output = Result<Delivered>> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let prepared = self.prepared?;
            self.anvil.publish(prepared, self.except).await
        })
    }
}

/// Events to explicit channels without an event type ([`Anvil::to`]).
#[derive(Debug)]
#[must_use = "name the event with `.event(...)`"]
pub struct To {
    anvil: Anvil,
    channels: Vec<Channel>,
}

impl To {
    /// One more channel.
    pub fn to(mut self, channel: Channel) -> Self {
        self.channels.push(channel);
        self
    }

    /// The event name (clients listen for exactly this name, with a leading dot in laravel-echo).
    pub fn event(self, name: impl Into<String>) -> Named {
        Named {
            to: self,
            name: name.into(),
        }
    }
}

/// A named event to explicit channels; [`with`](Self::with) gives its data.
#[derive(Debug)]
#[must_use = "give the data with `.with(...)` and await it"]
pub struct Named {
    to: To,
    name: String,
}

impl Named {
    /// The event's data (anything that serializes; an object is the usual shape).
    pub fn with(self, data: &impl Serialize) -> PendingEvent {
        let prepared = serde_json::to_string(data)
            .map_err(Error::from)
            .map(|data| Prepared {
                channels: distinct(self.to.channels.iter().map(|c| c.name().to_owned())),
                name: self.name,
                data,
            });
        PendingEvent {
            anvil: self.to.anvil,
            prepared,
            except: None,
        }
    }
}

/// The message between processes.
#[derive(Debug, Serialize, serde::Deserialize)]
pub(crate) struct Wire {
    /// The event name.
    pub(crate) e: String,
    /// The channels.
    pub(crate) c: Vec<String>,
    /// The data (JSON text).
    pub(crate) d: String,
    /// The socket left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) x: Option<String>,
    /// What it is: absent for an app's event, `m` a presence member event, `w` a client event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) k: Option<String>,
    /// A client event's sender on a presence channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) u: Option<String>,
}

/// The longest event name.
const MAX_EVENT_NAME: usize = 200;

/// The most channels one event goes to.
const MAX_CHANNELS: usize = 100;

impl Anvil {
    fn new(settings: Settings, channels: Channels) -> Self {
        // A process that just started has not seen the auth events before it: grants made earlier are refused.
        let revocations = Arc::new(revocation::Revocations::starting_at(unix_now()));
        let hub = hub::Hub::new(
            settings.max_connections,
            settings.max_connections_per_ip,
            settings.outbox,
            Arc::clone(&revocations),
        );
        let handshakes = endpoint::Handshakes::new(settings.handshakes_per_minute);
        Self {
            inner: Arc::new(Inner {
                settings,
                channels: Arc::new(channels),
                hub: Arc::new(hub),
                session: OnceLock::new(),
                policy: OnceLock::new(),
                handshakes,
                pubsub: OnceLock::new(),
                spy: Mutex::new(None),
                revocations,
                process: protocol::new_socket_id().unwrap_or_else(|_| format!("{}", unix_now())),
                memory: Arc::default(),
                store: OnceLock::new(),
                env: OnceLock::new(),
                memberships: presence::Memberships::default(),
                whisper_budget: Mutex::default(),
                listeners: smeltery_core::channels::ChannelEventSender::new(),
                socket_process: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// The app's Anvil, when `.anvil(...)` installed it.
    pub fn of(app: &App) -> Option<Self> {
        app.service::<Self>().map(|a| (*a).clone())
    }

    /// The settings.
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// The public app key (the socket path is `/app/<key>`).
    pub fn app_key(&self) -> &str {
        &self.inner.settings.app_key
    }

    /// Whether this process serves the socket endpoint: `serve` unless `ANVIL_IN_SERVE=false`, and the `anvil`
    /// process always.
    pub fn serves_sockets(&self) -> bool {
        self.inner.settings.in_serve
            || self
                .inner
                .socket_process
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Open sockets in this process.
    pub fn connections(&self) -> usize {
        self.inner.hub.connections()
    }

    /// Sockets of this process subscribed to `channel` (the full name, `private-orders.7`).
    pub fn subscribers(&self, channel: &str) -> usize {
        self.inner.hub.subscribers(channel)
    }

    /// Send `event` to its channels.
    ///
    /// ```no_run
    /// # use serde::Serialize;
    /// # use smeltery::anvil::{Anvil, BroadcastEvent, SocketId};
    /// # #[derive(Serialize, BroadcastEvent)]
    /// # #[broadcast(private = "orders.{order_id}")]
    /// # struct OrderShipped { order_id: i64 }
    /// async fn ship(anvil: Anvil, socket: Option<SocketId>) -> smeltery::Result<&'static str> {
    ///     anvil.send(&OrderShipped { order_id: 7 }).except(socket).await?;
    ///     Ok("shipped")
    /// }
    /// ```
    pub fn send<E: BroadcastEvent + ?Sized>(&self, event: &E) -> PendingEvent {
        let prepared = serde_json::to_string(event)
            .map_err(Error::from)
            .map(|data| Prepared {
                channels: distinct(event.channels().iter().map(|c| c.name().to_owned())),
                name: event.name().into_owned(),
                data,
            });
        PendingEvent {
            anvil: self.clone(),
            prepared,
            except: None,
        }
    }

    /// Events to `channel` without an event type: `anvil.to(Channel::public("news")).event("posted").with(&data)`.
    pub fn to(&self, channel: Channel) -> To {
        To {
            anvil: self.clone(),
            channels: vec![channel],
        }
    }

    /// Check an event before anything is delivered.
    fn check(&self, prepared: &Prepared, except: Option<&SocketId>) -> Result<String> {
        let name = &prepared.name;
        if name.is_empty() || name.len() > MAX_EVENT_NAME || name.chars().any(char::is_control) {
            return Err(Error::internal(format!(
                "the event name `{}` must be 1 to {MAX_EVENT_NAME} bytes without control characters",
                name.escape_debug()
            )));
        }
        if name.starts_with("pusher:") || name.starts_with("pusher_internal:") {
            return Err(Error::internal(format!(
                "the event name `{}` uses a prefix the protocol reserves (`pusher:`, `pusher_internal:`)",
                name.escape_debug()
            )));
        }
        if prepared.channels.is_empty() || prepared.channels.len() > MAX_CHANNELS {
            return Err(Error::internal(format!(
                "the event `{name}` must go to 1 to {MAX_CHANNELS} channels"
            )));
        }
        if let Some(bad) = prepared.channels.iter().find(|c| !valid_channel(c)) {
            return Err(Error::internal(format!(
                "the event `{name}` names an invalid channel `{}`",
                bad.escape_debug()
            )));
        }
        let max = self.inner.settings.max_event_size;
        if prepared.data.len() > max {
            return Err(Error::internal(format!(
                "the event `{name}` is {} bytes, more than ANVIL_MAX_EVENT_SIZE ({max})",
                prepared.data.len()
            )));
        }
        let wire = serde_json::to_string(&Wire {
            e: name.clone(),
            c: prepared.channels.clone(),
            d: prepared.data.clone(),
            x: except.map(|s| s.as_str().to_owned()),
            k: None,
            u: None,
        })?;
        // The PubSub envelope adds the topic, the process id and a time stamp.
        if wire.len() + 256 > smeltery_core::pubsub::MAX_MESSAGE_BYTES {
            return Err(Error::internal(format!(
                "the event `{name}` is too large to reach the app's other processes ({} bytes)",
                wire.len()
            )));
        }
        Ok(wire)
    }

    async fn publish(&self, prepared: Prepared, except: Option<SocketId>) -> Result<Delivered> {
        let wire = self.check(&prepared, except.as_ref())?;
        if let Some(sent) = self
            .inner
            .spy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            if sent.len() >= MAX_RECORDED {
                sent.remove(0);
            }
            sent.push(testing::Sent::new(
                &prepared.name,
                &prepared.channels,
                &prepared.data,
            ));
        }
        let local = self.deliver(&prepared, except.as_ref().map(SocketId::as_str));
        if let Some(pubsub) = self.inner.pubsub.get() {
            let value: serde_json::Value = serde_json::from_str(&wire)?;
            pubsub.publish_reserved(TOPIC, &value).await?;
        }
        Ok(Delivered { local })
    }

    /// Queue the event for this process's subscribers; the frame is built once per channel.
    fn deliver(&self, prepared: &Prepared, except: Option<&str>) -> usize {
        prepared
            .channels
            .iter()
            .map(|channel| {
                let frame =
                    Utf8Bytes::from(protocol::event(&prepared.name, channel, &prepared.data));
                // Listeners inside the app (Sparks) get every delivered event; they have no socket to leave out.
                self.inner
                    .listeners
                    .send(smeltery_core::channels::ChannelEvent::new(
                        channel.as_str(),
                        prepared.name.as_str(),
                        prepared.data.as_str(),
                    ));
                self.inner.hub.deliver(channel, except, &frame)
            })
            .sum()
    }

    /// Subscribe to core's auth events and raise the watermark to `now` (Unix seconds): the events before this
    /// moment were never seen here, so grants made before it are refused.
    fn listen_for_revocations(&self, pubsub: &PubSub, now: u64) -> Subscription {
        let events = pubsub.subscribe(smeltery_core::auth::EVENTS_TOPIC);
        self.inner.revocations.listening_from(now);
        events
    }

    /// Act on core's auth events (this process's and the others'), until shutdown: remember each revocation (a grant
    /// made before it cannot subscribe) and close the sockets it concerns with 4200.
    async fn revocations_from(
        self,
        events: Subscription,
        token: tokio_util::sync::CancellationToken,
    ) {
        // Each event with the time it was sent (Unix seconds by the sender's clock): a revocation is dated by when it
        // happened, not by when it arrived.
        let events = futures_util::stream::unfold(events, |mut sub| async move {
            let next = sub
                .recv()
                .await
                .map(|m| (m.payload.clone(), m.sent_at / 1000));
            Some((next, sub))
        });
        self.revocations_over(events, token).await;
    }

    /// The loop of [`revocations_from`](Self::revocations_from) over a stream of event payloads with their send
    /// times (Unix seconds); ends with the stream, on `Closed` or on the token.
    ///
    /// An event this version cannot read that names a user ends every credential of that user; one that names no
    /// user is handled like a lag (what this process cannot check, it refuses).
    ///
    /// After `Lagged` (auth events were lost), every grant made up to that second is refused at once, and every
    /// socket holding a subscription such a grant authorized closes with 4200 (its client reconnects and authorizes
    /// again, with a grant this process accepts): from the next second on, each at a random moment within 30 s, at
    /// most one such mass close a minute (`revocation::LagCloser`).
    async fn revocations_over(
        self,
        events: impl futures_util::Stream<
            Item = std::result::Result<(serde_json::Value, u64), RecvError>,
        >,
        token: tokio_util::sync::CancellationToken,
    ) {
        use futures_util::StreamExt as _;
        let mut events = std::pin::pin!(events);
        let mut closer = revocation::LagCloser::default();
        loop {
            let message = tokio::select! {
                biased;
                () = token.cancelled() => return,
                () = sleep_until_some(closer.next()) => {
                    let hub = &self.inner.hub;
                    let due = closer.due(
                        tokio::time::Instant::now(),
                        |limit| hub.granted_before(limit),
                        random_spread,
                    );
                    let closed = due
                        .into_iter()
                        .filter(|(id, limit)| hub.close_if_granted_before(*id, *limit))
                        .count();
                    if closed > 0 {
                        tracing::info!(closed, "{MASS_CLOSED}");
                    }
                    continue;
                }
                message = events.next() => message,
            };
            match message {
                None | Some(Err(RecvError::Closed)) => return,
                Some(Ok((payload, sent))) => {
                    if self.revoke(&payload, sent).is_none() {
                        let limit = self.inner.revocations.fell_behind(unix_now());
                        tracing::error!("{UNREADABLE_EVENT}");
                        closer.lagged(tokio::time::Instant::now(), until_next_second(), limit);
                    }
                }
                Some(Err(RecvError::Lagged(missed))) => {
                    let limit = self.inner.revocations.fell_behind(unix_now());
                    tracing::warn!(missed, "{FELL_BEHIND}");
                    closer.lagged(tokio::time::Instant::now(), until_next_second(), limit);
                }
                Some(Err(_)) => {}
            }
        }
    }

    /// Apply one auth event (its JSON) sent at `sent` (Unix seconds); how many sockets were closed, `None` when the
    /// event names no user this version can read (the caller fails closed).
    ///
    /// An event of a type or credential kind this version does not know (a newer process in a rolling deploy) that
    /// names a user ends every credential of that user (fail closed).
    fn revoke(&self, payload: &serde_json::Value, sent: u64) -> Option<usize> {
        let known = serde_json::from_value::<smeltery_core::auth::AuthEvent>(payload.clone())
            .ok()
            .as_ref()
            .and_then(revocation::Revocation::from_event);
        let revocation = match known {
            Some(revocation) => revocation,
            None => {
                let user = payload.get("user_id").and_then(serde_json::Value::as_i64)?;
                tracing::warn!(
                    user,
                    "anvil: an auth event this version does not know; every credential of its user is treated as ended"
                );
                revocation::Revocation::everything_of(user)
            }
        };
        self.inner.revocations.record(revocation.clone(), sent);
        let closed = self.inner.hub.revoke(&revocation);
        if closed > 0 {
            tracing::info!(closed, "{REVOKED_CLOSED}");
        }
        Some(closed)
    }

    /// Hand the events other processes publish to this process's sockets, until shutdown.
    async fn relay(self, pubsub: PubSub, token: tokio_util::sync::CancellationToken) {
        let messages =
            futures_util::stream::unfold(pubsub.subscribe(TOPIC), |mut sub| async move {
                let next = sub.recv().await.map(|m| (m.remote, m.payload.clone()));
                Some((next, sub))
            });
        self.relay_from(messages, token).await;
    }

    /// The relay's loop over a stream of `(remote, payload)` messages; ends with the stream, on `Closed` or on the
    /// token.
    async fn relay_from(
        self,
        messages: impl futures_util::Stream<
            Item = std::result::Result<(bool, serde_json::Value), RecvError>,
        >,
        token: tokio_util::sync::CancellationToken,
    ) {
        use futures_util::StreamExt as _;
        let mut messages = std::pin::pin!(messages);
        loop {
            let message = tokio::select! {
            () = token.cancelled() => return,
            message = messages.next() => message,
            };
            match message {
                None | Some(Err(RecvError::Closed)) => return,
                // This process delivered its own events itself.
                Some(Ok((false, _))) => {}
                Some(Ok((true, payload))) => {
                    self.deliver_remote(payload);
                }
                Some(Err(RecvError::Lagged(missed))) => {
                    tracing::warn!(missed, "anvil: fell behind other processes' events");
                }
                Some(Err(_)) => {}
            }
        }
    }

    /// Deliver an event another process published, after the checks of a local send; how many sockets got it.
    fn deliver_remote(&self, payload: serde_json::Value) -> usize {
        let Ok(wire) = serde_json::from_value::<Wire>(payload) else {
            tracing::warn!("anvil: an unreadable event from another process");
            return 0;
        };
        if wire.k.is_some() {
            return self.deliver_remote_live(wire);
        }
        let prepared = Prepared {
            channels: distinct(wire.c),
            name: wire.e,
            data: wire.d,
        };
        let except = match wire.x.as_deref() {
            None => None,
            Some(id) => match SocketId::parse(id) {
                Some(id) => Some(id),
                None => {
                    tracing::warn!("anvil: an event from another process was refused");
                    return 0;
                }
            },
        };
        if self.check(&prepared, except.as_ref()).is_err() {
            tracing::warn!("anvil: an event from another process was refused");
            return 0;
        }
        self.deliver(&prepared, except.as_ref().map(SocketId::as_str))
    }

    /// Start recording sends (`testing::AnvilSpy`).
    pub(crate) fn record(&self) {
        let mut spy = self
            .inner
            .spy
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if spy.is_none() {
            *spy = Some(Vec::new());
        }
    }

    pub(crate) fn recorded(&self) -> Vec<testing::Sent> {
        self.inner
            .spy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_default()
    }

    /// Prepare what needs the app: the channel secret, the Origin policy and the PubSub.
    fn boot(&self, app: &App) -> Result<()> {
        let errors = self.inner.channels.errors();
        if !errors.is_empty() {
            return Err(Error::internal(errors.join("; ")));
        }
        let core = app.settings();
        self.inner.settings.check(core)?;
        let policy = origin::Policy::new(
            &core.url,
            &self.inner.settings.allowed_origins,
            &core.cors_allowed_origins,
            core.is_local_development(),
        )?;
        if policy.allows_any() && core.env != "local" {
            tracing::warn!(
                "ANVIL_ALLOWED_ORIGINS contains `*`: pages of any site may open sockets to this app"
            );
        }
        let _ = self.inner.policy.set(policy);
        let secret = match &self.inner.settings.app_secret {
            Some(secret) => Some(secret.clone()),
            // Without a usable APP_KEY the app cannot serve (its web routes need it); console commands still build.
            None => app
                .derive_key(SECRET_PURPOSE)
                .ok()
                .map(|key| key.iter().map(|b| format!("{b:02x}")).collect::<String>()),
        };
        if self.inner.settings.app_key == settings::derived_key(&core.key) && core.is_production() {
            tracing::info!(
                "ANVIL_APP_KEY is derived from APP_KEY and changes when APP_KEY changes; set it in .env when clients \
                 are built with it"
            );
        }
        if let Some(secret) = secret {
            let s = &self.inner.settings;
            let _ = self.inner.session.set(Arc::new(session::Config {
                app_key: s.app_key.clone(),
                secret,
                channels: Arc::clone(&self.inner.channels),
                activity_timeout: s.activity_timeout,
                ping_interval: s.ping_interval,
                pong_timeout: s.pong_timeout,
                max_age: s.max_connection_age,
                max_subscriptions: s.max_subscriptions,
                max_presence_channels: s.max_presence_channels,
                frames_per_second: s.frames_per_second,
                frame_burst: s.frame_burst,
                revocations: Arc::clone(&self.inner.revocations),
            }));
        }
        if let Some(pubsub) = PubSub::of(app) {
            let _ = self.inner.pubsub.set(pubsub);
        }
        self.remember_app(app);
        Ok(())
    }
}

/// The log message when the auth subscription lagged.
const FELL_BEHIND: &str = "anvil: fell behind the auth events; grants made until now are refused, and the sockets \
                           they authorized reconnect within 30 s";

/// The log message when an auth event names no user this version can read.
const UNREADABLE_EVENT: &str = "anvil: an auth event names no user this version can read; grants made until now are \
                                refused, and the sockets they authorized reconnect within 30 s";

/// The log message when sockets closed after a lag.
const MASS_CLOSED: &str = "anvil: closed sockets authorized before missed auth events";

/// The log message when an auth event closed sockets.
const REVOKED_CLOSED: &str = "anvil: closed the sockets of ended credentials";

/// A random moment within `revocation::MASS_CLOSE_SPREAD`.
fn random_spread() -> std::time::Duration {
    let mut bytes = [0_u8; 4];
    // Without a random source every socket closes at once (it still closes).
    let _ = getrandom::fill(&mut bytes);
    let spread = u32::try_from(revocation::MASS_CLOSE_SPREAD.as_millis()).unwrap_or(30_000);
    std::time::Duration::from_millis(u64::from(u32::from_le_bytes(bytes) % spread.max(1)))
}

/// Unix seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The time until the next second of the clock starts (and a little more).
fn until_next_second() -> std::time::Duration {
    let into = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_millis());
    std::time::Duration::from_millis(u64::from(1_000 - into.min(999)) + 50)
}

/// Sleep until `deadline`; never ends without one.
async fn sleep_until_some(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

impl axum::extract::FromRequestParts<App> for Anvil {
    type Rejection = Error;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        Self::of(app).ok_or_else(|| {
            Error::internal("Anvil is not installed: call `.anvil(...)` in bootstrap/app.rs")
        })
    }
}

/// Installs Anvil on an [`AppBuilder`]: `.anvil(routes::channels::register)` in `bootstrap/app.rs`.
pub trait AnvilExt: Sized {
    /// Install Anvil with the settings from `.env` and the channels `register` declares: the socket endpoint
    /// `GET /app/<ANVIL_APP_KEY>` (outside the web stack: no session, no cookies read), the web route
    /// `POST /broadcasting/auth` (session and CSRF, `throttle:600,1`), `POST /api/broadcasting/auth` (an API route for
    /// the bearer credentials of the app's stateless guards, `throttle:600,1`; it answers 404 when the app has none),
    /// and the [`Anvil`] service. An invalid or
    /// repeated channel pattern, an invalid setting or `ANVIL_MAX_CONNECTIONS` not below
    /// `SERVER_MAX_CONNECTIONS` fails the build.
    fn anvil(self, register: impl FnOnce(&mut Channels)) -> Self;

    /// [`anvil`](Self::anvil) with explicit settings.
    fn anvil_with(self, settings: Settings, register: impl FnOnce(&mut Channels)) -> Self;
}

impl AnvilExt for AppBuilder {
    fn anvil(self, register: impl FnOnce(&mut Channels)) -> Self {
        let settings = Settings::from_env(self.settings());
        self.anvil_with(settings, register)
    }

    fn anvil_with(self, settings: Settings, register: impl FnOnce(&mut Channels)) -> Self {
        let mut channels = Channels::new();
        register(&mut channels);
        let key = settings.app_key.clone();
        let anvil = Anvil::new(settings, channels);
        let booted = anvil.clone();
        let relayed = anvil.clone();
        let builder = self
            .channel_authorizer(listeners::Listeners(anvil.clone()))
            .service(anvil)
            .on_boot(move |app| async move { booted.boot(&app) })
            .on_serve(move |app| async move {
                if relayed.serves_sockets() {
                    tracing::info!(path = %format!("/app/{}", relayed.app_key()), "anvil: socket endpoint");
                } else {
                    // The sockets are in the `anvil` process: events this process sends must reach it (under
                    // `PUBSUB_DRIVER=auto` the shared driver; A3).
                    app.set_web_only();
                    tracing::info!("anvil: ANVIL_IN_SERVE=false; the `anvil` process serves the sockets");
                }
                if let Some(pubsub) = PubSub::of(&app) {
                    let token = app.shutdown_token().clone();
                    // Subscribed here, before the server accepts: from now on no auth event is missed.
                    let events = relayed.listen_for_revocations(&pubsub, unix_now());
                    app.spawn_owned(relayed.clone().revocations_from(events, token.clone()));
                    // Presence: this process's rows stay alive, gone processes are swept, and at shutdown this
                    // process's members leave.
                    app.spawn_owned(relayed.clone().presence_heartbeat(token.clone()));
                    app.spawn_owned(relayed.relay(pubsub, token));
                }
                Ok(())
            })
            .routes(|r| {
                r.post("/broadcasting/auth", auth_route::authorize)
                    .name("anvil.auth")
                    .middleware("throttle:600,1");
            })
            .serve_command(process::NAME, process::ABOUT, process::run)
            .api_routes(|r| {
                r.post("/broadcasting/auth", auth_route::authorize_token)
                    .name("anvil.token_auth")
                    .middleware("throttle:600,1");
            });
        if settings::valid_app_key(&key) {
            let path = format!("/{key}");
            builder.api_routes_at("/app", move |r| {
                r.get(&path, endpoint::socket).name("anvil.socket");
            })
        } else {
            // `boot` reports the invalid key; no route is built from it.
            builder
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    fn anvil() -> Anvil {
        let core = smeltery_core::config::Settings::from_env();
        let mut channels = Channels::new();
        channels.public("news");
        Anvil::new(Settings::from_env(&core), channels)
    }

    #[test]
    fn a_channel_carries_a_bounded_number_of_client_events_a_second() {
        let anvil = anvil();
        for n in 0..live::CHANNEL_CLIENT_EVENTS_PER_SECOND {
            // Distinct addresses: only the channel's budget is in play.
            let client = Some(std::net::IpAddr::from([198, 51, 100, n as u8]));
            assert!(
                anvil
                    .whisper("1.1", client, "private-chat.1", "client-x", &json!(1), None)
                    .is_ok()
            );
        }
        assert_eq!(
            anvil.whisper("1.2", None, "private-chat.1", "client-x", &json!(1), None),
            Err(live::CHANNEL_BUDGET_SPENT),
            "the channel's budget, whoever sends"
        );
        assert!(
            anvil
                .whisper("1.1", None, "private-chat.2", "client-x", &json!(1), None)
                .is_ok()
        );
    }

    /// Sweep W5-01: one client address cannot send more than `ANVIL_CLIENT_EVENTS_PER_CLIENT` client events a second
    /// over all its sockets and channels, nor all clients together more than `ANVIL_CLIENT_EVENTS_PER_SECOND`: each
    /// one reaches the other processes (a database row with the `database` driver).
    #[test]
    fn client_events_are_budgeted_per_address_and_per_process() {
        let core = smeltery_core::config::Settings::from_env();
        let mut settings = Settings::from_env(&core);
        settings.client_events_per_client = 5;
        settings.client_events_per_second = 12;
        let anvil = Anvil::new(settings, Channels::new());
        let one = Some(std::net::IpAddr::from([203, 0, 113, 7]));
        // One address, a new socket and channel each time (the per-channel budget never binds).
        for n in 0..5 {
            let channel = format!("private-chat.{n}");
            assert!(
                anvil
                    .whisper(
                        &format!("1.{n}"),
                        one,
                        &channel,
                        "client-x",
                        &json!(1),
                        None
                    )
                    .is_ok()
            );
        }
        assert_eq!(
            anvil.whisper("1.9", one, "private-chat.9", "client-x", &json!(1), None),
            Err(live::CLIENT_BUDGET_SPENT)
        );
        // Other addresses go on until the process's budget is spent.
        for n in 0..7u8 {
            let other = Some(std::net::IpAddr::from([198, 51, 100, n]));
            assert!(
                anvil
                    .whisper("2.1", other, "private-chat.x", "client-x", &json!(1), None)
                    .is_ok(),
                "{n}"
            );
        }
        let late = Some(std::net::IpAddr::from([192, 0, 2, 1]));
        assert_eq!(
            anvil.whisper("3.1", late, "private-chat.y", "client-x", &json!(1), None),
            Err(live::PROCESS_BUDGET_SPENT)
        );
    }

    #[test]
    fn remote_member_events_need_a_well_formed_user_id() {
        let mut channels = Channels::new();
        channels.presence("room.{r}", |_| async { Ok(None) });
        let core = smeltery_core::config::Settings::from_env();
        let anvil = Anvil::new(Settings::from_env(&core), channels);
        let mut socket = anvil.inner.hub.register("1.1", None).ok().unwrap();
        assert!(socket.registration.join("presence-room.1", None));
        let member = |data: &str| json!({ "e": "pusher_internal:member_removed", "c": ["presence-room.1"], "d": data, "k": "m" });
        for bad in [
            r#"{"user_id":""}"#,
            r#"{"user_id":7}"#,
            r#"{"name":"x"}"#,
            "nope",
        ] {
            assert_eq!(anvil.deliver_remote(member(bad)), 0, "{bad}");
        }
        assert_eq!(anvil.deliver_remote(member(r#"{"user_id":"7"}"#)), 1);
        assert!(socket.outbox.try_recv().is_ok());
    }

    #[tokio::test]
    async fn a_join_that_fails_is_undone_and_forgotten() {
        use crate::presence::{Joined, Member, Store};
        /// A store whose joins write the socket and then fail (a timeout after the first statement).
        #[derive(Default)]
        struct Failing(presence::memory::MemoryStore);
        impl Store for Failing {
            fn join<'a>(
                &'a self,
                channel: &'a str,
                socket: &'a str,
                member: &'a Member,
                max: usize,
            ) -> smeltery_core::BoxFuture<'a, Result<Joined>> {
                Box::pin(async move {
                    self.0.join(channel, socket, member, max).await?;
                    Err(Error::internal("cut off"))
                })
            }
            fn leave<'a>(
                &'a self,
                channel: &'a str,
                socket: &'a str,
                user_id: &'a str,
            ) -> smeltery_core::BoxFuture<'a, Result<bool>> {
                self.0.leave(channel, socket, user_id)
            }
            fn members<'a>(
                &'a self,
                channel: &'a str,
                max: usize,
            ) -> smeltery_core::BoxFuture<'a, Result<Vec<Member>>> {
                self.0.members(channel, max)
            }
        }
        let anvil = anvil();
        let failing: Arc<dyn Store> = Arc::new(Failing::default());
        let _ = anvil.inner.store.set(Arc::clone(&failing));
        let result = anvil
            .presence_join("1.1", "presence-room.1", &Member::new(7))
            .await;
        assert!(result.is_err());
        assert!(anvil.memberships_of("1.1").is_empty(), "forgotten");
        assert!(
            failing
                .members("presence-room.1", 10)
                .await
                .unwrap()
                .is_empty(),
            "the half-made join is undone"
        );
    }

    fn wire(name: &str, channels: &[&str], except: Option<&str>) -> serde_json::Value {
        let mut value = json!({ "e": name, "c": channels, "d": "{\"id\":1}" });
        if let Some(id) = except {
            value["x"] = json!(id);
        }
        value
    }

    #[tokio::test]
    async fn the_relay_survives_lag_and_refuses_bad_remote_events() {
        let anvil = anvil();
        let mut socket = anvil.inner.hub.register("1.1", None).ok().unwrap();
        assert!(socket.registration.join("news", None));
        let messages = vec![
            Err(RecvError::Lagged(7)),
            // Unreadable, an invalid channel, a reserved name, a bad socket id, too large.
            Ok((true, json!({ "nonsense": true }))),
            Ok((true, wire("posted", &["bad channel"], None))),
            Ok((
                true,
                wire("pusher_internal:subscription_succeeded", &["news"], None),
            )),
            Ok((true, wire("posted", &["news"], Some("1.1; x")))),
            Ok((
                true,
                json!({ "e": "big", "c": ["news"], "d": "x".repeat(anvil.settings().max_event_size + 1) }),
            )),
            // This process's own event: delivered when it was sent, not again.
            Ok((false, wire("own", &["news"], None))),
            // A good one, with a repeated channel: delivered once.
            Ok((true, wire("posted", &["news", "news"], None))),
            Err(RecvError::Closed),
            Ok((true, wire("after-close", &["news"], None))),
        ];
        let token = tokio_util::sync::CancellationToken::new();
        anvil
            .clone()
            .relay_from(futures_util::stream::iter(messages), token)
            .await;
        let frame = socket.outbox.try_recv().unwrap();
        assert!(frame.as_str().contains("\"posted\""), "{}", frame.as_str());
        assert!(
            socket.outbox.try_recv().is_err(),
            "exactly one event got through"
        );
    }

    #[test]
    fn a_new_process_refuses_grants_made_before_it_started() {
        let anvil = anvil();
        let holder = |issued: u64| crate::revocation::Holder {
            user: 7,
            key: "fake:token:1".into(),
            issued,
        };
        let now = unix_now();
        assert!(anvil.inner.revocations.refuses_holder(&holder(now - 10)));
        assert!(!anvil.inner.revocations.refuses_holder(&holder(now + 1)));
    }

    #[tokio::test]
    async fn listening_to_the_auth_events_refuses_grants_made_before() {
        let app = smeltery_core::AppBuilder::new(smeltery_core::config::Settings::from_env())
            .build()
            .await
            .unwrap()
            .app;
        let pubsub = PubSub::of(&app).unwrap();
        let anvil = anvil();
        let later = unix_now() + 100;
        let holder = |issued: u64| crate::revocation::Holder {
            user: 7,
            key: "fake:token:1".into(),
            issued,
        };
        assert!(!anvil.inner.revocations.refuses_holder(&holder(later - 1)));
        let _events = anvil.listen_for_revocations(&pubsub, later);
        assert!(
            anvil.inner.revocations.refuses_holder(&holder(later - 1)),
            "made before the subscription existed"
        );
        assert!(!anvil.inner.revocations.refuses_holder(&holder(later)));
    }

    #[test]
    fn log_messages_have_no_runs_of_spaces() {
        for message in [FELL_BEHIND, MASS_CLOSED, REVOKED_CLOSED, UNREADABLE_EVENT] {
            assert!(!message.contains("  "), "{message}");
        }
    }

    /// Sweep W5-05: an auth event of a type or credential kind this version does not know (a newer process in a
    /// rolling deploy) ends every credential of the user it names; one that names no user is answered as `None` (the
    /// loop fails closed, see `an_auth_event_naming_no_user_fails_closed`).
    #[test]
    fn auth_events_this_version_cannot_read_fail_closed() {
        use crate::revocation::Holder;
        let anvil = anvil();
        let now = unix_now();
        let holder = |user: i64, key: &str| Holder {
            user,
            key: key.into(),
            issued: now,
        };
        let mut seven = anvil.inner.hub.register("1.1", None).ok().unwrap();
        assert!(
            seven
                .registration
                .join("private-a", Some(holder(7, "web:session:aa")))
        );
        let mut nine = anvil.inner.hub.register("1.2", None).ok().unwrap();
        assert!(
            nine.registration
                .join("private-a", Some(holder(9, "fake:token:3")))
        );
        let mut other = anvil.inner.hub.register("1.3", None).ok().unwrap();
        assert!(
            other
                .registration
                .join("private-a", Some(holder(8, "web:session:bb")))
        );
        let unknown_kind = json!({ "type": "revoked_all", "user_id": 7, "kind": "passkeys" });
        assert_eq!(anvil.revoke(&unknown_kind, now), Some(1));
        assert_eq!(*seven.close.borrow_and_update(), Some(CloseCode::Reconnect));
        assert!(
            anvil
                .inner
                .revocations
                .refuses_holder(&holder(7, "fake:token:4")),
            "every credential of user 7"
        );
        let unknown_type = json!({ "type": "revoked_device", "user_id": 9, "device": "x" });
        assert_eq!(anvil.revoke(&unknown_type, now), Some(1));
        assert_eq!(*nine.close.borrow_and_update(), Some(CloseCode::Reconnect));
        assert_eq!(*other.close.borrow_and_update(), None, "another user");
        assert_eq!(anvil.revoke(&json!({ "type": "rotated" }), now), None);
    }

    /// Sweep W5-06: a revocation is dated by when it was sent, not when it arrived: a grant made well after the
    /// event (beyond the clock margin) is not refused because the event arrived late or again.
    #[test]
    fn revocations_are_dated_by_when_they_were_sent() {
        use crate::revocation::Holder;
        let anvil = anvil();
        let now = unix_now();
        let fresh = Holder {
            user: 7,
            key: "web:session:aa".into(),
            issued: now,
        };
        let logout = json!({ "type": "revoked", "user_id": 7, "key": "web:session:aa" });
        // Sent two minutes ago (before the grant, beyond the 30 s margin).
        assert_eq!(anvil.revoke(&logout, now - 120), Some(0));
        assert!(!anvil.inner.revocations.refuses_holder(&fresh));
        // Sent now: the grant is refused.
        assert_eq!(anvil.revoke(&logout, now), Some(0));
        assert!(anvil.inner.revocations.refuses_holder(&fresh));
    }

    /// Sweep W5-05: an auth event naming no user this version can read is handled like a lag: grants made until
    /// now are refused and the sockets holding them close (spread over 30 s).
    #[tokio::test(start_paused = true)]
    async fn an_auth_event_naming_no_user_fails_closed() {
        use crate::revocation::Holder;
        use futures_util::StreamExt as _;
        let anvil = anvil();
        let now = unix_now();
        let mut old = anvil.inner.hub.register("1.1", None).ok().unwrap();
        assert!(old.registration.join(
            "private-a",
            Some(Holder {
                user: 7,
                key: "fake:token:1".into(),
                issued: now,
            })
        ));
        let token = tokio_util::sync::CancellationToken::new();
        let events = futures_util::stream::iter(vec![Ok((json!({ "type": "rotated" }), now))])
            .chain(futures_util::stream::pending());
        let task = tokio::spawn(anvil.clone().revocations_over(events, token.clone()));
        tokio::time::timeout(std::time::Duration::from_secs(32), old.close.changed())
            .await
            .expect("closed after the unreadable event")
            .unwrap();
        assert_eq!(*old.close.borrow_and_update(), Some(CloseCode::Reconnect));
        token.cancel();
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn falling_behind_the_auth_events_fails_closed() {
        use crate::revocation::Holder;
        use futures_util::StreamExt as _;
        let anvil = anvil();
        let now = unix_now();
        let holder = |key: &str, issued: u64| Holder {
            user: 7,
            key: key.into(),
            issued,
        };
        let mut old = anvil.inner.hub.register("1.1", None).ok().unwrap();
        assert!(
            old.registration
                .join("private-a", Some(holder("fake:token:1", now)))
        );
        let mut fresh = anvil.inner.hub.register("1.2", None).ok().unwrap();
        assert!(
            fresh
                .registration
                .join("private-a", Some(holder("fake:token:2", now + 5)))
        );
        let mut public = anvil.inner.hub.register("1.3", None).ok().unwrap();
        assert!(public.registration.join("news", None));

        let token = tokio_util::sync::CancellationToken::new();
        let events = futures_util::stream::iter(vec![Err(RecvError::Lagged(3))])
            .chain(futures_util::stream::pending());
        let task = tokio::spawn(anvil.clone().revocations_over(events, token.clone()));
        // Within the next second plus the 30 s spread (paused time).
        tokio::time::timeout(std::time::Duration::from_secs(32), old.close.changed())
            .await
            .expect("closed after the lag")
            .unwrap();
        assert_eq!(*old.close.borrow_and_update(), Some(CloseCode::Reconnect));
        assert_eq!(*fresh.close.borrow_and_update(), None, "a later grant");
        assert_eq!(*public.close.borrow_and_update(), None, "no grant");
        assert!(
            anvil
                .inner
                .revocations
                .refuses_holder(&holder("fake:token:3", now)),
            "a grant made before the lag cannot subscribe"
        );
        token.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn reserved_names_are_refused_and_channels_sent_once() {
        let anvil = anvil();
        let mut socket = anvil.inner.hub.register("1.1", None).ok().unwrap();
        assert!(socket.registration.join("news", None));
        let err = anvil
            .to(Channel::public("news"))
            .event("pusher:error")
            .with(&json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("reserves"), "{err}");
        assert!(socket.outbox.try_recv().is_err());
        let delivered = anvil
            .to(Channel::public("news"))
            .to(Channel::public("news"))
            .event("posted")
            .with(&json!({}))
            .await
            .unwrap();
        assert_eq!(delivered.local, 1);
    }

    #[tokio::test]
    async fn the_spy_keeps_a_bounded_record() {
        let anvil = anvil();
        anvil.record();
        for _ in 0..(MAX_RECORDED + 5) {
            anvil
                .to(Channel::public("news"))
                .event("posted")
                .with(&json!({}))
                .await
                .unwrap();
        }
        assert_eq!(anvil.recorded().len(), MAX_RECORDED);
    }
}
