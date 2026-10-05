//! The [`RateLimiter`] (at most `max` hits per key in a fixed window, counted in the app's cache so every process
//! of the app shares the count) and the `throttle:<max>,<minutes>[,<field>]` route middleware built on it: at most
//! `max` requests per `minutes` per client on one route (`throttle:5,1`).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};

use crate::app::App;
use crate::auth::{Auth, Principal, Throttle, throttle_ip};
use crate::error::{Error, Result};
use crate::middleware::{ErasedMiddleware, Next, Request};
use crate::session::Session;

/// The alias prefix.
const PREFIX: &str = "throttle:";

/// The field a web form's error goes on when the alias names none.
const DEFAULT_FIELD: &str = "email";

/// The outcome of [`RateLimiter::hit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RateLimit {
    /// Counted and let through; this many hits are left in the window.
    #[non_exhaustive]
    Allowed {
        /// Hits left in this window.
        remaining: u32,
    },
    /// Over the limit (this hit is refused); the window ends in this many seconds (at least 1).
    #[non_exhaustive]
    Limited {
        /// Seconds until the window ends.
        retry_after: u64,
    },
}

impl RateLimit {
    /// Whether the hit was let through.
    pub fn allowed(self) -> bool {
        matches!(self, Self::Allowed { .. })
    }
}

/// At most `max` hits per key in fixed windows of `window`, counted in the app's cache store (`CACHE_STORE`), so
/// every process sharing the store shares the counts. Each hit is one atomic `add` + `increment`, so a burst of
/// parallel hits never gets more than `max` through. A store error fails the hit (fail closed: the caller refuses
/// what it guards). Without a cache store (`CACHE_STORE=null`) the counts live in the app's process memory. Either
/// way limiters with the same name, `max` and `window` share their counts, so a limiter built per call counts like
/// one kept for the app's life. The `throttle:` middleware is built on it.
///
/// Count before the expensive or guarded work, and count unknown and known keys alike when the answer must not
/// reveal which exist.
///
/// ```
/// use std::time::Duration;
/// use std::sync::LazyLock;
/// use smeltery_core::App;
/// use smeltery_core::cache::{RateLimit, RateLimiter};
///
/// static CODES: LazyLock<RateLimiter> =
///     LazyLock::new(|| RateLimiter::new("codes", 5, Duration::from_secs(300)));
///
/// async fn guarded(app: &App, user_id: i64) -> smeltery_core::Result<bool> {
///     let hit = CODES.hit(app, &format!("user:{user_id}")).await?;
///     if let RateLimit::Limited { retry_after, .. } = hit {
///         tracing::info!(retry_after, "too many codes");
///     }
///     Ok(hit.allowed())
/// }
/// # let _ = guarded;
/// ```
pub struct RateLimiter {
    name: String,
    max: u32,
    window: Duration,
    /// The count outside an app's request stack (never in an app): this limiter's memory.
    local: Throttle,
}

/// The counts of every [`RateLimiter`] of an app without a cache store, by name, `max` and window.
#[derive(Default)]
struct MemoryCounts(std::sync::Mutex<std::collections::HashMap<String, Arc<Throttle>>>);

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("name", &self.name)
            .field("max", &self.max)
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

/// Seconds since the epoch.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl RateLimiter {
    /// A limiter named `name` (its counters are apart from every other name's): at most `max` hits per key per
    /// `window`. A window shorter than a second counts as one second; `max` 0 refuses every hit.
    pub fn new(name: impl Into<String>, max: u32, window: Duration) -> Self {
        let window = window.max(Duration::from_secs(1));
        Self {
            name: name.into(),
            max,
            window,
            local: Throttle::with_window(max, window),
        }
    }

    /// The most hits per key in a window.
    pub fn max(&self) -> u32 {
        self.max
    }

    /// The window.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// The cache key of `key` in the window starting at `start`.
    fn cache_key(&self, key: &str, start: u64) -> String {
        format!(
            "throttle:{}|{key}|{start}",
            crate::crypto::sha256_hex(&self.name)
        )
    }

    fn uses_memory(app: &App) -> bool {
        matches!(app.cache().store_name(), "" | "null")
    }

    /// The app's in-memory counter of this limiter (shared by every limiter with its name, `max` and window).
    fn memory(&self, app: &App) -> Arc<Throttle> {
        let counts = app.service_or_insert_with(MemoryCounts::default);
        let mut map = counts
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = format!("{}|{}|{}", self.name, self.max, self.window.as_secs());
        Arc::clone(
            map.entry(id)
                .or_insert_with(|| Arc::new(Throttle::with_window(self.max, self.window))),
        )
    }

    fn hit_in(&self, memory: &Throttle, key: &str) -> RateLimit {
        if self.max == 0 {
            return RateLimit::Limited {
                retry_after: self.window.as_secs().max(1),
            };
        }
        match memory.try_hit(key) {
            Ok(left) => RateLimit::Allowed { remaining: left },
            Err(blocked) => RateLimit::Limited {
                retry_after: blocked.retry_after,
            },
        }
    }

    /// Count one hit for `key` unless the key is over its limit in this window.
    ///
    /// # Errors
    /// The cache store fails (nothing is let through).
    pub async fn hit(&self, app: &App, key: &str) -> Result<RateLimit> {
        if Self::uses_memory(app) {
            return Ok(self.hit_in(&self.memory(app), key));
        }
        let cache = app.cache();
        // A fixed window: the key names it, and lives one window longer than it.
        let window = self.window.as_secs().max(1);
        let now = now_secs();
        let start = now - now % window;
        let key = self.cache_key(key, start);
        cache
            .add(&key, &0_i64, Duration::from_secs(window.saturating_mul(2)))
            .await?;
        let count = u64::try_from(cache.increment(&key, 1).await?).unwrap_or(0);
        if count > u64::from(self.max) {
            Ok(RateLimit::Limited {
                retry_after: (start + window).saturating_sub(now).max(1),
            })
        } else {
            Ok(RateLimit::Allowed {
                remaining: self
                    .max
                    .saturating_sub(u32::try_from(count).unwrap_or(u32::MAX)),
            })
        }
    }

    /// `key`'s state in the current window without counting a hit: `Allowed { remaining }` (the hits still let
    /// through) or `Limited { retry_after }` when none are left. A read only: parallel callers may all see
    /// `Allowed` and then race, so guard work with [`hit`](Self::hit), never with `peek` alone; `peek` serves to
    /// refuse early (before a lookup) a key that is blocked already.
    ///
    /// # Errors
    /// The cache store fails.
    pub async fn peek(&self, app: &App, key: &str) -> Result<RateLimit> {
        let window = self.window.as_secs().max(1);
        if self.max == 0 {
            return Ok(RateLimit::Limited {
                retry_after: window,
            });
        }
        if Self::uses_memory(app) {
            return Ok(match self.memory(app).peek(key) {
                Ok(left) => RateLimit::Allowed { remaining: left },
                Err(blocked) => RateLimit::Limited {
                    retry_after: blocked.retry_after,
                },
            });
        }
        let now = now_secs();
        let start = now - now % window;
        let count = app
            .cache()
            .get::<i64>(&self.cache_key(key, start))
            .await?
            .unwrap_or(0);
        let count = u64::try_from(count).unwrap_or(0);
        Ok(if count >= u64::from(self.max) {
            RateLimit::Limited {
                retry_after: (start + window).saturating_sub(now).max(1),
            }
        } else {
            RateLimit::Allowed {
                remaining: self
                    .max
                    .saturating_sub(u32::try_from(count).unwrap_or(u32::MAX)),
            }
        })
    }

    /// Forget `key`'s hits in the current window (e.g. after a success).
    ///
    /// # Errors
    /// The cache store fails.
    pub async fn clear(&self, app: &App, key: &str) -> Result<()> {
        if Self::uses_memory(app) {
            self.memory(app).clear(key);
            return Ok(());
        }
        let window = self.window.as_secs().max(1);
        let now = now_secs();
        app.cache()
            .forget(&self.cache_key(key, now - now % window))
            .await?;
        Ok(())
    }
}

/// One `throttle:` alias on one route.
struct Limit {
    /// Named by the route (its methods and path pattern, `POST /reset-password/{token}`), so every URL of the
    /// route shares one counter.
    limiter: RateLimiter,
    /// Where an HTML form's message goes.
    field: String,
}

/// The middleware for `alias` on the route `route` (methods and path pattern) when it is a
/// `throttle:` alias, `None` for any other alias.
pub(crate) fn from_alias(alias: &str, route: &str) -> Option<Result<ErasedMiddleware>> {
    let args = alias.strip_prefix(PREFIX)?;
    Some(
        parse(args)
            .map(|(max, window, field)| {
                let limit = Arc::new(Limit {
                    limiter: RateLimiter::new(route, max, window),
                    field,
                });
                ErasedMiddleware::new(move |req: Request, next: Next| {
                    let limit = Arc::clone(&limit);
                    async move { limit.handle(req, next).await }
                })
            })
            .ok_or_else(|| {
                Error::internal(format!(
                    "invalid middleware `{alias}`: write `throttle:<max>`, `throttle:<max>,<minutes>` or \
                     `throttle:<max>,<minutes>,<field>` with whole numbers from 1, e.g. `throttle:5,1`"
                ))
            }),
    )
}

/// The `throttle` middleware family: `args` is what follows `throttle:`.
pub(crate) fn family(args: &str, route: &str) -> Result<ErasedMiddleware> {
    from_alias(&format!("{PREFIX}{args}"), route)
        .unwrap_or_else(|| Err(Error::internal("not a throttle alias")))
}

fn parse(args: &str) -> Option<(u32, Duration, String)> {
    let mut parts = args.split(',').map(str::trim);
    let max: u32 = parts.next()?.parse().ok().filter(|m| *m > 0)?;
    let minutes: u64 = match parts.next() {
        Some(minutes) => minutes.parse().ok().filter(|m| *m > 0)?,
        None => 1,
    };
    let field = match parts.next() {
        Some(field) if !field.is_empty() => field.to_owned(),
        Some(_) => return None,
        None => DEFAULT_FIELD.to_owned(),
    };
    if parts.next().is_some() {
        return None;
    }
    Some((max, Duration::from_secs(minutes.checked_mul(60)?), field))
}

/// Who a request counts for: the authenticated user (the request's [`Principal`], which the web stack sets for a
/// signed-in session and an `auth:` middleware listed before `throttle:` sets for its guards), else the client
/// address (an IPv6 client by its /64).
fn client(req: &Request) -> String {
    if let Some(principal) = req.extensions().get::<Principal>() {
        return format!("user:{}", principal.user_id);
    }
    if let Some(id) = req.extensions().get::<Auth>().and_then(Auth::id) {
        return format!("user:{id}");
    }
    let ip = crate::client::of_request(req)
        .ip()
        .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    format!("ip:{}", throttle_ip(&ip))
}

impl Limit {
    async fn handle(&self, req: Request, next: Next) -> Response {
        let counted = match req.extensions().get::<App>() {
            Some(app) => self.limiter.hit(app, &client(&req)).await,
            // Outside the framework's stack (never in an app): this process's memory.
            None => Ok(self.limiter.hit_in(&self.limiter.local, &client(&req))),
        };
        let max = HeaderValue::from(self.limiter.max);
        match counted {
            // Fail closed: a request that could not be counted is not let through.
            Err(e) => {
                tracing::error!(error = %e, "the throttle could not count a request");
                e.into_response()
            }
            Ok(RateLimit::Allowed { remaining }) => {
                let mut response = next.run(req).await;
                let headers = response.headers_mut();
                headers.insert("x-ratelimit-limit", max);
                headers.insert("x-ratelimit-remaining", HeaderValue::from(remaining));
                response
            }
            Ok(RateLimit::Limited { retry_after }) => {
                let mut response = self.refusal(&req, retry_after);
                let headers = response.headers_mut();
                headers.insert(header::RETRY_AFTER, HeaderValue::from(retry_after));
                headers.insert("x-ratelimit-limit", max);
                headers.insert("x-ratelimit-remaining", HeaderValue::from(0_u32));
                response
            }
        }
    }

    /// 429: for a form on a web route a redirect back with the message on the alias's field
    /// (like a failed validation, input flashed); for JSON clients and API routes a plain 429.
    fn refusal(&self, req: &Request, retry_after: u64) -> Response {
        let headers = req.headers();
        let inertia = crate::session::web::is_inertia(headers);
        let json = crate::error::wants_json(headers)
            || (crate::session::web::is_json_body(headers) && !inertia);
        if req.extensions().get::<Session>().is_some() && !json {
            let message = format!("Too many attempts. Please try again in {retry_after} seconds.");
            let mut errors = crate::validation::ValidationErrors::new();
            errors.add(self.field.clone(), message.clone());
            let mut invalid =
                crate::validation::Invalid::new(errors, crate::validation::Input::new());
            invalid.status = StatusCode::TOO_MANY_REQUESTS;
            invalid.message = message;
            return Error::Validation(Box::new(invalid)).into_response();
        }
        Error::http(StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_aliases_parse() {
        let minute = Duration::from_secs(60);
        assert_eq!(parse("5,1"), Some((5, minute, "email".to_owned())));
        assert_eq!(parse("10"), Some((10, minute, "email".to_owned())));
        assert_eq!(
            parse(" 3 , 15 , name "),
            Some((3, Duration::from_secs(900), "name".to_owned()))
        );
        for bad in ["", "0,1", "5,0", "x,1", "5,y", "-1,1", "5,1,", "5,1,a,b"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        assert!(from_alias("auth", "GET /").is_none());
        assert!(from_alias("throttle:5,1", "GET /").is_some_and(|m| m.is_ok()));
        let err = from_alias("throttle:five", "GET /")
            .and_then(Result::err)
            .map(|e| e.to_string());
        assert!(err.is_some_and(|e| e.contains("throttle:<max>,<minutes>")));
    }

    /// The cache keys `throttle:` writes did not change with the move onto `RateLimiter`: old and new processes of
    /// one deploy keep one count.
    #[test]
    fn throttle_keys_are_unchanged() {
        let limiter = RateLimiter::new("POST /login", 5, Duration::from_secs(60));
        assert_eq!(
            limiter.cache_key("ip:1.2.3.4", 120),
            format!(
                "throttle:{}|ip:1.2.3.4|120",
                crate::crypto::sha256_hex("POST /login")
            )
        );
    }
}
