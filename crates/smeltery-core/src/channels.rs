//! Broadcast channels as other crates see them: who may receive a channel's events ([`ChannelAuthorizer`]) and the
//! events as they are delivered in this process ([`ChannelEvents`]).
//!
//! The crate that serves sockets and owns the channel rules (Anvil) implements [`ChannelAuthorizer`] and registers it
//! with [`AppBuilder::channel_authorizer`]; crates that only consume events (Sparks listeners) use it through
//! [`App::channel_authorizer`], without depending on that crate.
//!
//! ```
//! use smeltery_core::auth::Auth;
//! use smeltery_core::channels::{ChannelAuthorizer, ChannelEvent, ChannelEventSender, ChannelEvents};
//! use smeltery_core::{App, BoxFuture, Result};
//!
//! /// Everyone may receive `news`; nothing else exists.
//! struct NewsOnly(ChannelEventSender);
//!
//! impl ChannelAuthorizer for NewsOnly {
//!     fn authorize<'a>(&'a self, _app: &'a App, channel: &'a str, _auth: Option<&'a Auth>)
//!         -> BoxFuture<'a, Result<bool>> {
//!         Box::pin(async move { Ok(channel == "news") })
//!     }
//!     fn events(&self) -> ChannelEvents {
//!         self.0.subscribe()
//!     }
//! }
//!
//! let sender = ChannelEventSender::new();
//! let mut events = sender.subscribe();
//! sender.send(ChannelEvent::new("news", "posted", r#"{"id":1}"#));
//! # let _ = &mut events;
//! let app = smeltery_core::testing::TestApp::new(|b| b.channel_authorizer(NewsOnly(sender)));
//! assert!(app.app().channel_authorizer().is_some());
//! ```

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::auth::Auth;
use crate::pubsub::RecvError;
use crate::{App, AppBuilder, BoxFuture, Result};

/// How many events wait for a slow receiver before it lags.
const CAPACITY: usize = 1024;

/// One event broadcast on one channel, as it is delivered in this process (sent here or by another process).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ChannelEvent {
    /// The full channel name (`news`, `private-orders.7`).
    pub channel: String,
    /// The event name (`App\Events\OrderShipped`, `order.shipped`).
    pub event: String,
    /// The event's data as JSON text.
    pub data: String,
    /// The hex SHA-256 of `data`, computed on first use and shared by every receiver.
    data_sha256: std::sync::OnceLock<String>,
}

impl ChannelEvent {
    /// The event `event` on `channel` with `data` (JSON text).
    pub fn new(
        channel: impl Into<String>,
        event: impl Into<String>,
        data: impl Into<String>,
    ) -> Self {
        Self {
            channel: channel.into(),
            event: event.into(),
            data: data.into(),
            data_sha256: std::sync::OnceLock::new(),
        }
    }

    /// The hex SHA-256 of [`data`](Self::data), computed once for all receivers.
    pub fn data_sha256(&self) -> &str {
        self.data_sha256
            .get_or_init(|| crate::crypto::sha256_hex(&self.data))
    }
}

/// Two events are equal when their channel, name and data are (the cached hash is not compared).
impl PartialEq for ChannelEvent {
    fn eq(&self, other: &Self) -> bool {
        self.channel == other.channel && self.event == other.event && self.data == other.data
    }
}

impl Eq for ChannelEvent {}

/// The sending side of [`ChannelEvents`]: the authorizer's crate sends every event it delivers in this process
/// (one per channel), receivers get them from [`ChannelEventSender::subscribe`]. A cheap clone; sending never waits
/// (each receiver holds up to 1024 events, then lags).
#[derive(Clone)]
pub struct ChannelEventSender {
    tx: broadcast::Sender<Arc<ChannelEvent>>,
}

impl std::fmt::Debug for ChannelEventSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelEventSender")
            .field("receivers", &self.receivers())
            .finish()
    }
}

impl Default for ChannelEventSender {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelEventSender {
    /// A sender without receivers.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self { tx }
    }

    /// Hand `event` to the current receivers; how many there were. Nobody listening is not an error.
    pub fn send(&self, event: ChannelEvent) -> usize {
        if self.tx.receiver_count() == 0 {
            return 0;
        }
        self.tx.send(Arc::new(event)).unwrap_or(0)
    }

    /// The events sent from now on.
    pub fn subscribe(&self) -> ChannelEvents {
        ChannelEvents {
            rx: self.tx.subscribe(),
        }
    }

    /// How many receivers are open.
    pub fn receivers(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// The events delivered in this process from the moment of [`ChannelAuthorizer::events`] on.
#[derive(Debug)]
pub struct ChannelEvents {
    rx: broadcast::Receiver<Arc<ChannelEvent>>,
}

impl ChannelEvents {
    /// The next event.
    ///
    /// # Errors
    /// [`RecvError::Lagged`] when this receiver fell behind (events were skipped; the next call receives again),
    /// [`RecvError::Closed`] when the sender is gone.
    pub async fn recv(&mut self) -> std::result::Result<Arc<ChannelEvent>, RecvError> {
        match self.rx.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(n)) => Err(RecvError::Lagged(n)),
            Err(broadcast::error::RecvError::Closed) => Err(RecvError::Closed),
        }
    }
}

/// Decides who may receive a channel's events and hands out the events delivered in this process: the seam between
/// the crate that owns the channels (Anvil) and crates that consume their events (Sparks listeners). Register it with
/// [`AppBuilder::channel_authorizer`].
pub trait ChannelAuthorizer: Send + Sync + 'static {
    /// Whether the visitor of `auth` (`None`: a request without a session) may receive the events of `channel` (the
    /// full name: `news`, `private-orders.7`), by the same rules as the socket's subscriptions. `Ok(false)` is a
    /// denial; an error is a failure to decide (the caller refuses and reports it).
    fn authorize<'a>(
        &'a self,
        app: &'a App,
        channel: &'a str,
        auth: Option<&'a Auth>,
    ) -> BoxFuture<'a, Result<bool>>;

    /// The events delivered in this process from now on (sent here or by the app's other processes), one per
    /// channel.
    fn events(&self) -> ChannelEvents;
}

impl AppBuilder {
    /// Register the app's [`ChannelAuthorizer`] (the broadcasting crate does this when it is installed);
    /// [`App::channel_authorizer`] returns it.
    pub fn channel_authorizer(self, authorizer: impl ChannelAuthorizer) -> Self {
        self.service::<Arc<dyn ChannelAuthorizer>>(Arc::new(authorizer))
    }
}

impl App {
    /// The registered [`ChannelAuthorizer`], if a broadcasting crate is installed.
    pub fn channel_authorizer(&self) -> Option<Arc<dyn ChannelAuthorizer>> {
        self.service::<Arc<dyn ChannelAuthorizer>>()
            .map(|a| Arc::clone(&*a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn events_reach_every_receiver_and_a_slow_one_lags() {
        let sender = ChannelEventSender::new();
        assert_eq!(
            sender.send(ChannelEvent::new("a", "e", "1")),
            0,
            "nobody listens"
        );
        let mut one = sender.subscribe();
        let mut two = sender.subscribe();
        assert_eq!(sender.receivers(), 2);
        assert_eq!(sender.send(ChannelEvent::new("news", "posted", "{}")), 2);
        assert_eq!(one.recv().await.unwrap().channel, "news");
        assert_eq!(two.recv().await.unwrap().event, "posted");
        for i in 0..=CAPACITY {
            sender.send(ChannelEvent::new("news", "posted", i.to_string()));
        }
        assert!(matches!(one.recv().await, Err(RecvError::Lagged(_))));
        assert!(one.recv().await.is_ok(), "it receives again after the lag");
        drop(sender);
        drop(two);
    }

    #[test]
    fn the_data_hash_is_computed_once() {
        let event = ChannelEvent::new("news", "posted", "{}");
        assert_eq!(event.data_sha256(), crate::crypto::sha256_hex("{}"));
        assert!(std::ptr::eq(event.data_sha256(), event.data_sha256()));
    }

    #[test]
    fn equality_ignores_whether_the_hash_was_computed() {
        let hashed = ChannelEvent::new("news", "posted", "{}");
        let _ = hashed.data_sha256();
        assert_eq!(hashed, ChannelEvent::new("news", "posted", "{}"));
        assert_ne!(hashed, ChannelEvent::new("news", "posted", "[]"));
    }

    #[tokio::test]
    async fn a_receiver_ends_with_its_sender() {
        let sender = ChannelEventSender::new();
        let mut events = sender.subscribe();
        drop(sender);
        assert!(matches!(events.recv().await, Err(RecvError::Closed)));
    }
}
