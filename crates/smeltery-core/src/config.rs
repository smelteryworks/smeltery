//! Configuration: `.env` loading, the [`env()`] reader and the framework [`Settings`].
//!
//! An app keeps its configuration in typed Rust structs under `config/`, filled with
//! [`env()`]:
//!
//! ```
//! use smeltery_core::config::env;
//!
//! pub struct AppConfig {
//!     pub name: String,
//!     pub debug: bool,
//!     pub port: u16,
//! }
//!
//! pub fn app() -> AppConfig {
//!     AppConfig {
//!         name: env("APP_NAME", "Smeltery"),
//!         debug: env("APP_DEBUG", false),
//!         port: env("SERVER_PORT", 8000),
//!     }
//! }
//! # assert_eq!(app().port, 8000);
//! ```
//!
//! A value from the real process environment wins over the same key in `.env`, so a
//! deployment can override any setting. The `.env` file is parsed by Smeltery and kept in
//! memory: the process environment is never modified.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use crate::app::App;
use crate::error::Error;

/// The values loaded from `.env` files, shared by the whole process.
fn dotenv() -> &'static RwLock<HashMap<String, String>> {
    static DOTENV: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
    DOTENV.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Load a `.env` file. A missing file is not an error (it returns `Ok(false)`).
///
/// Later loads override earlier ones for the same key. The process environment still wins
/// over every `.env` value when [`env()`] reads a key.
///
/// # Errors
/// The file exists but cannot be read.
pub fn load_env_file(path: impl AsRef<Path>) -> std::io::Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let parsed = parse_env(&text);
    let mut map = dotenv().write().unwrap_or_else(|e| e.into_inner());
    map.extend(parsed);
    Ok(true)
}

/// Set a `.env`-level value in memory, e.g. from a test.
///
/// The process environment still wins over it.
pub fn set_env_value(key: impl Into<String>, value: impl Into<String>) {
    let mut map = dotenv().write().unwrap_or_else(|e| e.into_inner());
    map.insert(key.into(), value.into());
}

/// The raw value of `key`: the process environment first, then the loaded `.env` values.
pub fn env_value(key: &str) -> Option<String> {
    if let Ok(value) = std::env::var(key) {
        return Some(value);
    }
    let map = dotenv().read().unwrap_or_else(|e| e.into_inner());
    map.get(key).cloned()
}

/// Read `key` as a `T`, or return `default` when it is unset or cannot be parsed.
///
/// An unparsable value logs a warning naming the key (never the value, which may be a
/// secret).
///
/// ```
/// use smeltery_core::config::env;
///
/// let name: String = env("SMELTERY_DOC_UNSET_NAME", "Smeltery");
/// let port: u16 = env("SMELTERY_DOC_UNSET_PORT", 8000);
/// let debug: bool = env("SMELTERY_DOC_UNSET_DEBUG", false);
/// let key: Option<String> = env("SMELTERY_DOC_UNSET_KEY", None);
/// assert_eq!((name.as_str(), port, debug, key), ("Smeltery", 8000, false, None));
/// ```
pub fn env<T: FromEnv>(key: &str, default: T::Default) -> T {
    match env_value(key) {
        None => T::from_default(default),
        Some(raw) => T::parse_env(raw.trim()).unwrap_or_else(|| {
            tracing::warn!(key, "invalid value in the environment, using the default");
            T::from_default(default)
        }),
    }
}

/// A type [`env()`] can read.
///
/// `Default` is the type of the fallback argument, so `env("PORT", 8000)` infers `u16` for
/// the literal when the target is a `u16`, and strings take a `&str` default.
pub trait FromEnv: Sized {
    /// The type of the default value passed to [`env()`].
    type Default;

    /// Turn the default into the value.
    fn from_default(default: Self::Default) -> Self;

    /// Parse the raw (trimmed) string, `None` when it is not valid.
    fn parse_env(raw: &str) -> Option<Self>;
}

impl FromEnv for String {
    type Default = &'static str;

    fn from_default(default: &'static str) -> Self {
        default.to_owned()
    }

    fn parse_env(raw: &str) -> Option<Self> {
        Some(raw.to_owned())
    }
}

impl FromEnv for bool {
    type Default = bool;

    fn from_default(default: bool) -> Self {
        default
    }

    fn parse_env(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" | "" => Some(false),
            _ => None,
        }
    }
}

macro_rules! from_env_parse {
    ($($t:ty),*) => {$(
        impl FromEnv for $t {
            type Default = $t;

            fn from_default(default: $t) -> Self {
                default
            }

            fn parse_env(raw: &str) -> Option<Self> {
                raw.parse().ok()
            }
        }
    )*};
}

from_env_parse!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f32, f64);

impl<T: FromEnv<Default = T>> FromEnv for Option<T> {
    type Default = Option<T>;

    fn from_default(default: Option<T>) -> Self {
        default
    }

    fn parse_env(raw: &str) -> Option<Self> {
        if raw.is_empty() {
            Some(None)
        } else {
            T::parse_env(raw).map(Some)
        }
    }
}

impl FromEnv for Option<String> {
    type Default = Option<&'static str>;

    fn from_default(default: Option<&'static str>) -> Self {
        default.map(str::to_owned)
    }

    fn parse_env(raw: &str) -> Option<Self> {
        Some((!raw.is_empty()).then(|| raw.to_owned()))
    }
}

/// Parse `.env` text: `KEY=value` lines, `#` comments, optional `export ` prefix, single
/// or double quotes (double quotes understand `\n`, `\"` and `\\`), and inline ` #`
/// comments after unquoted values.
pub fn parse_env(text: &str) -> Vec<(String, String)> {
    // A UTF-8 byte order mark (Notepad, PowerShell 5.1 `Out-File`) would glue itself to the first key.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        out.push((key.to_owned(), parse_value(value.trim())));
    }
    out
}

fn parse_value(value: &str) -> String {
    if let Some(rest) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return out,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(other) => out.push(other),
                    None => break,
                },
                c => out.push(c),
            }
        }
        out
    } else if let Some(rest) = value.strip_prefix('\'') {
        rest.split_once('\'')
            .map_or(rest, |(inner, _)| inner)
            .to_owned()
    } else {
        value
            .split_once(" #")
            .map_or(value, |(v, _)| v)
            .trim_end()
            .to_owned()
    }
}

/// The app root directory: `SMELTERY_ROOT` when set, else the current directory.
pub fn root_dir() -> PathBuf {
    match env_value("SMELTERY_ROOT") {
        Some(root) if !root.is_empty() => PathBuf::from(root),
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// The framework's own settings, read from the environment.
///
/// | Key | Default | Field |
/// |---|---|---|
/// | `APP_NAME` | `Smeltery` | `name` |
/// | `APP_ENV` | `production` | `env` |
/// | `APP_DEBUG` | `false` | `debug` |
/// | `APP_URL` | `http://127.0.0.1:8000` | `url` |
/// | `APP_KEY` | empty | `key` |
/// | `SERVER_HOST` | `127.0.0.1` | `host` |
/// | `SERVER_PORT` | `8000` | `port` |
/// | `LOG_LEVEL` | `info` | `log_level` |
/// | `LOG_FILE` (path, relative to the root) | empty (stderr only) | `log_file` |
/// | `LOG_MAX_BYTES` | `10485760` | `log_max_bytes` |
/// | `REQUEST_TIMEOUT` (seconds, at least 1) | `30` | `request_timeout` |
/// | `BODY_LIMIT` (bytes) | `2097152` | `body_limit` |
/// | `UPLOAD_MAX_BYTES` (bytes) | `10485760` | `upload_max_bytes` |
/// | `SHUTDOWN_TIMEOUT` (seconds) | `30` | `shutdown_timeout` |
/// | `TRUSTED_PROXIES` (IPs / CIDR ranges, comma-separated, or `*`) | empty (trust nobody) | `trusted_proxies` |
/// | `STATIC_CACHE_CONTROL` (files from `public/`) | `no-cache` | `static_cache_control` |
/// | `SECURITY_HEADERS` | `true` | `security_headers` |
/// | `FRAME_OPTIONS` (`SAMEORIGIN`, `DENY` or `off`) | `SAMEORIGIN` | `frame_options` |
/// | `HSTS_MAX_AGE` (seconds, sent only when `APP_URL` is https) | `0` (off) | `hsts_max_age` |
/// | `CORS_ALLOWED_ORIGINS` (exact origins, comma-separated; `null` only when listed; see [`cors`](crate::cors)) | empty (off) | `cors_allowed_origins` |
/// | `CORS_PATHS` (path prefixes the CORS rules cover, comma-separated) | `/api/` | `cors_paths` |
/// | `DATABASE_URL` (a relative SQLite path is relative to the root) | empty (no database) | `database_url` |
/// | `DB_POOL_MAX` | `10` | `db_pool_max` |
/// | `DB_CONNECT_TIMEOUT` (seconds) | `5` | `db_connect_timeout` |
/// | `SESSION_DRIVER` | `cookie` | `session_driver` |
/// | `SESSION_LIFETIME` (minutes) | `120` | `session_lifetime` |
/// | `SESSION_COOKIE` | `<app name in snake case>_session` | `session_cookie` |
/// | `AUTH_HOME` | `/dashboard` | `auth_home` |
/// | `AUTH_VERIFICATION_EXPIRE` (minutes, at least 1) | `60` | `verification_expire` |
/// | `AUTH_PASSWORD_TIMEOUT` (seconds a password confirmation lasts, at least 1) | `10800` (3 hours) | `password_timeout` |
/// | `CACHE_STORE` | `database` | `cache_store` |
/// | `CACHE_PREFIX` | `<app name in snake case>_cache_` | `cache_prefix` |
/// | `CACHE_PATH` (file store, relative to the root) | `storage/framework/cache` | `cache_path` |
/// | `CACHE_TABLE` (database store) | `cache` | `cache_table` |
/// | `CACHE_MEMORY_CAPACITY` (entries, memory store) | `10000` | `cache_memory_capacity` |
/// | `CACHE_TIMEOUT` (seconds, database / redis / memcached calls) | `5` | `cache_timeout` |
/// | `CACHE_MAX_VALUE_BYTES` (largest cached value read back) | `16777216` (16 MiB) | `cache_max_value_bytes` |
/// | `REDIS_URL` | `redis://127.0.0.1:6379` | `redis_url` |
/// | `MEMCACHED_SERVERS` (comma-separated `host:port`) | `127.0.0.1:11211` | `memcached_servers` |
/// | `SESSION_ABSOLUTE_LIFETIME` (minutes since the session started or its user signed in; `0` = no limit) | `10080` (7 days) | `session_absolute_lifetime` |
/// | `HASH_CONCURRENCY` (argon2 hashes and checks at once, in the process) | the number of CPUs | `hash_concurrency` |
/// | `HASH_QUEUE` (hashes waiting for a turn before more answer 503) | `64` | `hash_queue` |
/// | `SERVER_HEADER_TIMEOUT` (seconds to send a request's headers; also how long a connection, HTTP/1 or HTTP/2, may stay open with no request running) | `30` | `server_header_timeout` |
/// | `SERVER_MAX_CONNECTIONS` (open connections; more wait to be accepted) | `4096` | `server_max_connections` |
/// | `SERVER_MAX_CONNECTIONS_PER_IP` (open connections from one client address, an IPv6 client by its /64, and requests it runs at once across them; `TRUSTED_PROXIES` are not counted; `0` = no limit) | `128` | `server_max_connections_per_ip` |
/// | `SERVER_MAX_STREAMS` (requests one HTTP/2 connection runs at once; at least 1) | `32` | `server_max_streams` |
/// | `PUBSUB_DRIVER` (`auto`, `local`, `database` or `redis`; see [`crate::pubsub`]) | `auto` | `pubsub_driver` |
/// | `PUBSUB_POLL_MS` (how often the `database` driver reads new messages; at least 10) | `250` | `pubsub_poll_interval` |
#[derive(Clone)]
#[non_exhaustive]
pub struct Settings {
    /// The app name.
    pub name: String,
    /// The environment name, e.g. `local` or `production`.
    pub env: String,
    /// Show error details in responses.
    pub debug: bool,
    /// The public URL of the app.
    pub url: String,
    /// The secret key (`APP_KEY`) signing and encrypting cookies and component state.
    pub key: String,
    /// The address the server binds to.
    pub host: String,
    /// The port the server binds to.
    pub port: u16,
    /// The log level: `trace`, `debug`, `info`, `warn` or `error`.
    pub log_level: String,
    /// The log file (`LOG_FILE`, resolved against [`root`](Self::root)); `None` logs to stderr only.
    pub log_file: Option<PathBuf>,
    /// The size at which the log file is renamed to `<file>.1` and a new one started.
    pub log_max_bytes: u64,
    /// The longest time one request may take before it gets a 408.
    pub request_timeout: Duration,
    /// The largest accepted request body in bytes.
    pub body_limit: usize,
    /// The largest `multipart/form-data` request (files and fields together) that `Valid`
    /// reads; its text fields together stay within `body_limit`.
    pub upload_max_bytes: u64,
    /// The budget for draining requests and background work on shutdown.
    pub shutdown_timeout: Duration,
    /// The proxies whose `X-Forwarded-*` headers are believed (`TRUSTED_PROXIES`, parsed with
    /// [`TrustedProxies::parse`](crate::http::TrustedProxies::parse) when the app builds).
    pub trusted_proxies: String,
    /// The `Cache-Control` header of files served from `public/`.
    pub static_cache_control: String,
    /// Send `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`
    /// and the [`frame_options`](Self::frame_options) headers with every response that does
    /// not set them itself.
    pub security_headers: bool,
    /// Who may show the app's pages in a frame: `SAMEORIGIN` (the app itself), `DENY`
    /// (nobody) or `off` (no header). Sent as `X-Frame-Options` and as
    /// `Content-Security-Policy: frame-ancestors`; any other value stops the build.
    pub frame_options: String,
    /// The `max-age` of `Strict-Transport-Security`; zero sends no header, and the header is
    /// only sent when [`url`](Self::url) is https.
    pub hsts_max_age: Duration,
    /// The origins whose pages and apps may call [`cors_paths`](Self::cors_paths) cross-origin (exact
    /// `scheme://host[:port]`, comma-separated; never with credentials). Empty: no CORS headers at all.
    pub cors_allowed_origins: String,
    /// The path prefixes the CORS rules cover (comma-separated).
    pub cors_paths: String,
    /// The app root directory (see [`root_dir`]).
    pub root: PathBuf,
    /// The database URL (`sqlite://database/database.sqlite`, `postgres://…`, `mysql://…`);
    /// empty means the app has no database. A relative SQLite path is resolved against
    /// [`root`](Self::root) when the app connects.
    pub database_url: String,
    /// The most connections in the database pool.
    pub db_pool_max: u32,
    /// How long connecting to the database, or waiting for a free pool connection, may take.
    pub db_connect_timeout: Duration,
    /// Where sessions live: `cookie` (in an encrypted cookie), `database` (the `sessions` table)
    /// or `file` (`storage/framework/sessions/`).
    pub session_driver: String,
    /// How long an idle session lives.
    pub session_lifetime: Duration,
    /// The session cookie's name.
    pub session_cookie: String,
    /// Where the `guest` middleware sends signed-in users.
    pub auth_home: String,
    /// How long an email verification link is valid
    /// ([`crate::auth::verification`]).
    pub verification_expire: Duration,
    /// How long a password confirmation lasts for the `password.confirm` middleware
    /// ([`Auth::confirm_password`](crate::auth::Auth::confirm_password)).
    pub password_timeout: Duration,
    /// The default cache store: `database`, `file`, `memory`, `redis`, `memcached`, `array` or
    /// `null` (see [`crate::cache`]).
    pub cache_store: String,
    /// Put in front of every cache key and lock name, so apps sharing a store stay apart.
    pub cache_prefix: String,
    /// The file store's directory (`CACHE_PATH`, resolved against [`root`](Self::root)).
    pub cache_path: PathBuf,
    /// The database store's table; its locks live in `<table>_locks`.
    pub cache_table: String,
    /// The most entries the memory store keeps.
    pub cache_memory_capacity: u64,
    /// How long one call to the database, Redis or memcached store may take.
    pub cache_timeout: Duration,
    /// The largest cached value (JSON text, bytes) the cache reads back; a larger one is an error for `get` and a
    /// miss for `remember`. The file, database and Redis stores check the size before reading the value.
    pub cache_max_value_bytes: usize,
    /// The Redis server of the `redis` cache store (`redis://` or `rediss://` for TLS).
    pub redis_url: String,
    /// The memcached servers of the `memcached` cache store, comma-separated `host:port`.
    pub memcached_servers: String,

    // Sessions, password hashing and the server: security limits.
    /// The longest a session lives, counted from when it started or its user signed in,
    /// however active it is; zero means no limit. A signed-in user is then signed out (a
    /// remember-me cookie signs them in again).
    pub session_absolute_lifetime: Duration,
    /// The most argon2 password hashes and checks running at once in the process (the first
    /// app built in a process sets it).
    pub hash_concurrency: usize,
    /// The most password hashes waiting for a turn; beyond that a hash fails with 503.
    pub hash_queue: usize,
    /// How long a client may take to send a request's headers, and the longest a connection
    /// (HTTP/1 or HTTP/2) may stay open with no request running.
    pub server_header_timeout: Duration,
    /// The most connections the server holds open at once; further clients wait to be
    /// accepted.
    pub server_max_connections: usize,
    /// The most connections one client address (an IPv6 client by its /64) holds open at once;
    /// a further connection from it is closed at once. Peers listed in `TRUSTED_PROXIES` are
    /// not counted. Zero means no limit. Also the most requests one client address runs at once
    /// across its connections (HTTP/2 multiplexes requests); a further one gets 429.
    pub server_max_connections_per_ip: usize,
    /// The most requests (streams) one HTTP/2 connection runs at once (at least 1).
    pub server_max_streams: u32,

    // PubSub: messages between the processes of one app.
    /// How messages reach the app's other processes: `auto`, `local`, `database` or `redis` (see
    /// [`crate::pubsub`]); any other value fails the build.
    pub pubsub_driver: String,
    /// How often the `database` driver reads the messages of the other processes.
    pub pubsub_poll_interval: Duration,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key is a secret: never print it.
        f.debug_struct("Settings")
            .field("name", &self.name)
            .field("env", &self.env)
            .field("debug", &self.debug)
            .field("url", &redact_url(&self.url))
            .field("key", &"<redacted>")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("log_level", &self.log_level)
            .field("log_file", &self.log_file)
            .field("log_max_bytes", &self.log_max_bytes)
            .field("request_timeout", &self.request_timeout)
            .field("body_limit", &self.body_limit)
            .field("upload_max_bytes", &self.upload_max_bytes)
            .field("shutdown_timeout", &self.shutdown_timeout)
            .field("trusted_proxies", &self.trusted_proxies)
            .field("static_cache_control", &self.static_cache_control)
            .field("security_headers", &self.security_headers)
            .field("frame_options", &self.frame_options)
            .field("hsts_max_age", &self.hsts_max_age)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("cors_paths", &self.cors_paths)
            .field("root", &self.root)
            // The URL may hold a password: only its scheme is shown.
            .field(
                "database_url",
                &self.database_url.split(':').next().unwrap_or_default(),
            )
            .field("db_pool_max", &self.db_pool_max)
            .field("db_connect_timeout", &self.db_connect_timeout)
            .field("session_driver", &self.session_driver)
            .field("session_lifetime", &self.session_lifetime)
            .field("session_cookie", &self.session_cookie)
            .field("auth_home", &self.auth_home)
            .field("verification_expire", &self.verification_expire)
            .field("password_timeout", &self.password_timeout)
            .field("cache_store", &self.cache_store)
            .field("cache_prefix", &self.cache_prefix)
            .field("cache_path", &self.cache_path)
            .field("cache_table", &self.cache_table)
            .field("cache_memory_capacity", &self.cache_memory_capacity)
            .field("cache_timeout", &self.cache_timeout)
            .field("cache_max_value_bytes", &self.cache_max_value_bytes)
            // The URL may hold a password: only its scheme is shown.
            .field(
                "redis_url",
                &self.redis_url.split(':').next().unwrap_or_default(),
            )
            // `user:password@host` entries (SASL) keep their hosts only.
            .field(
                "memcached_servers",
                &self
                    .memcached_servers
                    .split(',')
                    .map(|s| redact_url(s.trim()))
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .field("session_absolute_lifetime", &self.session_absolute_lifetime)
            .field("hash_concurrency", &self.hash_concurrency)
            .field("hash_queue", &self.hash_queue)
            .field("server_header_timeout", &self.server_header_timeout)
            .field("server_max_connections", &self.server_max_connections)
            .field(
                "server_max_connections_per_ip",
                &self.server_max_connections_per_ip,
            )
            .field("server_max_streams", &self.server_max_streams)
            .field("pubsub_driver", &self.pubsub_driver)
            .field("pubsub_poll_interval", &self.pubsub_poll_interval)
            .finish()
    }
}

impl Settings {
    /// Read every setting from the environment (see the table above).
    pub fn from_env() -> Self {
        let name: String = env("APP_NAME", "Smeltery");
        let default_cookie = format!("{}_session", snake(&name));
        let root = root_dir();
        let log_file: String = env("LOG_FILE", "");
        let log_file = (!log_file.trim().is_empty()).then(|| root.join(log_file.trim()));
        Self {
            session_driver: env("SESSION_DRIVER", "cookie"),
            session_lifetime: Duration::from_secs(
                env::<u64>("SESSION_LIFETIME", 120).saturating_mul(60),
            ),
            session_cookie: env::<Option<String>>("SESSION_COOKIE", None).unwrap_or(default_cookie),
            auth_home: env("AUTH_HOME", "/dashboard"),
            verification_expire: Duration::from_secs(
                env::<u64>("AUTH_VERIFICATION_EXPIRE", 60)
                    .max(1)
                    .saturating_mul(60),
            ),
            password_timeout: Duration::from_secs(
                env::<u64>("AUTH_PASSWORD_TIMEOUT", 10_800).max(1),
            ),
            cache_store: env("CACHE_STORE", "database"),
            cache_prefix: env::<Option<String>>("CACHE_PREFIX", None)
                .unwrap_or_else(|| format!("{}_cache_", snake(&name))),
            cache_path: root.join(env::<String>("CACHE_PATH", "storage/framework/cache").trim()),
            cache_table: env("CACHE_TABLE", "cache"),
            cache_memory_capacity: env("CACHE_MEMORY_CAPACITY", 10_000),
            cache_timeout: Duration::from_secs(env::<u64>("CACHE_TIMEOUT", 5).max(1)),
            cache_max_value_bytes: env::<usize>(
                "CACHE_MAX_VALUE_BYTES",
                crate::cache::DEFAULT_MAX_VALUE_BYTES,
            )
            .max(1),
            redis_url: env("REDIS_URL", "redis://127.0.0.1:6379"),
            memcached_servers: env("MEMCACHED_SERVERS", "127.0.0.1:11211"),
            name,
            // A missing APP_ENV is production: a forgotten line never turns on development behaviour (logged
            // links and mail bodies, the test key). New apps' `.env` sets `local` explicitly.
            env: env("APP_ENV", "production"),
            debug: env("APP_DEBUG", false),
            url: env("APP_URL", "http://127.0.0.1:8000"),
            key: env("APP_KEY", ""),
            host: env("SERVER_HOST", "127.0.0.1"),
            port: env("SERVER_PORT", 8000),
            log_level: env("LOG_LEVEL", "info"),
            log_file,
            log_max_bytes: env::<u64>("LOG_MAX_BYTES", 10 * 1024 * 1024).max(1),
            // `0` would answer every request 408 at once.
            request_timeout: Duration::from_secs(env::<u64>("REQUEST_TIMEOUT", 30).max(1)),
            body_limit: env("BODY_LIMIT", 2 * 1024 * 1024),
            upload_max_bytes: env("UPLOAD_MAX_BYTES", 10 * 1024 * 1024),
            shutdown_timeout: Duration::from_secs(env("SHUTDOWN_TIMEOUT", 30)),
            trusted_proxies: env("TRUSTED_PROXIES", ""),
            static_cache_control: env("STATIC_CACHE_CONTROL", "no-cache"),
            security_headers: env("SECURITY_HEADERS", true),
            frame_options: env("FRAME_OPTIONS", "SAMEORIGIN"),
            hsts_max_age: Duration::from_secs(env("HSTS_MAX_AGE", 0)),
            cors_allowed_origins: env("CORS_ALLOWED_ORIGINS", ""),
            cors_paths: env("CORS_PATHS", "/api/"),
            root,
            database_url: env("DATABASE_URL", ""),
            db_pool_max: env("DB_POOL_MAX", 10),
            db_connect_timeout: Duration::from_secs(env("DB_CONNECT_TIMEOUT", 5)),
            session_absolute_lifetime: Duration::from_secs(
                env::<u64>("SESSION_ABSOLUTE_LIFETIME", 7 * 24 * 60).saturating_mul(60),
            ),
            hash_concurrency: env::<usize>("HASH_CONCURRENCY", default_hash_concurrency()).max(1),
            hash_queue: env::<usize>("HASH_QUEUE", DEFAULT_HASH_QUEUE),
            server_header_timeout: Duration::from_secs(
                env::<u64>("SERVER_HEADER_TIMEOUT", 30).max(1),
            ),
            server_max_connections: env::<usize>("SERVER_MAX_CONNECTIONS", 4096).max(1),
            server_max_connections_per_ip: env("SERVER_MAX_CONNECTIONS_PER_IP", 128),
            server_max_streams: env::<u32>("SERVER_MAX_STREAMS", 32).max(1),
            pubsub_driver: env::<String>("PUBSUB_DRIVER", "auto")
                .trim()
                .to_ascii_lowercase(),
            pubsub_poll_interval: Duration::from_millis(env::<u64>("PUBSUB_POLL_MS", 250).max(10)),
        }
    }

    /// The directory served as static files: `<root>/public`.
    pub fn public_dir(&self) -> PathBuf {
        self.root.join("public")
    }

    /// The Mold views directory: `<root>/resources/views`.
    pub fn views_dir(&self) -> PathBuf {
        self.root.join("resources").join("views")
    }

    /// The storage directory: `<root>/storage`.
    pub fn storage_dir(&self) -> PathBuf {
        self.root.join("storage")
    }

    /// Whether cookies get the `Secure` flag (and the session and remember-me cookies the
    /// `__Host-` prefix): `APP_URL` starts with `https://`, in any letter case.
    pub fn secure_cookies(&self) -> bool {
        self.url
            .get(..8)
            .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
    }

    /// Whether the app runs in production (`APP_ENV=production`).
    pub fn is_production(&self) -> bool {
        self.env == "production"
    }

    /// Local development: `APP_ENV` is `local` or `testing` **and** `APP_URL` points at this machine
    /// (`localhost`, `*.localhost` or a loopback address; see [`loopback_url`]).
    ///
    /// Only then does the framework write secrets to the log: password-reset and verification links without a
    /// mailer, and the bodies of mails sent with `MAIL_MAILER=log`. A server sets its public `APP_URL` (links in
    /// mails need it), so a wrong `APP_ENV` there still keeps those secrets out of the log.
    ///
    /// ```
    /// use smeltery_core::config::Settings;
    ///
    /// let mut settings = Settings::from_env();
    /// settings.env = "local".into();
    /// settings.url = "http://127.0.0.1:8000".into();
    /// assert!(settings.is_local_development());
    /// settings.url = "https://app.example.com".into();
    /// assert!(!settings.is_local_development());
    /// ```
    pub fn is_local_development(&self) -> bool {
        matches!(self.env.as_str(), "local" | "testing") && loopback_url(&self.url)
    }

    /// Whether the app signs and encrypts with the framework's public test key: `APP_ENV=testing` **and** an empty
    /// `APP_KEY`. A malformed or short `APP_KEY` is never replaced by the test key (it is an error instead), and
    /// `serve` / `work` refuse to run under `APP_ENV=testing`.
    ///
    /// ```
    /// use smeltery_core::config::Settings;
    ///
    /// let mut settings = Settings::from_env();
    /// settings.env = "testing".into();
    /// settings.key = String::new();
    /// assert!(settings.uses_test_key());
    /// settings.key = "short".into();
    /// assert!(!settings.uses_test_key());
    /// settings.env = "local".into();
    /// settings.key = String::new();
    /// assert!(!settings.uses_test_key());
    /// ```
    pub fn uses_test_key(&self) -> bool {
        self.env == "testing" && self.key.trim().is_empty()
    }

    /// Whether the app has a key to sign and encrypt with (sessions, cookies, signed URLs, Spark state): a usable
    /// `APP_KEY` (see [`app_key_bytes`]) or the public test key ([`Settings::uses_test_key`]).
    ///
    /// ```
    /// use smeltery_core::config::Settings;
    ///
    /// let mut settings = Settings::from_env();
    /// settings.env = "local".into();
    /// settings.key = "0123456789abcdef0123456789abcdef".into();
    /// assert!(settings.has_signing_key());
    /// settings.key = "short".into();
    /// assert!(!settings.has_signing_key());
    /// ```
    pub fn has_signing_key(&self) -> bool {
        app_key_bytes(&self.key).is_some() || self.uses_test_key()
    }
}

/// The key bytes of an `APP_KEY` value: the decoded data of `base64:<data>`, or the text itself; `None` when the
/// base64 is malformed or the key is shorter than 32 bytes. Everything the framework signs or encrypts derives its
/// keys from these bytes.
///
/// ```
/// use smeltery_core::config::app_key_bytes;
///
/// assert_eq!(app_key_bytes("0123456789abcdef0123456789abcdef").map(|k| k.len()), Some(32));
/// assert!(app_key_bytes("short").is_none());
/// assert!(app_key_bytes("base64:not*base64").is_none());
/// ```
pub fn app_key_bytes(app_key: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let bytes = match app_key.strip_prefix("base64:") {
        Some(encoded) => base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()?,
        None => app_key.as_bytes().to_vec(),
    };
    (bytes.len() >= 32).then_some(bytes)
}

/// Whether `url`'s host is `localhost` (or `*.localhost`) or a loopback address.
///
/// ```
/// use smeltery_core::config::loopback_url;
///
/// assert!(loopback_url("http://127.0.0.1:8000"));
/// assert!(loopback_url("http://[::1]:8000/path"));
/// assert!(loopback_url("http://shop.localhost"));
/// assert!(!loopback_url("https://app.example.com"));
/// assert!(!loopback_url("not a url"));
/// ```
pub fn loopback_url(url: &str) -> bool {
    let Ok(uri) = url.trim().parse::<http::Uri>() else {
        return false;
    };
    uri.host().is_some_and(loopback_host)
}

/// Whether `host` (a name, an IP address, or an IPv6 address with or without brackets) is `localhost`,
/// `*.localhost` or a loopback address.
///
/// ```
/// use smeltery_core::config::loopback_host;
///
/// assert!(loopback_host("127.0.0.1") && loopback_host("::1") && loopback_host("[::1]"));
/// assert!(loopback_host("localhost") && !loopback_host("smtp.example.com"));
/// ```
pub fn loopback_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    let lower = host.to_ascii_lowercase();
    lower == "localhost"
        || lower.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// `url` with its user name and password replaced by `***` (`postgres://***@db/app`). Everything up to the
/// last `@` counts as credentials, so a password with an unescaped `/`, `?`, `#` or `@` is hidden too. A
/// value without `@` is returned as it is.
pub(crate) fn redact_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url),
    };
    let Some(at) = rest.rfind('@') else {
        return url.to_owned();
    };
    let host = rest.get(at + 1..).unwrap_or_default();
    match scheme {
        Some(scheme) => format!("{scheme}://***@{host}"),
        None => format!("***@{host}"),
    }
}

/// `text` (an error message) with every copy of `url`, of its credentials and of its password replaced, so a
/// driver error that quotes the connection string never shows the password.
pub(crate) fn redact_url_in(text: &str, url: &str) -> String {
    let mut out = text.replace(url, &redact_url(url));
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    if let Some(at) = rest.rfind('@') {
        let userinfo = rest.get(..at).unwrap_or_default();
        let password = userinfo.split_once(':').map_or("", |(_, p)| p);
        let decoded = percent_decode(password);
        // Short values would mask ordinary words of the message; the whole URL is replaced above.
        for secret in [userinfo, password, decoded.as_str()] {
            if secret.len() >= 4 {
                out = out.replace(secret, "***");
            }
        }
    }
    out
}

/// `%XX` sequences decoded (invalid ones kept as they are).
pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b, hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The default of `HASH_CONCURRENCY`: the number of CPUs.
pub(crate) fn default_hash_concurrency() -> usize {
    std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get)
}

/// The default of `HASH_QUEUE`.
pub(crate) const DEFAULT_HASH_QUEUE: usize = 64;

/// `My App` → `my_app`: lowercase ASCII letters and digits, everything else one `_`.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_owned();
    if out.is_empty() {
        "smeltery".to_owned()
    } else {
        out
    }
}

/// Handler argument giving a config struct registered with
/// [`AppBuilder::config`](crate::AppBuilder::config).
///
/// ```
/// use smeltery_core::config::Config;
///
/// #[derive(Clone)]
/// struct MailConfig {
///     from: String,
/// }
///
/// async fn show(mail: Config<MailConfig>) -> String {
///     mail.from.clone()
/// }
/// ```
///
/// A config type that was never registered is a 500 error with the type name in the log.
#[derive(Debug)]
pub struct Config<T>(pub Arc<T>);

impl<T> Clone for Config<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> std::ops::Deref for Config<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Send + Sync + 'static> axum::extract::FromRequestParts<App> for Config<T> {
    type Rejection = Error;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> Result<Self, Self::Rejection> {
        app.config::<T>().map(Config).ok_or_else(|| {
            Error::internal(format!(
                "config `{}` is not registered",
                std::any::type_name::<T>()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_text() {
        let text = r#"
# comment
APP_NAME=My App
export APP_ENV=production
QUOTED="a \"b\"\nc"
SINGLE='x # y'
INLINE=value # trailing
EMPTY=
 SPACED = v
bad line
1-BAD=x
"#;
        let parsed: HashMap<_, _> = parse_env(text).into_iter().collect();
        assert_eq!(parsed["APP_NAME"], "My App");
        assert_eq!(parsed["APP_ENV"], "production");
        assert_eq!(parsed["QUOTED"], "a \"b\"\nc");
        assert_eq!(parsed["SINGLE"], "x # y");
        assert_eq!(parsed["INLINE"], "value");
        assert_eq!(parsed["EMPTY"], "");
        assert_eq!(parsed["SPACED"], "v");
        assert_eq!(parsed.len(), 7);
    }

    #[test]
    fn a_leading_utf8_bom_does_not_hide_the_first_key() {
        // Notepad and PowerShell 5.1 `Out-File` write a BOM first.
        let parsed: HashMap<_, _> = parse_env("\u{feff}APP_NAME=Shop\r\nAPP_ENV=production\r\n")
            .into_iter()
            .collect();
        assert_eq!(parsed.get("APP_NAME").map(String::as_str), Some("Shop"));
        assert_eq!(parsed["APP_ENV"], "production");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, b"\xEF\xBB\xBFSMELTERY_TEST_BOM_KEY=7\n").unwrap();
        assert!(load_env_file(&path).unwrap());
        assert_eq!(env::<u32>("SMELTERY_TEST_BOM_KEY", 0), 7);
    }

    #[test]
    fn env_reads_typed_values_and_falls_back() {
        set_env_value("SMELTERY_TEST_PORT", "9000");
        set_env_value("SMELTERY_TEST_BAD_PORT", "nope");
        set_env_value("SMELTERY_TEST_FLAG", "yes");
        set_env_value("SMELTERY_TEST_OPT", "");
        let port: u16 = env("SMELTERY_TEST_PORT", 8000);
        let bad: u16 = env("SMELTERY_TEST_BAD_PORT", 8000);
        let flag: bool = env("SMELTERY_TEST_FLAG", false);
        let opt: Option<String> = env("SMELTERY_TEST_OPT", Some("x"));
        let missing: Option<u32> = env("SMELTERY_TEST_MISSING", None);
        assert_eq!(
            (port, bad, flag, opt, missing),
            (9000, 8000, true, None, None)
        );
    }

    #[test]
    fn a_zero_verification_expiry_counts_as_one_minute() {
        // No other lib test reads `verification_expire` from the environment.
        set_env_value("AUTH_VERIFICATION_EXPIRE", "0");
        assert_eq!(
            Settings::from_env().verification_expire,
            Duration::from_secs(60)
        );
        set_env_value("AUTH_VERIFICATION_EXPIRE", "15");
        assert_eq!(
            Settings::from_env().verification_expire,
            Duration::from_secs(15 * 60)
        );
    }

    #[test]
    fn load_env_file_missing_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!load_env_file(dir.path().join(".env")).unwrap());
        std::fs::write(dir.path().join(".env"), "SMELTERY_TEST_FILE_KEY=42\n").unwrap();
        assert!(load_env_file(dir.path().join(".env")).unwrap());
        assert_eq!(env::<u32>("SMELTERY_TEST_FILE_KEY", 0), 42);
    }

    #[test]
    fn session_cookie_name_is_the_snake_app_name() {
        assert_eq!(snake("My Cool App!"), "my_cool_app");
        assert_eq!(snake("--"), "smeltery");
        assert_eq!(snake("Smeltery"), "smeltery");
    }

    #[test]
    fn settings_debug_hides_the_key() {
        let mut s = Settings::from_env();
        s.key = "secret-value".into();
        s.database_url = "postgres://user:hunter2@db/app".into();
        let debug = format!("{s:?}");
        assert!(!debug.contains("secret-value"));
        assert!(!debug.contains("hunter2") && debug.contains("\"postgres\""));
    }

    #[test]
    fn settings_debug_hides_credentials_in_every_url() {
        let mut s = Settings::from_env();
        s.memcached_servers = "mc_user:MC_SECRET@10.0.0.5:11211, 10.0.0.6:11211".into();
        s.url = "https://admin:APP_URL_SECRET@app.example.com".into();
        let debug = format!("{s:?}");
        assert!(
            !debug.contains("MC_SECRET") && !debug.contains("mc_user"),
            "{debug}"
        );
        assert!(
            debug.contains("***@10.0.0.5:11211,10.0.0.6:11211"),
            "{debug}"
        );
        assert!(!debug.contains("APP_URL_SECRET"), "{debug}");
        assert!(debug.contains("https://***@app.example.com"), "{debug}");
    }

    #[test]
    fn a_missing_app_env_means_production() {
        // The process environment or another test's `.env` value would win: only check a clean process.
        if env_value("APP_ENV").is_some() {
            return;
        }
        let s = Settings::from_env();
        assert_eq!(s.env, "production");
        assert!(s.is_production());
        assert!(!s.debug);
    }

    #[test]
    fn local_development_needs_a_loopback_url() {
        let mut s = Settings::from_env();
        for (env, url, local) in [
            ("local", "http://127.0.0.1:8000", true),
            ("testing", "http://localhost:8000", true),
            ("local", "http://[::1]:8000", true),
            ("local", "http://shop.localhost", true),
            ("local", "https://app.example.com", false),
            ("production", "http://127.0.0.1:8000", false),
            ("staging", "http://127.0.0.1:8000", false),
            ("local", "", false),
        ] {
            s.env = env.into();
            s.url = url.into();
            assert_eq!(s.is_local_development(), local, "{env} {url}");
        }
    }

    #[test]
    fn redaction_hides_passwords_in_urls_and_messages() {
        assert_eq!(
            redact_url("postgres://u:pw@db/app"),
            "postgres://***@db/app"
        );
        assert_eq!(
            redact_url("mysql://app:S3CRET/PW@127.0.0.1:1/db"),
            "mysql://***@127.0.0.1:1/db"
        );
        assert_eq!(
            redact_url("sqlite://database/x.sqlite"),
            "sqlite://database/x.sqlite"
        );
        let url = "mysql://app:p%40ss-W0RD@127.0.0.1:1/db";
        let message =
            format!("The connection string '{url}' cannot be parsed; user app, password p@ss-W0RD");
        let clean = redact_url_in(&message, url);
        assert!(
            !clean.contains("p%40ss") && !clean.contains("p@ss-W0RD"),
            "{clean}"
        );
        assert!(clean.contains("mysql://***@127.0.0.1:1/db"), "{clean}");
    }
}
