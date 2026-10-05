//! Anvil's settings (`ANVIL_*` in `.env`).

use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use smeltery_core::config::{app_key_bytes, env};
use smeltery_core::{Error, Result};

/// The key used when `ANVIL_APP_KEY` is unset and `APP_KEY` is unusable (the server refuses to start then anyway:
/// the auth endpoint is a web route).
const FALLBACK_KEY: &str = "smeltery";

/// Anvil's settings, read from `.env` by [`Settings::from_env`].
///
/// | Key | Default | Field |
/// |---|---|---|
/// | `ANVIL_APP_ID` | `smeltery` | `app_id` |
/// | `ANVIL_APP_KEY` (the public key in the socket path `/app/<key>`; letters, digits, `-`, `_`, at most 64) | derived from `APP_KEY` (20 characters) | `app_key` |
/// | `ANVIL_APP_SECRET` (the channel signature secret) | derived from `APP_KEY` | `app_secret` |
/// | `ANVIL_ALLOWED_ORIGINS` (comma-separated origins besides `APP_URL`'s and `CORS_ALLOWED_ORIGINS`; `null` only when listed; `*` = any) | empty | `allowed_origins` |
/// | `ANVIL_MAX_CONNECTIONS` (sockets in this process; below `SERVER_MAX_CONNECTIONS`) | `2048` | `max_connections` |
/// | `ANVIL_MAX_CONNECTIONS_PER_IP` (sockets of one client address, an IPv6 client by its /64; `0` = no limit) | `100` | `max_connections_per_ip` |
/// | `ANVIL_HANDSHAKES_PER_MINUTE` (socket handshakes of one client address a minute) | `60` | `handshakes_per_minute` |
/// | `ANVIL_MAX_SUBSCRIPTIONS` (channels per socket) | `100` | `max_subscriptions` |
/// | `ANVIL_MAX_MESSAGE_SIZE` (bytes of one message from a client; 256 to 1048576) | `10000` | `max_message_size` |
/// | `ANVIL_MAX_EVENT_SIZE` (bytes of one event's data) | `32768` | `max_event_size` |
/// | `ANVIL_MAX_PRESENCE_MEMBERS` (distinct users in one presence channel; the member list holds at most this many) | `100` | `max_presence_members` |
/// | `ANVIL_MAX_MEMBER_BYTES` (bytes of one member's `channel_data`: user id and `user_info`) | `1024` | `max_member_bytes` |
/// | `ANVIL_MAX_PRESENCE_CHANNELS` (presence channels one socket is in) | `10` | `max_presence_channels` |
/// | `ANVIL_CLIENT_EVENTS_PER_SECOND` (client events this process accepts a second, from all sockets) | `500` | `client_events_per_second` |
/// | `ANVIL_CLIENT_EVENTS_PER_CLIENT` (client events one client address sends a second, an IPv6 client by its /64) | `50` | `client_events_per_client` |
/// | `ANVIL_ACTIVITY_TIMEOUT` (seconds; clients ping after this much silence) | `30` | `activity_timeout` |
/// | `ANVIL_PING_INTERVAL` (seconds of silence before the server pings) | `60` | `ping_interval` |
/// | `ANVIL_PONG_TIMEOUT` (seconds to answer the server's ping) | `30` | `pong_timeout` |
/// | `ANVIL_MAX_CONNECTION_AGE` (seconds a socket lives) | `86400` | `max_connection_age` |
/// | `ANVIL_IN_SERVE` (`false`: `serve` answers the socket path with 404; the sockets are in the `anvil` process) | `true` | `in_serve` |
/// | `ANVIL_SERVER_HOST` (the address the `anvil` process listens on) | `127.0.0.1` | `server_host` |
/// | `ANVIL_SERVER_PORT` (the port the `anvil` process listens on) | `8080` | `server_port` |
#[derive(Clone)]
#[non_exhaustive]
pub struct Settings {
    /// The app id (informational).
    pub app_id: String,
    /// The public app key: the socket path is `/app/<app_key>` and the `auth` strings start with it.
    pub app_key: String,
    /// The secret channel signatures are made with; `None` derives it from `APP_KEY`.
    pub app_secret: Option<String>,
    /// Origins allowed to open sockets besides `APP_URL`'s (`*` allows any).
    pub allowed_origins: Vec<String>,
    /// The most sockets this process holds.
    pub max_connections: usize,
    /// The most sockets one client address (an IPv6 client by its /64) holds; zero means no limit.
    pub max_connections_per_ip: usize,
    /// Socket handshakes one client address may make a minute.
    pub handshakes_per_minute: u32,
    /// The most channels one socket subscribes to.
    pub max_subscriptions: usize,
    /// The largest message a client may send, in bytes.
    pub max_message_size: usize,
    /// The largest event data (the JSON the event serializes to), in bytes.
    pub max_event_size: usize,
    /// The most distinct users in one presence channel.
    pub max_presence_members: usize,
    /// The largest `channel_data` of one presence member (its user id and `user_info`), in bytes.
    pub max_member_bytes: usize,
    /// The most presence channels one socket is in.
    pub max_presence_channels: usize,
    /// Client events this process accepts a second, from all its sockets (each also reaches the other processes).
    pub client_events_per_second: u32,
    /// Client events one client address (an IPv6 client by its /64) may send a second, over all its sockets.
    pub client_events_per_client: u32,
    /// The silence after which clients ping (sent to them at connect).
    pub activity_timeout: Duration,
    /// The silence after which the server pings a socket.
    pub ping_interval: Duration,
    /// How long a pinged socket has to answer before it is closed.
    pub pong_timeout: Duration,
    /// How long a socket lives before it is closed (the client reconnects at once).
    pub max_connection_age: Duration,
    /// Frames a socket may send per second (on average).
    pub frames_per_second: u32,
    /// Frames a socket may send at once (the bucket size).
    pub frame_burst: u32,
    /// Messages waiting for one slow socket before it is closed.
    pub outbox: usize,
    /// The longest one write to a socket may take.
    pub write_timeout: Duration,
    /// Whether `serve` serves the socket endpoint; `false` when an `anvil` process holds the sockets.
    pub in_serve: bool,
    /// The address the `anvil` process listens on.
    pub server_host: String,
    /// The port the `anvil` process listens on.
    pub server_port: u16,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("app_id", &self.app_id)
            .field("app_key", &self.app_key)
            .field("app_secret", &self.app_secret.as_ref().map(|_| "***"))
            .field("allowed_origins", &self.allowed_origins)
            .field("max_connections", &self.max_connections)
            .field("max_connections_per_ip", &self.max_connections_per_ip)
            .field("handshakes_per_minute", &self.handshakes_per_minute)
            .field("max_subscriptions", &self.max_subscriptions)
            .field("max_message_size", &self.max_message_size)
            .field("max_event_size", &self.max_event_size)
            .field("max_presence_members", &self.max_presence_members)
            .field("max_member_bytes", &self.max_member_bytes)
            .field("max_presence_channels", &self.max_presence_channels)
            .field("client_events_per_second", &self.client_events_per_second)
            .field("client_events_per_client", &self.client_events_per_client)
            .field("activity_timeout", &self.activity_timeout)
            .field("ping_interval", &self.ping_interval)
            .field("pong_timeout", &self.pong_timeout)
            .field("max_connection_age", &self.max_connection_age)
            .field("in_serve", &self.in_serve)
            .field("server_host", &self.server_host)
            .field("server_port", &self.server_port)
            .finish_non_exhaustive()
    }
}

/// The largest member list the settings may allow (`protocol::presence_list_bound`), in bytes: every socket that
/// joins a presence channel is sent the whole list in one frame.
pub(crate) const MAX_MEMBER_LIST_BYTES: usize = 16 * 1024 * 1024;

/// The largest event data: an event travels between processes inside one PubSub message
/// (`smeltery_core::pubsub::MAX_MESSAGE_BYTES`, 64 KiB, with its envelope).
pub(crate) const MAX_EVENT_SIZE_LIMIT: usize = 48 * 1024;

impl Settings {
    /// Read the settings from `.env` (and the process environment); `app` is the framework's settings (the
    /// default key is derived from its `APP_KEY`).
    pub fn from_env(app: &smeltery_core::config::Settings) -> Self {
        let key: String = env("ANVIL_APP_KEY", "");
        let secret: String = env("ANVIL_APP_SECRET", "");
        let origins: String = env("ANVIL_ALLOWED_ORIGINS", "");
        let secs = |key: &str, default: u64| {
            let (low, high) = bounds(key);
            let value = env::<u64>(key, default);
            let clamped = value.clamp(low, high);
            if clamped != value {
                tracing::warn!(
                    key,
                    low,
                    high,
                    "value outside its range, using the nearest bound"
                );
            }
            Duration::from_secs(clamped)
        };
        Self {
            app_id: env("ANVIL_APP_ID", "smeltery"),
            app_key: if key.trim().is_empty() {
                derived_key(&app.key)
            } else {
                key.trim().to_owned()
            },
            app_secret: Some(secret.trim().to_owned()).filter(|s| !s.is_empty()),
            allowed_origins: origins
                .split(',')
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(str::to_owned)
                .collect(),
            max_connections: env::<usize>("ANVIL_MAX_CONNECTIONS", 2048).max(1),
            max_connections_per_ip: env("ANVIL_MAX_CONNECTIONS_PER_IP", 100),
            handshakes_per_minute: env::<u32>("ANVIL_HANDSHAKES_PER_MINUTE", 60).max(1),
            max_subscriptions: env::<usize>("ANVIL_MAX_SUBSCRIPTIONS", 100).max(1),
            max_message_size: env::<usize>("ANVIL_MAX_MESSAGE_SIZE", 10_000).clamp(256, 1 << 20),
            max_event_size: env::<usize>("ANVIL_MAX_EVENT_SIZE", 32 * 1024).max(256),
            max_presence_members: env::<usize>("ANVIL_MAX_PRESENCE_MEMBERS", 100).clamp(1, 10_000),
            max_member_bytes: env::<usize>("ANVIL_MAX_MEMBER_BYTES", 1024).clamp(64, 8 * 1024),
            max_presence_channels: env::<usize>("ANVIL_MAX_PRESENCE_CHANNELS", 10).clamp(1, 100),
            client_events_per_second: env::<u32>("ANVIL_CLIENT_EVENTS_PER_SECOND", 500)
                .clamp(1, 100_000),
            client_events_per_client: env::<u32>("ANVIL_CLIENT_EVENTS_PER_CLIENT", 50)
                .clamp(1, 100_000),
            activity_timeout: secs("ANVIL_ACTIVITY_TIMEOUT", 30),
            ping_interval: secs("ANVIL_PING_INTERVAL", 60),
            pong_timeout: secs("ANVIL_PONG_TIMEOUT", 30),
            max_connection_age: secs("ANVIL_MAX_CONNECTION_AGE", 86_400),
            frames_per_second: 20,
            frame_burst: 40,
            outbox: 256,
            write_timeout: Duration::from_secs(10),
            in_serve: env::<bool>("ANVIL_IN_SERVE", true),
            server_host: env("ANVIL_SERVER_HOST", "127.0.0.1"),
            server_port: env("ANVIL_SERVER_PORT", 8080),
        }
    }

    /// Check the values that cannot be clamped.
    pub(crate) fn check(&self, app: &smeltery_core::config::Settings) -> Result<()> {
        if !valid_app_key(&self.app_key) {
            return Err(Error::internal(
                "ANVIL_APP_KEY may hold only letters, digits, `-` and `_` (at most 64)",
            ));
        }
        if self.max_connections >= app.server_max_connections {
            return Err(Error::internal(format!(
                "ANVIL_MAX_CONNECTIONS ({}) must be below SERVER_MAX_CONNECTIONS ({}): sockets and HTTP requests \
                 share the server's connections",
                self.max_connections, app.server_max_connections
            )));
        }
        let list =
            crate::protocol::presence_list_bound(self.max_presence_members, self.max_member_bytes);
        if list > MAX_MEMBER_LIST_BYTES {
            return Err(Error::internal(format!(
                "ANVIL_MAX_PRESENCE_MEMBERS ({}) times ANVIL_MAX_MEMBER_BYTES ({}) allows a member list of {list} \
                 bytes, more than {MAX_MEMBER_LIST_BYTES}: every joining socket is sent the whole list",
                self.max_presence_members, self.max_member_bytes
            )));
        }
        if self.max_event_size > MAX_EVENT_SIZE_LIMIT {
            return Err(Error::internal(format!(
                "ANVIL_MAX_EVENT_SIZE ({}) may be at most {MAX_EVENT_SIZE_LIMIT}: events travel between processes \
                 in one PubSub message",
                self.max_event_size
            )));
        }
        if app.server_max_connections_per_ip > 0
            && self.max_connections_per_ip > 0
            && self.max_connections_per_ip >= app.server_max_connections_per_ip
        {
            return Err(Error::internal(format!(
                "ANVIL_MAX_CONNECTIONS_PER_IP ({}) must be below SERVER_MAX_CONNECTIONS_PER_IP ({}): a client's \
                 sockets would otherwise take every connection of its address, its page loads included",
                self.max_connections_per_ip, app.server_max_connections_per_ip
            )));
        }
        for (key, value) in [
            ("ANVIL_ACTIVITY_TIMEOUT", self.activity_timeout),
            ("ANVIL_PING_INTERVAL", self.ping_interval),
            ("ANVIL_PONG_TIMEOUT", self.pong_timeout),
            ("ANVIL_MAX_CONNECTION_AGE", self.max_connection_age),
        ] {
            let (low, high) = bounds(key);
            if value.as_secs() < low || value > Duration::from_secs(high) {
                return Err(Error::internal(format!(
                    "{key} must be between {low} and {high} seconds"
                )));
            }
        }
        if let Some(secret) = &self.app_secret
            && secret.len() < MIN_SECRET_BYTES
            && !matches!(app.env.as_str(), "local" | "testing")
        {
            return Err(Error::internal(format!(
                "ANVIL_APP_SECRET is shorter than {MIN_SECRET_BYTES} bytes: use a long random value, or leave it \
                 empty to derive the secret from APP_KEY"
            )));
        }
        crate::origin::Policy::new(
            &app.url,
            &self.allowed_origins,
            &app.cors_allowed_origins,
            false,
        )?;
        Ok(())
    }
}

/// The accepted range of a timer setting, in seconds.
pub(crate) fn bounds(key: &str) -> (u64, u64) {
    match key {
        "ANVIL_MAX_CONNECTION_AGE" => (60, 30 * 86_400),
        "ANVIL_PONG_TIMEOUT" => (1, 300),
        // ANVIL_ACTIVITY_TIMEOUT, ANVIL_PING_INTERVAL
        _ => (1, 3_600),
    }
}

/// The shortest explicit `ANVIL_APP_SECRET` outside `local` / `testing`, in bytes.
pub(crate) const MIN_SECRET_BYTES: usize = 32;

/// Whether `key` can be the socket path's last segment.
pub(crate) fn valid_app_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The purpose the default app key is derived for: the key is the first 10 bytes of `App::derive_key("anvil.key")`
/// in hex, inside core's labelled derivation (`smeltery-app-key:` and the purpose), so no other key of the app is
/// ever this one.
const KEY_PURPOSE: &str = "anvil.key";

/// The default app key: 20 lowercase hex characters of `App::derive_key("anvil.key")` (HMAC-SHA256 of APP_KEY over
/// core's public label and the purpose; computed here because the socket route is built before the app). The key is
/// public (it is in every page and app that connects), so deriving it reveals nothing about `APP_KEY` (an HMAC is
/// one-way); every process with the same `APP_KEY` agrees on it.
pub(crate) fn derived_key(app_key: &str) -> String {
    let Some(bytes) = app_key_bytes(app_key) else {
        return FALLBACK_KEY.to_owned();
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&bytes) else {
        return FALLBACK_KEY.to_owned();
    };
    mac.update(b"smeltery-app-key:");
    mac.update(KEY_PURPOSE.as_bytes());
    let digest = mac.finalize().into_bytes();
    digest.iter().take(10).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_key_is_derived_and_stable() {
        let a = derived_key("0123456789abcdef0123456789abcdef");
        assert_eq!(a.len(), 20);
        assert!(valid_app_key(&a));
        assert_eq!(a, derived_key("0123456789abcdef0123456789abcdef"));
        assert_ne!(a, derived_key("fedcba9876543210fedcba9876543210"));
        assert_eq!(derived_key(""), FALLBACK_KEY);
    }

    /// Sweep W5-11: the default key is `App::derive_key("anvil.key")` (core's labelled derivation), not a hash of
    /// its own.
    #[tokio::test]
    async fn the_default_key_is_cores_derived_key_for_its_purpose() {
        let mut settings = core();
        settings.key = "0123456789abcdef0123456789abcdef".into();
        let app = smeltery_core::AppBuilder::new(settings.clone())
            .build()
            .await
            .unwrap()
            .app;
        let derived = app.derive_key(KEY_PURPOSE).unwrap();
        let expected: String = derived
            .iter()
            .take(10)
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(derived_key(&settings.key), expected);
    }

    fn core() -> smeltery_core::config::Settings {
        let mut core = smeltery_core::config::Settings::from_env();
        core.env = "production".into();
        core.url = "https://app.example.com".into();
        core.server_max_connections = 4096;
        core.server_max_connections_per_ip = 128;
        core
    }

    #[test]
    fn out_of_range_timers_and_caps_fail_the_check() {
        let core = core();
        let base = Settings::from_env(&core);
        assert!(base.check(&core).is_ok());
        let mut s = base.clone();
        s.max_connection_age = Duration::ZERO;
        assert!(
            s.check(&core)
                .unwrap_err()
                .to_string()
                .contains("ANVIL_MAX_CONNECTION_AGE")
        );
        let mut s = base.clone();
        s.max_connection_age = Duration::from_secs(u64::MAX);
        assert!(s.check(&core).is_err());
        let mut s = base.clone();
        s.ping_interval = Duration::from_secs(7_200);
        assert!(s.check(&core).is_err());
        let mut s = base.clone();
        s.max_connections_per_ip = 128;
        assert!(
            s.check(&core)
                .unwrap_err()
                .to_string()
                .contains("SERVER_MAX_CONNECTIONS_PER_IP")
        );
        // Off on either side: nothing to compare.
        s.max_connections_per_ip = 0;
        assert!(s.check(&core).is_ok());
        assert_eq!(bounds("ANVIL_MAX_CONNECTION_AGE"), (60, 30 * 86_400));
    }

    #[test]
    fn boot_messages_have_no_runs_of_spaces() {
        let core = core();
        let mut s = Settings::from_env(&core);
        s.max_connections_per_ip = 128;
        let per_ip = s.check(&core).unwrap_err().to_string();
        s.max_connections_per_ip = 1;
        s.app_secret = Some("secret".into());
        let secret = s.check(&core).unwrap_err().to_string();
        let origin = crate::origin::Policy::new("not a url", &[], "", false)
            .unwrap_err()
            .to_string();
        for message in [per_ip, secret, origin] {
            assert!(!message.contains("  "), "{message}");
        }
    }

    #[test]
    fn a_short_explicit_secret_is_refused_outside_development() {
        let mut core = core();
        let mut s = Settings::from_env(&core);
        s.app_secret = Some("secret".into());
        assert!(
            s.check(&core)
                .unwrap_err()
                .to_string()
                .contains("ANVIL_APP_SECRET")
        );
        s.app_secret = Some("x".repeat(MIN_SECRET_BYTES));
        assert!(s.check(&core).is_ok());
        s.app_secret = Some("secret".into());
        core.env = "local".into();
        assert!(s.check(&core).is_ok(), "allowed in local development");
    }

    #[test]
    fn app_keys_are_path_safe() {
        assert!(valid_app_key("abc-DEF_123"));
        assert!(!valid_app_key(""));
        assert!(!valid_app_key("a/b"));
        assert!(!valid_app_key("a b"));
        assert!(!valid_app_key(&"a".repeat(65)));
    }

    #[test]
    fn debug_hides_the_secret() {
        let mut s = Settings::from_env(&smeltery_core::config::Settings::from_env());
        s.app_secret = Some("very-secret-value".into());
        assert!(!format!("{s:?}").contains("very-secret-value"));
    }
}
