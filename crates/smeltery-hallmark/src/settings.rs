//! Hallmark's settings (`HALLMARK_*` in `.env`), built with [`Hallmark`].

use std::time::Duration;

use smeltery_core::config::env;

/// One day.
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The longest expiration accepted (100 years); longer values are clamped to it.
const MAX_EXPIRATION_DAYS: u64 = 36_500;

/// The largest per-user token cap accepted.
const MAX_TOKENS_LIMIT: u32 = 10_000;

/// The largest guess budget accepted (per client and minute).
const MAX_GUESS_LIMIT: u32 = 100_000;

/// Hallmark's settings: read from `.env` by [`Hallmark::new`], changed with the builder methods, installed with
/// [`HallmarkExt::hallmark`](crate::HallmarkExt::hallmark).
///
/// | `.env` | Builder | Default | Meaning |
/// |---|---|---|---|
/// | `HALLMARK_TOKEN_EXPIRATION` | [`expiration`](Self::expiration) | `365` (days; `0` = never) | a new token's expiry, and the most a token lives counted from its creation, checked on every request |
/// | `HALLMARK_MAX_TOKENS_PER_USER` | [`max_tokens_per_user`](Self::max_tokens_per_user) | `100` | creating one more deletes the user's least recently used token |
/// | `HALLMARK_GUESS_LIMIT` | [`guess_limit`](Self::guess_limit) | `60` | invalid bearer tokens one client may send a minute before it gets 429 |
/// | `HALLMARK_SPA` | [`spa`](Self::spa) | `false` | same-origin SPA mode: the session authenticates first-party requests on `auth:hallmark` routes |
/// | `HALLMARK_STATEFUL` | [`stateful`](Self::stateful) | empty | first-party origins besides `APP_URL`'s, comma-separated |
///
/// ```
/// use std::time::Duration;
/// use smeltery::hallmark::Hallmark;
///
/// let hallmark = Hallmark::new()
///     .expiration(Duration::from_secs(30 * 24 * 3600))
///     .max_tokens_per_user(20);
/// assert_eq!(hallmark.max_age(), Some(Duration::from_secs(30 * 24 * 3600)));
/// assert_eq!(hallmark.tokens_per_user(), 20);
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct Hallmark {
    expiration: Option<Duration>,
    max_tokens_per_user: u32,
    guess_limit: u32,
    spa: bool,
    stateful: Vec<String>,
}

impl std::fmt::Debug for Hallmark {
    // Hand-written like every settings type (SECURITY.md §3.5), although nothing here is secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hallmark")
            .field("expiration", &self.expiration)
            .field("max_tokens_per_user", &self.max_tokens_per_user)
            .field("guess_limit", &self.guess_limit)
            .field("spa", &self.spa)
            .field("stateful", &self.stateful)
            .finish()
    }
}

impl Default for Hallmark {
    fn default() -> Self {
        Self::new()
    }
}

impl Hallmark {
    /// The settings from `.env` (and the process environment). Values out of range are clamped, with a warning
    /// naming the key.
    pub fn new() -> Self {
        let days = clamp(
            "HALLMARK_TOKEN_EXPIRATION",
            env::<u64>("HALLMARK_TOKEN_EXPIRATION", 365),
            0,
            MAX_EXPIRATION_DAYS,
        );
        Self {
            expiration: (days > 0).then(|| DAY.saturating_mul(days_u32(days))),
            max_tokens_per_user: clamp(
                "HALLMARK_MAX_TOKENS_PER_USER",
                env::<u32>("HALLMARK_MAX_TOKENS_PER_USER", 100),
                1,
                MAX_TOKENS_LIMIT,
            ),
            guess_limit: clamp(
                "HALLMARK_GUESS_LIMIT",
                env::<u32>("HALLMARK_GUESS_LIMIT", 60),
                1,
                MAX_GUESS_LIMIT,
            ),
            spa: env::<bool>("HALLMARK_SPA", false),
            stateful: split_origins(&env::<String>("HALLMARK_STATEFUL", "")),
        }
    }

    /// Turn on the same-origin SPA mode: API routes with `auth:hallmark` also accept the session cookie of a
    /// first-party request (`Sec-Fetch-Site: same-origin`, or without that header an `Origin` / `Referer` origin
    /// equal to `APP_URL`'s or listed with [`stateful`](Self::stateful)), with the CSRF check of web routes;
    /// `GET /hallmark/csrf-cookie` and the `XSRF-TOKEN` cookie are added. `HALLMARK_SPA=true` does the same.
    #[must_use]
    pub fn spa(mut self) -> Self {
        self.spa = true;
        self
    }

    /// More first-party origins for SPA mode besides `APP_URL`'s (`scheme://host[:port]`, e.g. a development
    /// server on `http://localhost:5173`); replaces `HALLMARK_STATEFUL`. An entry that is not an origin (`*`,
    /// `null`, a path) stops the app at boot.
    #[must_use]
    pub fn stateful(mut self, origins: &[&str]) -> Self {
        self.stateful = origins.iter().map(|o| (*o).trim().to_owned()).collect();
        self
    }

    /// Whether SPA mode is on.
    pub fn is_spa(&self) -> bool {
        self.spa
    }

    /// The listed first-party origins (besides `APP_URL`'s).
    pub fn stateful_origins(&self) -> &[String] {
        &self.stateful
    }

    /// How long tokens live: a new token's default expiry, and the most any token lives counted from its creation
    /// (lowering it shortens tokens issued before). `Duration::ZERO` means tokens never expire by age. Clamped to
    /// at least a second and at most 100 years.
    #[must_use]
    pub fn expiration(mut self, expiration: Duration) -> Self {
        self.expiration = (!expiration.is_zero()).then(|| {
            expiration
                .max(Duration::from_secs(1))
                .min(DAY.saturating_mul(days_u32(MAX_EXPIRATION_DAYS)))
        });
        self
    }

    /// The most tokens one user holds (at least 1, at most 10,000): creating one more deletes the user's least
    /// recently used token.
    #[must_use]
    pub fn max_tokens_per_user(mut self, max: u32) -> Self {
        self.max_tokens_per_user = max.clamp(1, MAX_TOKENS_LIMIT);
        self
    }

    /// Invalid bearer tokens (malformed, unknown, expired, revoked) one client may send in a minute before every
    /// further request with a bearer token gets 429 for the rest of the minute (at least 1).
    #[must_use]
    pub fn guess_limit(mut self, limit: u32) -> Self {
        self.guess_limit = limit.clamp(1, MAX_GUESS_LIMIT);
        self
    }

    /// The most a token lives, `None` when tokens never expire by age.
    pub fn max_age(&self) -> Option<Duration> {
        self.expiration
    }

    /// The per-user token cap.
    pub fn tokens_per_user(&self) -> u32 {
        self.max_tokens_per_user
    }

    /// The invalid bearer tokens a client may send a minute.
    pub fn guesses_per_minute(&self) -> u32 {
        self.guess_limit
    }
}

/// The comma-separated entries of `HALLMARK_STATEFUL`.
fn split_origins(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `days` as a `u32` (it is at most [`MAX_EXPIRATION_DAYS`]).
fn days_u32(days: u64) -> u32 {
    u32::try_from(days).unwrap_or(u32::MAX)
}

/// `value` within `low..=high`, with a warning naming `key` when it was outside.
fn clamp<T: Ord + Copy>(key: &str, value: T, low: T, high: T) -> T {
    let clamped = value.clamp(low, high);
    if clamped != value {
        tracing::warn!(key, "value outside its range, using the nearest bound");
    }
    clamped
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_defaults_and_the_bounds() {
        let h = Hallmark::new();
        assert_eq!(h.max_age(), Some(DAY * 365));
        assert_eq!(h.tokens_per_user(), 100);
        assert_eq!(h.guesses_per_minute(), 60);
        let h = h
            .expiration(Duration::ZERO)
            .max_tokens_per_user(0)
            .guess_limit(0);
        assert_eq!(h.max_age(), None);
        assert_eq!(h.tokens_per_user(), 1);
        assert_eq!(h.guesses_per_minute(), 1);
        let h = h
            .expiration(Duration::from_millis(1))
            .max_tokens_per_user(u32::MAX);
        assert_eq!(h.max_age(), Some(Duration::from_secs(1)));
        assert_eq!(h.tokens_per_user(), MAX_TOKENS_LIMIT);
        let h = h.expiration(Duration::MAX);
        assert_eq!(h.max_age(), Some(DAY * 36_500));
        let debug = format!("{h:?}");
        assert!(debug.contains("max_tokens_per_user"), "{debug}");
    }
}
