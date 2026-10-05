//! PubSub: messages between the processes of one app.
//!
//! [`PubSub`] is a service every app has (`app.service::<PubSub>()`, [`PubSub::of`]). A message has a topic and a
//! JSON payload. [`PubSub::publish`] delivers it to the subscribers in this process at once and hands it to the
//! driver, which carries it to the app's other processes; [`PubSub::subscribe`] receives the messages of one topic,
//! from this process and the others. Sparks' `Broadcast` sends its refreshes and events through it, so an agent in a
//! `work` process reaches the pages held by a `serve --no-agents` process.
//!
//! ```
//! # async fn demo(app: smeltery_core::App) -> smeltery_core::Result<()> {
//! use serde_json::json;
//! use smeltery_core::pubsub::PubSub;
//!
//! let pubsub = PubSub::of(&app).expect("every app has one");
//! let mut prices = pubsub.subscribe("prices");
//! pubsub.publish("prices", &json!({ "symbol": "ORE", "price": 7 })).await?;
//! let message = prices.recv().await.expect("delivered in this process");
//! assert_eq!(message.payload["price"], 7);
//! # Ok(())
//! # }
//! ```
//!
//! # Drivers
//!
//! `PUBSUB_DRIVER` chooses how messages reach the other processes:
//!
//! | Value | Messages reach |
//! |---|---|
//! | `auto` (default) | decided by what the process runs, once at start, and logged: `serve` (web server and background work in one process), console commands and `TestApp` use `local`; `serve --no-agents` (or `WATCHFIRE_IN_SERVE=false`) and `work` use `redis` when `CACHE_STORE=redis`, else `database` when the app has a database, else `local` (with a warning) |
//! | `local` | this process only |
//! | `database` | every process using the app's database: the `pubsub_messages` table ([`migrations`]), read by each process every `PUBSUB_POLL_MS` (250) while something in it subscribes |
//! | `redis` | every process using the Redis server of `REDIS_URL` (feature `redis`): `PUBLISH` / `SUBSCRIBE` on the channel `<CACHE_PREFIX>pubsub` |
//!
//! Several `serve` processes behind a load balancer each count as "one process" under `auto`, and so does a `serve`
//! with its background work next to an extra `work` process: set `PUBSUB_DRIVER=database` or `redis` for them.
//!
//! # Delivery
//!
//! At most once, in order per publishing process. A message the driver cannot carry (the database or Redis is down,
//! a full queue) is lost: [`PubSub::publish`] returns the error, [`PubSub::forward`] counts it in
//! [`PubSub::dropped`] and logs it. Each topic has its own buffer of 1024 messages in a process, so a burst on one
//! topic never makes the subscribers of another fall behind; a receiver that falls behind its topic by more than
//! 1024 messages gets [`RecvError::Lagged`]. (Past [`MAX_TOPICS`] subscribed topics in one process, further topics
//! share one buffer.) A message is at most [`MAX_MESSAGE_BYTES`] of JSON. Before the process has started (the
//! `serve` / `work` start, a console command, `TestApp`), messages stay in the process; forwarded ones wait in the
//! queue until the driver is chosen. At shutdown the forwarder keeps taking messages while they come (at most
//! 250 ms apart), then sends what is left within 2 s; what it cannot send, and what is forwarded after it stopped,
//! is counted as dropped.
//!
//! Messages that leave the process are encrypted and authenticated with AES-256-GCM under a key derived from
//! `APP_KEY` (purpose `pubsub`), so the processes must share `APP_KEY`. Each carries a random id and its send time: a
//! message sent more than [`MAX_MESSAGE_AGE`] earlier (or dated that far ahead) by the sender's clock is refused like
//! an unreadable one, and a process delivers each id once, so a captured message written again (into the table or
//! Redis) is never delivered twice. A message longer than a sealed envelope of [`MAX_MESSAGE_BYTES`] is skipped
//! before it is decoded. The processes' clocks must agree within five minutes (NTP); beyond that their messages are
//! refused and logged. Every process of the `database` driver deletes rows
//! older than a minute (or dated more than [`MAX_MESSAGE_AGE`] ahead) every 10 s; the `redis` driver checks its
//! subscriber connection with `PING` every 30 s.
//!
//! # The framework's topics
//!
//! [`RESERVED_TOPICS`] (`auth`, `anvil`, `sparks`) carry auth events, Anvil's events and Sparks' pushes.
//! [`PubSub::publish`] and [`PubSub::forward`] refuse them: a payload there would close sockets and refuse grants,
//! reach private channels, or refresh components, in every process. Auth events go through
//! [`auth::publish_event`](crate::auth::publish_event), Anvil's events through Anvil, Sparks' pushes through
//! `Broadcast`.

mod database;
#[cfg(feature = "redis")]
mod redis;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::app::{App, BoxFuture};
use crate::config::Settings;
use crate::error::{Error, Result};

/// The largest message (its JSON envelope: topic, process id and payload), in bytes.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// How many messages of one topic wait for a slow subscriber in this process.
const LOCAL_CAPACITY: usize = 1024;

/// How many topics get a buffer of their own in one process; further topics share one. The
/// [`RESERVED_TOPICS`] always have their own and do not count.
pub const MAX_TOPICS: usize = 256;

/// The framework's topics (auth events, Anvil's events, Sparks' pushes): each always gets a buffer of its own,
/// outside [`MAX_TOPICS`], so an app's many topics can never put them into the shared buffer.
pub const RESERVED_TOPICS: [&str; 3] = ["auth", "anvil", "sparks"];

/// How many [`PubSub::forward`]ed messages wait for the driver.
pub const FORWARD_QUEUE: usize = 1024;

/// The longest one driver call (an insert, a poll, a `PUBLISH`, a connect) may take.
const DRIVER_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the forwarder keeps sending what is queued after shutdown.
const DRAIN_BUDGET: Duration = Duration::from_secs(2);

/// After shutdown, the forwarder waits this long for one more message before it closes the queue.
const DRAIN_IDLE: Duration = Duration::from_millis(250);

/// The key purpose of the message encryption.
const PURPOSE: &str = "pubsub";

/// The table of the `database` driver.
pub(crate) const TABLE: &str = "pubsub_messages";

/// The oldest message a process accepts from another one (and how far its sender's clock may run ahead): a
/// captured sealed message written again into the table or Redis later is refused.
pub const MAX_MESSAGE_AGE: Duration = Duration::from_secs(300);

/// The topics [`PubSub::publish`] and [`PubSub::forward`] refuse: what is published there acts on every process
/// (auth events close sockets and refuse grants; Anvil's messages reach private channels; Sparks' pushes refresh
/// components).
const GUARDED_TOPICS: [&str; 3] = RESERVED_TOPICS;

/// The longest sealed message: `base64(12-byte nonce, an envelope of MAX_MESSAGE_BYTES, 16-byte tag)`. Anything
/// longer is skipped before it is decoded.
pub(crate) const MAX_SEALED_BYTES: usize = (12 + MAX_MESSAGE_BYTES + 16).div_ceil(3) * 4;

/// The most message ids a process remembers (replays are refused by id within [`MAX_MESSAGE_AGE`]).
const MAX_REMEMBERED_IDS: usize = 100_000;

/// The envelope format (2: with the message id).
const ENVELOPE_VERSION: u8 = 2;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Whether a message sent at `sent` (Unix ms) is fresh at `now`.
fn fresh(sent: u64, now: u64) -> bool {
    let max = u64::try_from(MAX_MESSAGE_AGE.as_millis()).unwrap_or(u64::MAX);
    now.saturating_sub(sent) <= max && sent.saturating_sub(now) <= max
}

/// How a [`PubSub`] reaches the app's other processes (see the [module docs](self)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Driver {
    /// This process only.
    Local,
    /// The `pubsub_messages` table of the app's database.
    Database,
    /// Redis `PUBLISH` / `SUBSCRIBE`.
    Redis,
}

impl Driver {
    /// The `PUBSUB_DRIVER` value of this driver.
    pub fn name(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Database => "database",
            Self::Redis => "redis",
        }
    }
}

/// What a process runs, which decides the driver under `PUBSUB_DRIVER=auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// `serve` with the background work.
    Serve,
    /// `serve --no-agents` (or Watchfire's `WATCHFIRE_IN_SERVE=false`): the background work runs elsewhere.
    WebOnly,
    /// `work`.
    Work,
    /// A process that serves one part of the app beside its other processes ([`start_as_part`]: `smeltery anvil`).
    Part,
    /// A console command or `TestApp`.
    Other,
}

/// `PUBSUB_DRIVER`, parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    Auto,
    Fixed(Driver),
}

fn parse_setting(value: &str) -> Result<Setting> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Ok(Setting::Auto),
        "local" => Ok(Setting::Fixed(Driver::Local)),
        "database" => Ok(Setting::Fixed(Driver::Database)),
        "redis" if cfg!(feature = "redis") => Ok(Setting::Fixed(Driver::Redis)),
        "redis" => Err(Error::internal(
            "PUBSUB_DRIVER=redis needs the `redis` feature of smeltery",
        )),
        other => Err(Error::internal(format!(
            "PUBSUB_DRIVER must be `auto`, `local`, `database` or `redis`, not `{other}`"
        ))),
    }
}

/// What the driver choice reads from the app.
#[derive(Debug, Clone, Copy)]
struct Facts {
    role: Role,
    has_db: bool,
    has_key: bool,
    redis_cache: bool,
}

/// The chosen driver, why, and whether the reason deserves a warning.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    driver: Driver,
    reason: String,
    warn: bool,
}

fn choose(setting: Setting, facts: Facts) -> Result<Choice> {
    let fixed = |driver: Driver| Choice {
        driver,
        reason: format!("PUBSUB_DRIVER={}", driver.name()),
        warn: false,
    };
    let no_key = |driver: Driver| {
        Error::internal(format!(
            "PUBSUB_DRIVER={} needs APP_KEY: messages between processes are encrypted with it",
            driver.name()
        ))
    };
    match setting {
        Setting::Fixed(Driver::Local) => Ok(fixed(Driver::Local)),
        Setting::Fixed(Driver::Database) if !facts.has_db => Err(Error::internal(
            "PUBSUB_DRIVER=database needs a database: set DATABASE_URL in .env",
        )),
        Setting::Fixed(driver) if !facts.has_key => Err(no_key(driver)),
        Setting::Fixed(driver) => Ok(fixed(driver)),
        Setting::Auto => {
            let process = match facts.role {
                Role::Serve => {
                    return Ok(Choice {
                        driver: Driver::Local,
                        reason: "auto: one process serves the web and runs the background work"
                            .to_owned(),
                        warn: false,
                    });
                }
                Role::Other => {
                    return Ok(Choice {
                        driver: Driver::Local,
                        reason: "auto: not a `serve` or `work` process".to_owned(),
                        warn: false,
                    });
                }
                Role::WebOnly => "a web-only `serve` process",
                Role::Work => "a `work` process",
                Role::Part => "a process serving one part of the app",
            };
            let shared = if !facts.has_key {
                None
            } else if facts.redis_cache {
                Some(Driver::Redis)
            } else if facts.has_db {
                Some(Driver::Database)
            } else {
                None
            };
            Ok(match shared {
                Some(driver) => Choice {
                    driver,
                    reason: format!(
                        "auto: {process} shares messages with the app's other processes"
                    ),
                    warn: false,
                },
                None => Choice {
                    driver: Driver::Local,
                    reason: format!(
                        "auto: {process}, but no shared driver is usable (it needs APP_KEY and a database or \
                         CACHE_STORE=redis): messages stay in this process"
                    ),
                    warn: true,
                },
            })
        }
    }
}

/// One message as a subscriber receives it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Message {
    /// The topic it was published on.
    pub topic: String,
    /// The payload.
    pub payload: serde_json::Value,
    /// Whether another process published it.
    pub remote: bool,
    /// When it was published: Unix milliseconds by the publishing process's clock.
    pub sent_at: u64,
}

/// Why [`Subscription::recv`] returned no message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RecvError {
    /// The subscriber fell behind and this many messages of its topic were skipped (of the shared topics, past
    /// [`MAX_TOPICS`]); the next call receives again.
    #[error("the subscriber fell behind and missed {0} messages")]
    Lagged(u64),
    /// The PubSub is gone.
    #[error("the PubSub is closed")]
    Closed,
}

/// The messages of one topic ([`PubSub::subscribe`]).
#[derive(Debug)]
pub struct Subscription {
    topic: String,
    rx: broadcast::Receiver<Arc<Message>>,
}

impl Subscription {
    /// The topic.
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// The next message of the topic.
    ///
    /// # Errors
    /// [`RecvError::Lagged`] when the subscriber fell behind (then it receives again), [`RecvError::Closed`] when
    /// the PubSub is gone.
    pub async fn recv(&mut self) -> std::result::Result<Arc<Message>, RecvError> {
        loop {
            match self.rx.recv().await {
                Ok(message) if message.topic == self.topic => return Ok(message),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => return Err(RecvError::Lagged(n)),
                Err(broadcast::error::RecvError::Closed) => return Err(RecvError::Closed),
            }
        }
    }
}

/// What [`PubSub::forward`] did with a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Forward {
    /// Queued for the other processes.
    Queued,
    /// The driver is `local`: there is nobody to forward to.
    NotShared,
    /// Lost: the queue was full or the message too large (counted in [`PubSub::dropped`] and logged).
    Dropped,
}

/// A transport to the other processes.
trait Transport: Send + Sync + 'static {
    /// Send one sealed message.
    fn send<'a>(&'a self, sealed: &'a str) -> BoxFuture<'a, Result<()>>;
}

/// AES-256-GCM under the `pubsub` key, without associated data: `base64(nonce ‖ ciphertext ‖ tag)` ([`App::encrypt`]'s
/// format and code).
struct Sealer(crate::crypto::Cipher);

impl Sealer {
    fn new(key: &[u8; 32]) -> Result<Self> {
        crate::crypto::Cipher::new(key).map(Self)
    }

    fn seal(&self, plain: &[u8]) -> Result<String> {
        self.0.seal(b"", plain)
    }

    fn open(&self, text: &str) -> Option<Vec<u8>> {
        self.0.open(b"", text)
    }
}

/// The envelope of a message between processes.
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    /// Format version.
    v: u8,
    /// Topic.
    t: String,
    /// The publishing process.
    o: String,
    /// Payload.
    p: serde_json::Value,
    /// When it was sent (Unix ms, the sender's clock): older messages are refused (replays).
    s: u64,
    /// A random id: a process delivers each id once (replays inside [`MAX_MESSAGE_AGE`]).
    i: String,
}

/// The message ids a process has delivered within [`MAX_MESSAGE_AGE`], bounded: when full, the oldest is forgotten
/// and every message sent at or before it is refused from then on (what it cannot vouch for, it refuses).
#[derive(Default)]
struct Replays {
    order: std::collections::VecDeque<(u64, String)>,
    ids: std::collections::HashSet<String>,
    /// Messages sent at or before this time (Unix ms) are refused.
    floor: Option<u64>,
}

impl Replays {
    /// Whether the message `id` sent at `sent` is new at `now` (Unix ms); remembers it.
    fn first_time(&mut self, id: &str, sent: u64, now: u64) -> bool {
        let max = u64::try_from(MAX_MESSAGE_AGE.as_millis()).unwrap_or(u64::MAX);
        // Messages sent this long ago are refused by their age anyway (with room for a sender running ahead).
        while self
            .order
            .front()
            .is_some_and(|(at, _)| now.saturating_sub(*at) > max.saturating_mul(2))
        {
            if let Some((_, old)) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        if self.floor.is_some_and(|floor| sent <= floor) || self.ids.contains(id) {
            return false;
        }
        if self.order.len() >= MAX_REMEMBERED_IDS
            && let Some((at, old)) = self.order.pop_front()
        {
            self.ids.remove(&old);
            self.floor = Some(self.floor.map_or(at, |floor| floor.max(at)));
        }
        self.order.push_back((sent, id.to_owned()));
        self.ids.insert(id.to_owned());
        true
    }
}

/// A message, logged at most once a minute.
struct RareLog {
    last: AtomicU64,
}

impl RareLog {
    const fn new() -> Self {
        Self {
            last: AtomicU64::new(0),
        }
    }

    /// Whether to log now (the first time, then at most once a minute).
    fn due(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_secs().max(1));
        let last = self.last.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < 60 {
            return false;
        }
        self.last
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }
}

struct Shared {
    /// This process's id (random): receivers skip their own messages.
    origin: String,
    /// One buffer per subscribed topic: a burst on one topic cannot make another topic's subscribers lag.
    topics: Mutex<std::collections::HashMap<String, broadcast::Sender<Arc<Message>>>>,
    /// The buffer of the topics past [`MAX_TOPICS`] (its subscribers filter by topic).
    overflow: broadcast::Sender<Arc<Message>>,
    setting: Setting,
    poll_interval: Duration,
    #[cfg_attr(not(feature = "redis"), allow(dead_code))]
    redis_channel: String,
    chosen: OnceLock<Driver>,
    starting: std::sync::atomic::AtomicBool,
    queue: mpsc::Sender<String>,
    queue_rx: Mutex<Option<mpsc::Receiver<String>>>,
    sealer: OnceLock<Sealer>,
    transport: OnceLock<Arc<dyn Transport>>,
    dropped: AtomicU64,
    drop_log: RareLog,
    undecodable: AtomicU64,
    undecodable_log: RareLog,
    /// Messages that arrived again (replays), refused.
    replayed: AtomicU64,
    replayed_log: RareLog,
    replays: Mutex<Replays>,
}

impl Shared {
    fn topics(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, broadcast::Sender<Arc<Message>>>>
    {
        self.topics.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many subscriptions are open in this process.
    pub(crate) fn subscribers(&self) -> usize {
        self.topics()
            .values()
            .map(broadcast::Sender::receiver_count)
            .sum::<usize>()
            + self.overflow.receiver_count()
    }

    /// A receiver for `topic`: its own buffer, or the shared one past [`MAX_TOPICS`].
    fn subscribe(&self, topic: &str) -> broadcast::Receiver<Arc<Message>> {
        let mut topics = self.topics();
        if let Some(sender) = topics.get(topic) {
            return sender.subscribe();
        }
        let reserved = RESERVED_TOPICS.contains(&topic);
        let counted = |topics: &std::collections::HashMap<String, _>| {
            topics
                .keys()
                .filter(|t| !RESERVED_TOPICS.contains(&t.as_str()))
                .count()
        };
        if !reserved && counted(&topics) >= MAX_TOPICS {
            topics.retain(|_, sender| sender.receiver_count() > 0);
        }
        if !reserved && counted(&topics) >= MAX_TOPICS {
            return self.overflow.subscribe();
        }
        let (sender, receiver) = broadcast::channel(LOCAL_CAPACITY);
        topics.insert(topic.to_owned(), sender);
        receiver
    }

    /// Hand `message` to the subscribers of its topic in this process.
    fn deliver(&self, message: Message) {
        let message = Arc::new(message);
        {
            let mut topics = self.topics();
            if let Some(sender) = topics.get(&message.topic) {
                if sender.receiver_count() > 0 {
                    let _ = sender.send(Arc::clone(&message));
                } else {
                    // Nobody listens any more: the topic's buffer goes (a later subscribe makes a new one).
                    topics.remove(&message.topic);
                }
            }
        }
        if self.overflow.receiver_count() > 0 {
            let _ = self.overflow.send(message);
        }
    }

    /// Deliver a sealed message from another process to the subscribers here.
    fn receive(&self, sealed: &str) {
        if self.subscribers() == 0 {
            return;
        }
        let Some(sealer) = self.sealer.get() else {
            return;
        };
        let now = now_ms();
        // Longer than any sealed envelope: not one of ours; skipped before it is decoded.
        let envelope = (sealed.len() <= MAX_SEALED_BYTES)
            .then(|| sealer.open(sealed))
            .flatten()
            .and_then(|plain| serde_json::from_slice::<Envelope>(&plain).ok());
        let Some(envelope) = envelope.filter(|e| e.v == ENVELOPE_VERSION && fresh(e.s, now)) else {
            self.count_undecodable();
            return;
        };
        if envelope.o == self.origin {
            return;
        }
        let first = self
            .replays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .first_time(&envelope.i, envelope.s, now);
        if !first {
            let n = self.replayed.fetch_add(1, Ordering::Relaxed) + 1;
            if self.replayed_log.due() {
                tracing::warn!(
                    total = n,
                    "pubsub: a message from another process arrived again (a replay?); skipped (logged at most once a minute)"
                );
            }
            return;
        }
        self.deliver(Message {
            topic: envelope.t,
            payload: envelope.p,
            remote: true,
            sent_at: envelope.s,
        });
    }

    /// Count (and log at most once a minute) a message that could not be read.
    pub(crate) fn count_undecodable(&self) {
        let n = self.undecodable.fetch_add(1, Ordering::Relaxed) + 1;
        if self.undecodable_log.due() {
            tracing::warn!(
                total = n,
                "pubsub: a message from another process could not be read (another APP_KEY? longer than any message?) or is outside MAX_MESSAGE_AGE (clocks more than 5 minutes apart?); skipped"
            );
        }
    }

    fn count_drop(&self, why: &str) {
        let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if self.drop_log.due() {
            tracing::warn!(
                total = n,
                why,
                "pubsub: a message for the other processes was dropped (logged at most once a minute)"
            );
        }
    }
}

/// Messages between the processes of one app (see the [module docs](self)). A cheap clone; every app has one.
#[derive(Clone)]
pub struct PubSub {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for PubSub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PubSub")
            .field("driver", &self.driver())
            .field("subscribers", &self.shared.subscribers())
            .field("dropped", &self.dropped())
            .finish_non_exhaustive()
    }
}

impl PubSub {
    /// A PubSub for these settings, not started yet.
    ///
    /// # Errors
    /// `PUBSUB_DRIVER` is not a known driver, or `redis` without the `redis` feature.
    pub(crate) fn new(settings: &Settings) -> Result<Self> {
        let setting = parse_setting(&settings.pubsub_driver)?;
        let (overflow, _) = broadcast::channel(LOCAL_CAPACITY);
        let (queue, queue_rx) = mpsc::channel(FORWARD_QUEUE);
        Ok(Self {
            shared: Arc::new(Shared {
                origin: crate::crypto::random_token(16)?,
                topics: Mutex::new(std::collections::HashMap::new()),
                overflow,
                setting,
                poll_interval: settings.pubsub_poll_interval,
                redis_channel: format!("{}pubsub", settings.cache_prefix),
                chosen: OnceLock::new(),
                starting: std::sync::atomic::AtomicBool::new(false),
                queue,
                queue_rx: Mutex::new(Some(queue_rx)),
                sealer: OnceLock::new(),
                transport: OnceLock::new(),
                dropped: AtomicU64::new(0),
                drop_log: RareLog::new(),
                undecodable: AtomicU64::new(0),
                undecodable_log: RareLog::new(),
                replayed: AtomicU64::new(0),
                replayed_log: RareLog::new(),
                replays: Mutex::new(Replays::default()),
            }),
        })
    }

    /// The app's PubSub.
    pub fn of(app: &App) -> Option<Self> {
        app.service::<Self>().map(|p| (*p).clone())
    }

    /// The driver, once the process has started (`None` before).
    pub fn driver(&self) -> Option<Driver> {
        self.shared.chosen.get().copied()
    }

    /// How many forwarded messages were lost (a full queue, a message too large, a driver error).
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Receive the messages of `topic`, from this process and (with a shared driver) the others.
    pub fn subscribe(&self, topic: impl Into<String>) -> Subscription {
        let topic = topic.into();
        let rx = self.shared.subscribe(&topic);
        Subscription { topic, rx }
    }

    /// The JSON envelope of a message sent at `sent`, checked against [`MAX_MESSAGE_BYTES`].
    fn envelope_at(&self, topic: &str, payload: serde_json::Value, sent: u64) -> Result<String> {
        let text = serde_json::to_string(&Envelope {
            v: ENVELOPE_VERSION,
            t: topic.to_owned(),
            o: self.shared.origin.clone(),
            p: payload,
            s: sent,
            i: crate::crypto::random_token(16)?,
        })?;
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(Error::internal(format!(
                "the PubSub message on `{topic}` is {} bytes, more than {MAX_MESSAGE_BYTES}",
                text.len()
            )));
        }
        Ok(text)
    }

    /// The envelope of a message sent now (the SQLite tests).
    #[cfg(all(test, feature = "sqlite"))]
    fn envelope(&self, topic: &str, payload: serde_json::Value) -> Result<String> {
        self.envelope_at(topic, payload, now_ms())
    }

    /// The error for a topic only the framework publishes on.
    fn guarded(topic: &str) -> Option<Error> {
        GUARDED_TOPICS.contains(&topic).then(|| {
            Error::internal(format!(
                "the PubSub topic `{topic}` is the framework's: auth events go through `auth::publish_event`, \
                 Anvil's events through `Anvil`, Sparks' pushes through `Broadcast`"
            ))
        })
    }

    /// Deliver `payload` on `topic` to the subscribers in this process at once, and send it to the other
    /// processes (with a shared driver), waiting for the driver.
    ///
    /// # Errors
    /// `topic` is `auth`, `anvil` or `sparks` (the framework's; nothing is delivered), the payload does not serialize or is
    /// larger than [`MAX_MESSAGE_BYTES`] (nothing is delivered), or the driver failed (the subscribers here got it;
    /// the other processes did not).
    pub async fn publish(&self, topic: &str, payload: &impl Serialize) -> Result<()> {
        if let Some(error) = Self::guarded(topic) {
            return Err(error);
        }
        self.publish_reserved(topic, payload).await
    }

    /// [`publish`](Self::publish) on any topic, the framework's included: for the framework's crates (core's auth
    /// events, Anvil, Sparks). Not for app code.
    ///
    /// # Errors
    /// As [`publish`](Self::publish), except for the topic.
    #[doc(hidden)]
    pub async fn publish_reserved(&self, topic: &str, payload: &impl Serialize) -> Result<()> {
        let value = serde_json::to_value(payload)?;
        let sent = now_ms();
        let text = self.envelope_at(topic, value.clone(), sent)?;
        self.shared.deliver(Message {
            topic: topic.to_owned(),
            payload: value,
            remote: false,
            sent_at: sent,
        });
        let (Some(transport), Some(sealer)) =
            (self.shared.transport.get(), self.shared.sealer.get())
        else {
            return Ok(());
        };
        let sealed = sealer.seal(text.as_bytes())?;
        tokio::time::timeout(DRIVER_TIMEOUT, transport.send(&sealed))
            .await
            .map_err(|_| {
                Error::internal(format!(
                    "the PubSub driver did not answer within {DRIVER_TIMEOUT:?}"
                ))
            })??;
        Ok(())
    }

    /// Send `payload` on `topic` to the other processes only, without waiting: for publishers that delivered in
    /// this process themselves and cannot wait (Sparks' `Broadcast`). The message goes into a queue of
    /// [`FORWARD_QUEUE`] that one task of the app sends on; a full queue, a message larger than
    /// [`MAX_MESSAGE_BYTES`] or a driver error loses it (counted in [`PubSub::dropped`], logged at most once a
    /// minute). The framework's topics `auth`, `anvil` and `sparks` are refused ([`Forward::Dropped`], logged).
    pub fn forward(&self, topic: &str, payload: &impl Serialize) -> Forward {
        if let Some(error) = Self::guarded(topic) {
            tracing::warn!(topic, error = %error, "pubsub: message not forwarded");
            return Forward::Dropped;
        }
        self.forward_reserved(topic, payload)
    }

    /// [`forward`](Self::forward) on any topic, the framework's included: for the framework's crates. Not for app
    /// code.
    #[doc(hidden)]
    pub fn forward_reserved(&self, topic: &str, payload: &impl Serialize) -> Forward {
        if self.driver() == Some(Driver::Local) {
            return Forward::NotShared;
        }
        let text = match serde_json::to_value(payload)
            .map_err(Error::from)
            .and_then(|value| self.envelope_at(topic, value, now_ms()))
        {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(topic, error = %e, "pubsub: message not forwarded");
                self.shared.count_drop("not serializable or too large");
                return Forward::Dropped;
            }
        };
        match self.shared.queue.try_send(text) {
            Ok(()) => Forward::Queued,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.shared.count_drop("the queue is full");
                Forward::Dropped
            }
            // Closed with a shared driver: the forwarder has stopped (shutdown).
            Err(mpsc::error::TrySendError::Closed(_))
                if self.driver().is_some_and(|d| d != Driver::Local) =>
            {
                self.shared.count_drop("the process is shutting down");
                Forward::Dropped
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Forward::NotShared,
        }
    }
}

fn facts(app: &App, role: Role) -> Facts {
    Facts {
        role,
        has_db: app.db().is_ok(),
        has_key: app.purpose_key(PURPOSE).is_ok(),
        redis_cache: cfg!(feature = "redis") && app.settings().cache_store.trim() == "redis",
    }
}

/// Fail when an explicit `PUBSUB_DRIVER` (`database`, `redis`) cannot work in this app (no database, no `APP_KEY`):
/// `start_background` checks it before any background work starts.
///
/// # Errors
/// What the driver lacks.
pub(crate) fn check(app: &App) -> Result<()> {
    let Some(pubsub) = PubSub::of(app) else {
        return Ok(());
    };
    match pubsub.shared.setting {
        Setting::Fixed(_) if pubsub.shared.chosen.get().is_none() => {
            choose(pubsub.shared.setting, facts(app, Role::Other)).map(|_| ())
        }
        _ => Ok(()),
    }
}

/// Start the app's PubSub for a process that serves one part of the app beside its other processes (Anvil's
/// `anvil` process, which holds the sockets while `serve` answers the pages): under `PUBSUB_DRIVER=auto` it uses the
/// shared driver (Redis when `CACHE_STORE=redis`, else the database). Returns the driver in use; when the PubSub was
/// started already, the one it started with. Call it before [`serve_on`](crate::serve_on), which then keeps it.
///
/// # Errors
/// An explicit `PUBSUB_DRIVER` cannot work (no database, no `APP_KEY`, a bad `REDIS_URL`).
pub async fn start_as_part(app: &App) -> Result<Driver> {
    start(app, Role::Part).await?;
    Ok(PubSub::of(app)
        .and_then(|pubsub| pubsub.driver())
        .unwrap_or(Driver::Local))
}

/// Choose the driver for `role` and start it (once per app; later calls do nothing). `serve`, `work`, console
/// commands and `TestApp` call it.
///
/// # Errors
/// An explicit `PUBSUB_DRIVER` cannot work (no database, no `APP_KEY`, a bad `REDIS_URL`).
pub(crate) async fn start(app: &App, role: Role) -> Result<()> {
    let Some(pubsub) = PubSub::of(app) else {
        return Ok(());
    };
    let shared = &pubsub.shared;
    if shared.chosen.get().is_some() {
        return Ok(());
    }
    #[cfg(feature = "redis")]
    let settings = app.settings();
    let choice = choose(shared.setting, facts(app, role))?;
    let token = app.shutdown_token().clone();
    let transport: Option<Arc<dyn Transport>> = match choice.driver {
        Driver::Local => None,
        Driver::Database => {
            let db = app.db()?;
            Some(Arc::new(database::DatabaseTransport::new(db)))
        }
        #[cfg(feature = "redis")]
        Driver::Redis => Some(Arc::new(redis::RedisTransport::new(
            &settings.redis_url,
            &shared.redis_channel,
        )?)),
        #[cfg(not(feature = "redis"))]
        Driver::Redis => {
            return Err(Error::internal(
                "PUBSUB_DRIVER=redis needs the `redis` feature of smeltery",
            ));
        }
    };
    let sealer = match &transport {
        Some(_) => Some(Sealer::new(&app.purpose_key(PURPOSE)?)?),
        None => None,
    };
    // One start per app; the driver is published (`chosen`) only once the sealer, transport and tasks are in place,
    // so `driver()` never names a shared driver that `publish` cannot use yet.
    if shared.starting.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    if choice.warn {
        tracing::warn!(driver = choice.driver.name(), reason = %choice.reason, "pubsub driver");
    } else {
        tracing::info!(driver = choice.driver.name(), reason = %choice.reason, "pubsub driver");
    }
    let queue_rx = shared
        .queue_rx
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    let (Some(transport), Some(sealer)) = (transport, sealer) else {
        // Local: what was queued before the start has nobody to go to; later forwards answer `NotShared`.
        drop(queue_rx);
        let _ = shared.chosen.set(choice.driver);
        return Ok(());
    };
    let _ = shared.sealer.set(sealer);
    let _ = shared.transport.set(Arc::clone(&transport));
    if let Some(rx) = queue_rx {
        app.tasks().spawn(forward_loop(
            Arc::clone(shared),
            Arc::clone(&transport),
            rx,
            token.clone(),
        ));
    }
    match choice.driver {
        Driver::Database => {
            let db = app.db()?;
            app.tasks().spawn(database::poll_loop(
                Arc::clone(shared),
                db.clone(),
                shared.poll_interval,
                token.clone(),
            ));
            app.tasks().spawn(database::prune_loop(db, token));
        }
        #[cfg(feature = "redis")]
        Driver::Redis => {
            app.tasks().spawn(redis::subscribe_loop(
                Arc::clone(shared),
                settings.redis_url.clone(),
                shared.redis_channel.clone(),
                token,
            ));
        }
        _ => {}
    }
    let _ = shared.chosen.set(choice.driver);
    Ok(())
}

/// Send what [`PubSub::forward`] queued, until shutdown; then what is still queued, within [`DRAIN_BUDGET`].
async fn forward_loop(
    shared: Arc<Shared>,
    transport: Arc<dyn Transport>,
    mut rx: mpsc::Receiver<String>,
    token: CancellationToken,
) {
    let mut outage = Outage::new("pubsub: sending to the other processes");
    loop {
        let text = tokio::select! {
            biased;
            () = token.cancelled() => break,
            next = rx.recv() => match next {
                Some(text) => text,
                None => return,
            },
        };
        send_one(&shared, transport.as_ref(), &text, &mut outage).await;
    }
    // Work that stops on the same token (agents) may still push: keep taking messages while they come (at most
    // [`DRAIN_IDLE`] apart), then close the queue (later forwards are counted as dropped) and send what is left,
    // all within [`DRAIN_BUDGET`].
    let drain = async {
        while let Ok(Some(text)) = tokio::time::timeout(DRAIN_IDLE, rx.recv()).await {
            send_one(&shared, transport.as_ref(), &text, &mut outage).await;
        }
        rx.close();
        while let Some(text) = rx.recv().await {
            send_one(&shared, transport.as_ref(), &text, &mut outage).await;
        }
    };
    let _ = tokio::time::timeout(DRAIN_BUDGET, drain).await;
    // What the budget left unsent is lost: counted like any drop.
    rx.close();
    while rx.try_recv().is_ok() {
        shared.count_drop("the shutdown budget ran out");
    }
}

async fn send_one(shared: &Shared, transport: &dyn Transport, text: &str, outage: &mut Outage) {
    let Some(sealer) = shared.sealer.get() else {
        return;
    };
    let sent = match sealer.seal(text.as_bytes()) {
        Ok(sealed) => tokio::time::timeout(DRIVER_TIMEOUT, transport.send(&sealed))
            .await
            .unwrap_or_else(|_| {
                Err(Error::internal(format!(
                    "no answer within {DRIVER_TIMEOUT:?}"
                )))
            }),
        Err(e) => Err(e),
    };
    match sent {
        Ok(()) => outage.ok(),
        Err(e) => {
            outage.fail(&e);
            shared.count_drop("the driver failed");
        }
    }
}

/// Logs a failing call once until it works again.
struct Outage {
    what: &'static str,
    down: bool,
}

impl Outage {
    fn new(what: &'static str) -> Self {
        Self { what, down: false }
    }

    fn fail(&mut self, error: &dyn std::fmt::Display) {
        if self.down {
            tracing::debug!(error = %error, "{} failed", self.what);
        } else {
            tracing::warn!(error = %error, "{} failed; retrying (logged once until it works again)", self.what);
            self.down = true;
        }
    }

    fn ok(&mut self) {
        if std::mem::take(&mut self.down) {
            tracing::info!("{} works again", self.what);
        }
    }
}

/// The wait after `previous` failures in a row: 1 s doubling to 30 s, ±25 % jitter.
fn backoff(failures: u32) -> Duration {
    let base = 1000_u64.saturating_mul(1 << failures.min(5)).min(30_000);
    let jitter = crate::crypto::random_bytes(1)
        .ok()
        .and_then(|b| b.first().copied())
        .map_or(0, u64::from);
    // 75 % + (0..=255)/255 * 50 %.
    Duration::from_millis(base * 3 / 4 + base * jitter / 510)
}

/// The `pubsub_messages` table of the `database` driver, for the app's migration (`smeltery pubsub:install`
/// writes one that calls these).
///
/// ```
/// use smeltery_core::Result;
/// use smeltery_core::db::migration::{Migration, Schema};
///
/// pub struct CreatePubsubMessagesTable;
///
/// impl Migration for CreatePubsubMessagesTable {
///     fn name(&self) -> &'static str {
///         "2026_10_05_000000_create_pubsub_messages_table"
///     }
///
///     async fn up(&self, schema: &Schema) -> Result<()> {
///         smeltery_core::pubsub::migrations::up(schema).await
///     }
///
///     async fn down(&self, schema: &Schema) -> Result<()> {
///         smeltery_core::pubsub::migrations::down(schema).await
///     }
/// }
/// ```
///
/// | Column | Type |
/// |---|---|
/// | `id` | 64-bit auto-increment primary key |
/// | `payload` | text (`MEDIUMTEXT` on MySQL): the encrypted message |
/// | `created_at` | Unix milliseconds by the database's clock, indexed |
pub mod migrations {
    use crate::db::Backend;
    use crate::db::migration::Schema;
    use crate::error::Result;

    /// Create the `pubsub_messages` table.
    ///
    /// # Errors
    /// The table exists already, or a statement fails.
    pub async fn up(schema: &Schema) -> Result<()> {
        schema
            .create(super::TABLE, |t| {
                t.id();
                t.text("payload");
                t.big_integer("created_at").index();
            })
            .await?;
        if schema.backend() == Backend::MySql {
            // TEXT holds 64 KB; an encrypted message of `MAX_MESSAGE_BYTES` is larger in base64.
            schema
                .raw("ALTER TABLE `pubsub_messages` MODIFY `payload` MEDIUMTEXT NOT NULL")
                .await?;
        }
        Ok(())
    }

    /// Drop the table.
    ///
    /// # Errors
    /// A statement fails.
    pub async fn down(schema: &Schema) -> Result<()> {
        schema.drop_if_exists(super::TABLE).await
    }
}

#[cfg(test)]
mod tests;
