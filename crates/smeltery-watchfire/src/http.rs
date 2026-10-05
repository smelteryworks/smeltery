//! The polite HTTP client agents use: [`Http`].
//!
//! Every request has a timeout; connect errors, timeouts, `429` and `5xx` answers are retried
//! with exponential backoff and full jitter (honouring `Retry-After`); requests to a host with a
//! rate limit (`Watchfire::rate_limit`) wait for a token of its bucket first. Redirects are
//! followed by the client itself ([`Redirects`]), so every hop goes through the same rate limits,
//! and response bodies have a size limit ([`HttpOptions::max_body`]). Errors name the URL without
//! its query string. The network sits behind the [`Transport`] trait: reqwest (rustls with ring)
//! in apps, [`FakeTransport`] in tests.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
pub use http::{HeaderMap, Method, StatusCode};
use http::{HeaderName, HeaderValue, header};
use serde::Serialize;
use serde::de::DeserializeOwned;
use smeltery_core::BoxFuture;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::policy::{Backoff, Jitter};
use crate::time::Rate;

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HttpError {
    /// The URL could not be parsed or has no host (the text says why; it never repeats the query string).
    #[error("invalid URL ({0})")]
    InvalidUrl(String),
    /// The connection could not be made (also after the retries).
    #[error("cannot connect: {0}")]
    Connect(String),
    /// No complete answer within the timeout (also after the retries).
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    /// The run was cancelled while waiting.
    #[error("cancelled")]
    Cancelled,
    /// The answer had an error status (from [`Response::error_for_status`]).
    #[error("HTTP {0}")]
    Status(StatusCode),
    /// The body was not valid UTF-8 or JSON.
    #[error("cannot decode the body: {0}")]
    Decode(String),
    /// The response body is larger than the limit (in bytes), see [`HttpOptions::max_body`].
    #[error("the response body is larger than {0} bytes")]
    TooLarge(usize),
    /// A redirect was not followed: too many hops, or a target the [`Redirects`] policy refuses.
    #[error("redirect refused: {0}")]
    Redirect(String),
    /// Anything else the transport reported.
    #[error("{0}")]
    Other(String),
}

/// `scheme://host[:port]/path` of `url`: no user info, query string or fragment, which often
/// carry keys and tokens. Used in errors and logs.
pub(crate) fn redact_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(url) => redact(&url),
        Err(e) => format!("unparseable URL: {e}"),
    }
}

fn redact(url: &reqwest::Url) -> String {
    let mut text = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        text.push_str(&format!(":{port}"));
    }
    text.push_str(url.path());
    text
}

/// Which redirects the client follows (it follows them itself, so each hop waits for the rate
/// limit of its own host). Only `301`, `302`, `303`, `307` and `308` with a `Location` are
/// followed, at most [`HttpOptions::max_redirects`] hops; a target that is not `http` or `https`
/// is refused ([`HttpError::Redirect`]).
///
/// On a hop to another origin (scheme, host or port) only the `User-Agent`, `Accept` and
/// `Accept-Language` headers go along (no `Authorization`, cookies or API-key headers), and a
/// request body is never sent there (a `307`/`308` with a body to another origin is refused).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Redirects {
    /// Follow none: a `3xx` answer is returned as it is.
    None,
    /// Follow redirects within the same origin only; another origin is refused.
    SameOrigin,
    /// Follow redirects to any `http` or `https` URL, but never from `https` to `http` (default).
    #[default]
    Follow,
}

/// A request as the [`Transport`] sees it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Request {
    /// The method.
    pub method: Method,
    /// The full URL.
    pub url: String,
    /// The headers (including `User-Agent`).
    pub headers: HeaderMap,
    /// The body.
    pub body: Bytes,
    /// How long the transport may take for the whole exchange.
    pub timeout: Duration,
    /// The largest response body the transport may read, in bytes; a larger one is
    /// [`TransportError::TooLarge`].
    pub max_body: usize,
}

/// A complete answer: status, headers and the whole body.
#[derive(Clone, Debug)]
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Response {
    /// A response from parts (what a [`Transport`] returns).
    pub fn new(status: StatusCode, headers: HeaderMap, body: Bytes) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// The status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// One header as text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The body as text.
    ///
    /// # Errors
    /// The body is not UTF-8.
    pub async fn text(&self) -> Result<String, HttpError> {
        String::from_utf8(self.body.to_vec()).map_err(|e| HttpError::Decode(e.to_string()))
    }

    /// The body parsed as JSON.
    ///
    /// # Errors
    /// The body is not JSON of that shape.
    pub async fn json<T: DeserializeOwned>(&self) -> Result<T, HttpError> {
        serde_json::from_slice(&self.body).map_err(|e| HttpError::Decode(e.to_string()))
    }

    /// The raw body.
    pub async fn bytes(&self) -> Bytes {
        self.body.clone()
    }

    /// `Err(HttpError::Status)` for 4xx and 5xx answers.
    ///
    /// # Errors
    /// See above.
    pub fn error_for_status(self) -> Result<Self, HttpError> {
        if self.status.is_client_error() || self.status.is_server_error() {
            Err(HttpError::Status(self.status))
        } else {
            Ok(self)
        }
    }
}

/// Why a transport could not deliver a response.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportError {
    /// Connecting failed (retried: the request was not sent).
    Connect(String),
    /// The exchange timed out.
    Timeout,
    /// The response body is larger than [`Request::max_body`] (not retried).
    TooLarge(usize),
    /// Any other failure (not retried).
    Other(String),
}

/// What sends requests: [`ReqwestTransport`] in apps, [`FakeTransport`] in tests.
pub trait Transport: Send + Sync + 'static {
    /// Send one request and read the whole answer (at most [`Request::max_body`] bytes of body).
    /// Redirects are not followed here: [`Http`] follows them.
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>>;
}

/// Tunables of an [`Http`] client.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HttpOptions {
    /// Per-request timeout (`WATCHFIRE_HTTP_TIMEOUT`, seconds; default 30 s).
    pub timeout: Duration,
    /// Retries after the first attempt (default 3).
    pub retries: u32,
    /// Backoff between retries (default 500 ms..=10 s, full jitter).
    pub backoff: Backoff,
    /// The longest `Retry-After` honoured; a longer one returns the answer at once (60 s).
    pub max_retry_after: Duration,
    /// The default `User-Agent`.
    pub user_agent: String,
    /// The largest response body read, in bytes (`WATCHFIRE_HTTP_MAX_BODY`; default 10 MiB); a
    /// larger one fails with [`HttpError::TooLarge`] without being read to the end.
    pub max_body: usize,
    /// Which redirects are followed (default [`Redirects::Follow`]).
    pub redirects: Redirects,
    /// The most redirects followed for one request (default 5).
    pub max_redirects: u32,
}

/// The default [`HttpOptions::max_body`]: 10 MiB.
pub const DEFAULT_MAX_BODY: usize = 10 * 1024 * 1024;

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            retries: 3,
            backoff: Backoff::new(Duration::from_millis(500)..=Duration::from_secs(10)),
            max_retry_after: Duration::from_secs(60),
            user_agent: "smeltery-watchfire".to_owned(),
            max_body: DEFAULT_MAX_BODY,
            redirects: Redirects::default(),
            max_redirects: 5,
        }
    }
}

/// Per-host token buckets.
#[derive(Debug, Default)]
pub(crate) struct RateLimits {
    buckets: HashMap<String, Bucket>,
}

#[derive(Debug)]
struct Bucket {
    rate: Rate,
    state: Mutex<(f64, Instant)>,
}

impl RateLimits {
    pub(crate) fn new(rates: &[(String, Rate)]) -> Self {
        let buckets = rates
            .iter()
            .map(|(host, rate)| {
                (
                    host.to_ascii_lowercase(),
                    Bucket {
                        rate: *rate,
                        state: Mutex::new((f64::from(rate.count()), Instant::now())),
                    },
                )
            })
            .collect();
        Self { buckets }
    }

    /// Wait for a token of `host`'s bucket (at once for hosts without a limit).
    pub(crate) async fn wait(&self, host: &str) {
        let Some(bucket) = self.buckets.get(&host.to_ascii_lowercase()) else {
            return;
        };
        let capacity = f64::from(bucket.rate.count());
        let per_sec = capacity / bucket.rate.per().as_secs_f64();
        loop {
            let wait = {
                let mut state = bucket.state.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                let refill = now.duration_since(state.1).as_secs_f64() * per_sec;
                state.0 = (state.0 + refill).min(capacity);
                state.1 = now;
                if state.0 >= 1.0 {
                    state.0 -= 1.0;
                    return;
                }
                Duration::from_secs_f64((1.0 - state.0) / per_sec)
            };
            tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
        }
    }
}

/// The rate-limited, retrying HTTP client. Cheap to clone. From an agent use
/// [`AgentCtx::http`](crate::AgentCtx::http), which also stops waiting when the run is
/// cancelled.
///
/// ```
/// # async fn demo(ctx: smeltery_watchfire::AgentCtx) -> Result<(), smeltery_watchfire::AgentError> {
/// let page = ctx.http().get("https://example.com/").await?.text().await?;
/// # let _ = page;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Http {
    inner: Arc<HttpInner>,
    token: Option<CancellationToken>,
}

struct HttpInner {
    transport: Arc<dyn Transport>,
    limits: RateLimits,
    options: HttpOptions,
    jitter: Jitter,
}

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http")
            .field("options", &self.inner.options)
            .finish_non_exhaustive()
    }
}

impl Http {
    /// A client over `transport` with these options and per-host rates.
    pub fn new(transport: impl Transport, options: HttpOptions, rates: &[(String, Rate)]) -> Self {
        Self::from_arc(Arc::new(transport), options, rates)
    }

    pub(crate) fn from_arc(
        transport: Arc<dyn Transport>,
        options: HttpOptions,
        rates: &[(String, Rate)],
    ) -> Self {
        Self {
            inner: Arc::new(HttpInner {
                transport,
                limits: RateLimits::new(rates),
                options,
                jitter: Jitter::from_os(),
            }),
            token: None,
        }
    }

    pub(crate) fn with_token(&self, token: CancellationToken) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            token: Some(token),
        }
    }

    pub(crate) fn limits(&self) -> &RateLimits {
        &self.inner.limits
    }

    /// `GET url`.
    ///
    /// # Errors
    /// See [`RequestBuilder::send`].
    pub async fn get(&self, url: &str) -> Result<Response, HttpError> {
        self.request(Method::GET, url).send().await
    }

    /// `POST url` with a JSON body.
    ///
    /// # Errors
    /// See [`RequestBuilder::send`].
    pub async fn post_json<T: Serialize + ?Sized>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<Response, HttpError> {
        self.request(Method::POST, url).json(body).send().await
    }

    /// A request to build: headers, body, timeout, then [`RequestBuilder::send`].
    pub fn request(&self, method: Method, url: &str) -> RequestBuilder<'_> {
        RequestBuilder {
            http: self,
            method,
            url: url.to_owned(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
            timeout: None,
            error: None,
            repeatable: false,
            retries: None,
            redirects: None,
            max_body: None,
        }
    }

    async fn cancellable<T>(&self, fut: impl Future<Output = T>) -> Result<T, HttpError> {
        match &self.token {
            Some(token) => tokio::select! {
                biased;
                () = token.cancelled() => Err(HttpError::Cancelled),
                value = fut => Ok(value),
            },
            None => Ok(fut.await),
        }
    }

    /// Send `request`, following redirects as `redirects` allows.
    async fn execute(
        &self,
        mut request: Request,
        repeatable: bool,
        retries: Option<u32>,
        redirects: Redirects,
    ) -> Result<Response, HttpError> {
        let options = &self.inner.options;
        if !request.headers.contains_key(header::USER_AGENT)
            && let Ok(value) = HeaderValue::from_str(&options.user_agent)
        {
            request.headers.insert(header::USER_AGENT, value);
        }
        let mut hops = 0_u32;
        loop {
            let url = reqwest::Url::parse(&request.url)
                .map_err(|e| HttpError::InvalidUrl(format!("cannot parse it: {e}")))?;
            let host = url.host_str().map(str::to_owned).ok_or_else(|| {
                HttpError::InvalidUrl(format!("a `{}:` URL has no host", url.scheme()))
            })?;
            let response = self
                .send_retrying(&request, &host, repeatable, retries)
                .await?;
            let status = response.status().as_u16();
            if redirects == Redirects::None || !matches!(status, 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let Some(location) = response.header("location") else {
                return Ok(response);
            };
            let next = url
                .join(location.trim())
                .map_err(|_| HttpError::Redirect("the Location header is not a URL".to_owned()))?;
            if !matches!(next.scheme(), "http" | "https") {
                return Err(HttpError::Redirect(format!(
                    "to a `{}` URL (only http and https are followed)",
                    next.scheme()
                )));
            }
            if url.scheme() == "https" && next.scheme() == "http" {
                return Err(HttpError::Redirect(format!(
                    "from https to http ({})",
                    redact(&next)
                )));
            }
            let same_origin = url.origin() == next.origin();
            if redirects == Redirects::SameOrigin && !same_origin {
                return Err(HttpError::Redirect(format!(
                    "to another origin ({})",
                    redact(&next)
                )));
            }
            hops += 1;
            if hops > options.max_redirects {
                return Err(HttpError::Redirect(format!(
                    "more than {} redirects",
                    options.max_redirects
                )));
            }
            // What browsers do: 303 turns into a GET, and so do 301/302 after a POST; 307/308 repeat the request.
            let to_get = (status == 303 && request.method != Method::HEAD)
                || (matches!(status, 301 | 302) && request.method == Method::POST);
            if to_get {
                request.method = Method::GET;
                request.body = Bytes::new();
                request.headers.remove(header::CONTENT_TYPE);
                request.headers.remove(header::CONTENT_LENGTH);
            } else if !same_origin && !request.body.is_empty() {
                return Err(HttpError::Redirect(format!(
                    "a request body is not sent to another origin ({})",
                    redact(&next)
                )));
            }
            if !same_origin {
                // Credentials and API-key headers stay with the origin they were meant for.
                let kept: HeaderMap = request
                    .headers
                    .iter()
                    .filter(|(name, _)| {
                        [header::USER_AGENT, header::ACCEPT, header::ACCEPT_LANGUAGE].contains(name)
                    })
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                request.headers = kept;
            }
            tracing::debug!(from = %host, to = ?next.host_str(), status, "following a redirect");
            request.url = next.to_string();
        }
    }

    /// One hop: wait for the host's rate limit, send, retry what may be retried.
    async fn send_retrying(
        &self,
        request: &Request,
        host: &str,
        repeatable: bool,
        retries: Option<u32>,
    ) -> Result<Response, HttpError> {
        let options = &self.inner.options;
        // Retrying a request that may have reached the server is only safe when repeating it
        // is harmless.
        let idempotent = repeatable
            || matches!(
                request.method,
                Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
            );
        let max_retries = retries.unwrap_or(options.retries);
        let timeout = request.timeout;
        let mut attempt = 0_u32;
        loop {
            self.cancellable(self.inner.limits.wait(host)).await?;
            let sent = self
                .cancellable(tokio::time::timeout(
                    timeout,
                    self.inner.transport.send(request.clone()),
                ))
                .await?;
            let more = attempt < max_retries;
            let delay = match sent {
                Ok(Ok(response)) => {
                    // Whatever the transport: a body over the limit is refused.
                    if response.body.len() > request.max_body {
                        return Err(HttpError::TooLarge(request.max_body));
                    }
                    let status = response.status();
                    let retryable = status == StatusCode::TOO_MANY_REQUESTS
                        || (status.is_server_error() && idempotent);
                    if !retryable || !more {
                        return Ok(response);
                    }
                    match retry_after(&response) {
                        Some(wait) if wait > options.max_retry_after => return Ok(response),
                        Some(wait) => wait,
                        None => options.backoff.delay(attempt, &self.inner.jitter),
                    }
                }
                Ok(Err(TransportError::Connect(e))) => {
                    if !more {
                        return Err(HttpError::Connect(e));
                    }
                    options.backoff.delay(attempt, &self.inner.jitter)
                }
                Ok(Err(TransportError::Timeout)) | Err(_) => {
                    if !more || !idempotent {
                        return Err(HttpError::Timeout(timeout));
                    }
                    options.backoff.delay(attempt, &self.inner.jitter)
                }
                Ok(Err(TransportError::TooLarge(limit))) => return Err(HttpError::TooLarge(limit)),
                Ok(Err(TransportError::Other(e))) => return Err(HttpError::Other(e)),
            };
            // The host only: the URL's query string may hold a key.
            tracing::debug!(host = %host, attempt, delay_ms = delay.as_millis(), "retrying request");
            self.cancellable(tokio::time::sleep(delay)).await?;
            attempt += 1;
        }
    }
}

/// `Retry-After` as seconds or an HTTP date.
pub(crate) fn retry_after(response: &Response) -> Option<Duration> {
    let value = response.header("retry-after")?.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = parse_http_date(value)?;
    let now = crate::time::system_ms() / 1000;
    Some(Duration::from_secs(
        u64::try_from(at.saturating_sub(now)).unwrap_or(0),
    ))
}

/// `Sun, 06 Nov 1994 08:49:37 GMT` → Unix seconds; `None` for anything out of range (the
/// header comes from the server, so absurd values must not overflow).
fn parse_http_date(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [_, day, month, year, time, "GMT"] = parts.as_slice() else {
        return None;
    };
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| m == month)?;
    let mut hms = time.split(':').map(|p| p.parse::<i64>().ok());
    let (h, m, s) = (hms.next()??, hms.next()??, hms.next()??);
    if hms.next().is_some()
        || !(0..24).contains(&h)
        || !(0..60).contains(&m)
        || !(0..=60).contains(&s)
    {
        return None;
    }
    let year: i64 = year.parse().ok()?;
    let day: u32 = day.parse().ok()?;
    if !(1970..=9999).contains(&year) || !(1..=31).contains(&day) {
        return None;
    }
    let days = crate::time::days_from_civil(year, u32::try_from(month + 1).ok()?, day);
    Some(days * 86_400 + h * 3600 + m * 60 + s)
}

/// A request being built by [`Http::request`].
#[derive(Debug)]
pub struct RequestBuilder<'a> {
    http: &'a Http,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Bytes,
    timeout: Option<Duration>,
    error: Option<HttpError>,
    repeatable: bool,
    retries: Option<u32>,
    redirects: Option<Redirects>,
    max_body: Option<usize>,
}

impl RequestBuilder<'_> {
    /// Which redirects this request follows (instead of the client's [`HttpOptions::redirects`]).
    pub fn redirects(mut self, redirects: Redirects) -> Self {
        self.redirects = Some(redirects);
        self
    }

    /// The largest response body for this request, in bytes (instead of the client's
    /// [`HttpOptions::max_body`]).
    pub fn max_body(mut self, bytes: usize) -> Self {
        self.max_body = Some(bytes);
        self
    }

    /// Retry `5xx` answers and timeouts for this request too, whatever its method: for requests
    /// that are safe to repeat (e.g. a webhook delivery).
    pub fn repeatable(mut self) -> Self {
        self.repeatable = true;
        self
    }

    /// Retries after the first attempt for this request (instead of the client's).
    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = Some(retries);
        self
    }

    /// Add a header (an invalid name or value fails the request).
    pub fn header(mut self, name: &str, value: &str) -> Self {
        match (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            (Ok(name), Ok(value)) => {
                self.headers.append(name, value);
            }
            _ => self.error = Some(HttpError::Other(format!("invalid header `{name}`"))),
        }
        self
    }

    /// `Authorization: Bearer <token>`.
    pub fn bearer(self, token: &str) -> Self {
        self.header("authorization", &format!("Bearer {token}"))
    }

    /// The body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    /// A JSON body (and `Content-Type: application/json`).
    pub fn json<T: Serialize + ?Sized>(mut self, body: &T) -> Self {
        match serde_json::to_vec(body) {
            Ok(bytes) => {
                self.body = Bytes::from(bytes);
                self.headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
            }
            Err(e) => self.error = Some(HttpError::Other(e.to_string())),
        }
        self
    }

    /// This request's timeout (per attempt) instead of the client's.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Send it, with rate limiting and retries.
    ///
    /// # Errors
    /// An invalid URL or header, a connect error or timeout after the retries, a body over the
    /// size limit, a refused redirect, another transport failure, or cancellation of the run. Error statuses are answers, not errors
    /// (see [`Response::error_for_status`]).
    pub async fn send(self) -> Result<Response, HttpError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let options = &self.http.inner.options;
        let timeout = self.timeout.unwrap_or(options.timeout);
        self.http
            .execute(
                Request {
                    method: self.method,
                    url: self.url,
                    headers: self.headers,
                    body: self.body,
                    timeout,
                    max_body: self.max_body.unwrap_or(options.max_body),
                },
                self.repeatable,
                self.retries,
                self.redirects.unwrap_or(options.redirects),
            )
            .await
    }
}

/// The real transport: reqwest with rustls (ring) and the platform's certificate verifier,
/// HTTP/1.1, a 10 s connect timeout. It follows no redirect and sends no `Referer` ([`Http`]
/// follows redirects under its [`Redirects`] policy), reads the body in chunks and stops at
/// [`Request::max_body`], and its errors carry the URL without the query string.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Build the client.
    ///
    /// # Errors
    /// The TLS configuration or the platform verifier cannot be set up.
    pub fn new() -> Result<Self, HttpError> {
        use rustls_platform_verifier::BuilderVerifierExt as _;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| HttpError::Other(format!("TLS setup: {e}")))?
            .with_platform_verifier()
            .map_err(|e| HttpError::Other(format!("TLS setup: {e}")))?
            .with_no_client_auth();
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .build()
            .map_err(|e| HttpError::Other(format!("HTTP client setup: {e}")))?;
        Ok(Self { client })
    }
}

impl Transport for ReqwestTransport {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(async move {
            let shown = redact_url(&request.url);
            let classify = |e: reqwest::Error| {
                if e.is_timeout() {
                    TransportError::Timeout
                } else if e.is_connect() {
                    TransportError::Connect(describe(e, &shown))
                } else {
                    TransportError::Other(describe(e, &shown))
                }
            };
            let max = request.max_body;
            let mut response = self
                .client
                .request(request.method, &request.url)
                .headers(request.headers)
                .body(request.body)
                .timeout(request.timeout)
                .send()
                .await
                .map_err(classify)?;
            let status = response.status();
            let headers = response.headers().clone();
            let announced = response.content_length();
            if announced.is_some_and(|n| n > u64::try_from(max).unwrap_or(u64::MAX)) {
                return Err(TransportError::TooLarge(max));
            }
            let capacity = announced.map_or(0, |n| usize::try_from(n).unwrap_or(0).min(max));
            let mut body = bytes::BytesMut::with_capacity(capacity);
            while let Some(chunk) = response.chunk().await.map_err(classify)? {
                if body.len().saturating_add(chunk.len()) > max {
                    return Err(TransportError::TooLarge(max));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Response::new(status, headers, body.freeze()))
        })
    }
}

/// A reqwest error as text without its URL (reqwest appends the full URL, query string included),
/// with its causes, then the redacted URL.
fn describe(error: reqwest::Error, shown: &str) -> String {
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.ends_with(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    format!("{text} ({shown})")
}

/// A transport that fails every request: used when the real one cannot be set up, so agents
/// get a clear error instead of a crash.
pub(crate) struct BrokenTransport(pub(crate) String);

impl Transport for BrokenTransport {
    fn send(&self, _: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        let reason = self.0.clone();
        Box::pin(async move { Err(TransportError::Other(reason)) })
    }
}

/// One canned answer of a [`FakeTransport`].
#[derive(Clone, Debug)]
pub struct FakeResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    delay: Option<Duration>,
    error: Option<TransportError>,
}

impl FakeResponse {
    /// An answer with this status and an empty body.
    pub fn status(status: u16) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            headers: HeaderMap::new(),
            body: Bytes::new(),
            delay: None,
            error: None,
        }
    }

    /// `200` with this text body.
    pub fn text(body: &str) -> Self {
        Self::status(200).body(body.to_owned())
    }

    /// `200` with this JSON body.
    pub fn json(body: &serde_json::Value) -> Self {
        Self::status(200)
            .header("content-type", "application/json")
            .body(body.to_string())
    }

    /// A connection error instead of an answer.
    pub fn connect_error() -> Self {
        let mut fake = Self::status(500);
        fake.error = Some(TransportError::Connect(
            "connection refused (fake)".to_owned(),
        ));
        fake
    }

    /// Set the body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    /// Add a header.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            self.headers.append(name, value);
        }
        self
    }

    /// Answer only after `delay` (with a shorter request timeout: a timeout).
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }
}

/// A request a [`FakeTransport`] received.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RecordedRequest {
    /// The method.
    pub method: Method,
    /// The URL.
    pub url: String,
    /// The headers.
    pub headers: HeaderMap,
    /// The body.
    pub body: Bytes,
    /// When it arrived (Tokio time).
    pub at: Instant,
}

/// A transport with canned answers by method + URL, recording every request. Answers for one
/// route are used in order; the last one repeats. An unknown route is an error.
///
/// ```
/// use smeltery_watchfire::http::{FakeResponse, FakeTransport, Http, HttpOptions, Method};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let fake = FakeTransport::new();
/// fake.on(Method::GET, "https://example.com/a", FakeResponse::status(503))
///     .on(Method::GET, "https://example.com/a", FakeResponse::text("ok"));
/// let mut options = HttpOptions::default();
/// options.backoff = smeltery_watchfire::Backoff::new(
///     std::time::Duration::from_millis(1)..=std::time::Duration::from_millis(1),
/// );
/// let http = Http::new(fake.clone(), options, &[]);
/// let res = http.get("https://example.com/a").await.unwrap();
/// assert_eq!(res.text().await.unwrap(), "ok");
/// assert_eq!(fake.requests().len(), 2);
/// # }
/// ```
#[derive(Clone, Debug, Default)]
pub struct FakeTransport {
    inner: Arc<Mutex<FakeInner>>,
}

#[derive(Debug, Default)]
struct FakeInner {
    routes: HashMap<(Method, String), VecDeque<FakeResponse>>,
    requests: Vec<RecordedRequest>,
}

impl FakeTransport {
    /// No routes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an answer for `method url`.
    pub fn on(&self, method: Method, url: &str, response: FakeResponse) -> &Self {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .entry((method, url.to_owned()))
            .or_default()
            .push_back(response);
        self
    }

    /// Every request so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requests
            .clone()
    }
}

impl Transport for FakeTransport {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        let reply = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.requests.push(RecordedRequest {
                method: request.method.clone(),
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
                at: Instant::now(),
            });
            inner
                .routes
                .get_mut(&(request.method.clone(), request.url.clone()))
                .and_then(|queue| {
                    if queue.len() > 1 {
                        queue.pop_front()
                    } else {
                        queue.front().cloned()
                    }
                })
        };
        Box::pin(async move {
            let Some(reply) = reply else {
                return Err(TransportError::Other(format!(
                    "no fake response for {} {}",
                    request.method, request.url
                )));
            };
            if let Some(delay) = reply.delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(error) = reply.error {
                return Err(error);
            }
            Ok(Response::new(reply.status, reply.headers, reply.body))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::RateExt;

    const URL: &str = "https://example.com/x";

    fn fast() -> HttpOptions {
        HttpOptions {
            backoff: Backoff::new(Duration::from_millis(100)..=Duration::from_millis(100)),
            ..HttpOptions::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retries_5xx_then_succeeds_and_sets_user_agent() {
        let fake = FakeTransport::new();
        fake.on(Method::GET, URL, FakeResponse::status(500))
            .on(Method::GET, URL, FakeResponse::status(502))
            .on(Method::GET, URL, FakeResponse::text("ok"));
        let http = Http::new(fake.clone(), fast(), &[]);
        let res = http.get(URL).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let requests = fake.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[0].headers.get("user-agent").unwrap(),
            "smeltery-watchfire"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_retries_and_returns_last_answer() {
        let fake = FakeTransport::new();
        fake.on(Method::GET, URL, FakeResponse::status(503));
        let http = Http::new(fake.clone(), fast(), &[]);
        let res = http.get(URL).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(fake.requests().len(), 4);
        assert!(res.error_for_status().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn honours_retry_after_and_caps_it() {
        let fake = FakeTransport::new();
        fake.on(
            Method::GET,
            URL,
            FakeResponse::status(429).header("retry-after", "7"),
        )
        .on(Method::GET, URL, FakeResponse::text("ok"));
        let http = Http::new(fake.clone(), fast(), &[]);
        http.get(URL).await.unwrap();
        let r = fake.requests();
        assert_eq!(r[1].at - r[0].at, Duration::from_secs(7));

        let fake = FakeTransport::new();
        fake.on(
            Method::GET,
            URL,
            FakeResponse::status(429).header("retry-after", "3600"),
        );
        let http = Http::new(fake.clone(), fast(), &[]);
        assert_eq!(http.get(URL).await.unwrap().status(), 429);
        assert_eq!(
            fake.requests().len(),
            1,
            "a too long Retry-After is not waited"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timeouts_and_connect_errors_are_retried() {
        let fake = FakeTransport::new();
        fake.on(
            Method::GET,
            URL,
            FakeResponse::text("slow").delay(Duration::from_secs(60)),
        )
        .on(Method::GET, URL, FakeResponse::connect_error())
        .on(Method::GET, URL, FakeResponse::text("ok"));
        let options = HttpOptions {
            timeout: Duration::from_secs(5),
            ..fast()
        };
        let http = Http::new(fake.clone(), options, &[]);
        let res = http.get(URL).await.unwrap();
        assert_eq!(res.text().await.unwrap(), "ok");
        let r = fake.requests();
        assert_eq!(r.len(), 3);
        // The 5 s timeout, then a jittered backoff of at most 100 ms.
        let gap = r[1].at - r[0].at;
        assert!(
            gap >= Duration::from_secs(5) && gap <= Duration::from_millis(5_100),
            "{gap:?}"
        );

        // Timeouts every time: the error names the timeout.
        let fake = FakeTransport::new();
        fake.on(
            Method::GET,
            URL,
            FakeResponse::text("slow").delay(Duration::from_secs(60)),
        );
        let http = Http::new(
            fake.clone(),
            HttpOptions {
                timeout: Duration::from_secs(1),
                ..fast()
            },
            &[],
        );
        assert_eq!(
            http.get(URL).await.unwrap_err(),
            HttpError::Timeout(Duration::from_secs(1))
        );
        assert_eq!(fake.requests().len(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn post_is_not_retried_on_5xx_but_is_on_429() {
        let fake = FakeTransport::new();
        fake.on(Method::POST, URL, FakeResponse::status(500));
        let http = Http::new(fake.clone(), fast(), &[]);
        let res = http
            .post_json(URL, &serde_json::json!({"a": 1}))
            .await
            .unwrap();
        assert_eq!(res.status(), 500);
        let r = fake.requests();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].body.as_ref(), br#"{"a":1}"#);
        assert_eq!(
            r[0].headers.get("content-type").unwrap(),
            "application/json"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn token_bucket_spaces_requests_per_host() {
        let fake = FakeTransport::new();
        fake.on(Method::GET, URL, FakeResponse::text("ok"));
        fake.on(Method::GET, "https://other.org/", FakeResponse::text("ok"));
        let http = Http::new(
            fake.clone(),
            fast(),
            &[("example.com".to_owned(), 2.per_second())],
        );
        let start = Instant::now();
        for _ in 0..6 {
            http.get(URL).await.unwrap();
            http.get("https://other.org/").await.unwrap();
        }
        let times: Vec<Duration> = fake
            .requests()
            .iter()
            .filter(|r| r.url == URL)
            .map(|r| r.at - start)
            .collect();
        // A burst of two, then one every 500 ms.
        assert_eq!(times[0], Duration::ZERO);
        assert_eq!(times[1], Duration::ZERO);
        assert_eq!(times[2], Duration::from_millis(500));
        assert_eq!(times[5], Duration::from_millis(2_000));
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_interrupts_waiting() {
        let fake = FakeTransport::new();
        fake.on(
            Method::GET,
            URL,
            FakeResponse::text("slow").delay(Duration::from_secs(10)),
        );
        let token = CancellationToken::new();
        let http = Http::new(fake, fast(), &[]).with_token(token.clone());
        let cancel = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            token.cancel();
        };
        let (result, ()) = tokio::join!(http.get(URL), cancel);
        assert_eq!(result.unwrap_err(), HttpError::Cancelled);
    }

    #[tokio::test]
    async fn errors_for_bad_input_and_unknown_routes() {
        let http = Http::new(FakeTransport::new(), fast(), &[]);
        assert!(matches!(
            http.get("not a url").await,
            Err(HttpError::InvalidUrl(_))
        ));
        assert!(matches!(http.get(URL).await, Err(HttpError::Other(_))));
        assert!(matches!(
            http.request(Method::GET, URL)
                .header("bad name", "x")
                .send()
                .await,
            Err(HttpError::Other(_))
        ));
    }

    #[test]
    fn http_dates_parse() {
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(parse_http_date("yesterday"), None);
    }

    /// S4-05: a hostile `Retry-After` date overflowed the arithmetic (a panic in debug builds).
    #[test]
    fn absurd_http_dates_are_refused_without_overflow() {
        for text in [
            "Sun, 06 Nov 99999999999999999 00:00:00 GMT",
            "Sun, 06 Nov 1994 9999999999999999:00:00 GMT",
            "Sun, 06 Nov 1994 00:9999999999999999:00 GMT",
            "Sun, 06 Nov 1994 00:00:9999999999999999 GMT",
            "Sun, 99999999999 Nov 1994 00:00:00 GMT",
            "Sun, 06 Nov -99999999999999999 00:00:00 GMT",
            "Sun, 06 Nov 1994 24:00:00 GMT",
            "Sun, 06 Nov 1994 -1:00:00 GMT",
            "Sun, 06 Nov 1994 00:00:00:00 GMT",
            "Sun, 00 Nov 1994 00:00:00 GMT",
        ] {
            assert_eq!(parse_http_date(text), None, "{text}");
            let response = Response::new(
                StatusCode::SERVICE_UNAVAILABLE,
                {
                    let mut h = HeaderMap::new();
                    h.insert("retry-after", HeaderValue::from_str(text).unwrap());
                    h
                },
                Bytes::new(),
            );
            assert_eq!(retry_after(&response), None, "{text}");
        }
        assert_eq!(
            parse_http_date("Fri, 31 Dec 9999 23:59:60 GMT"),
            Some(253_402_300_800)
        );
        // A huge number of seconds is a long wait (not honoured), never a panic.
        let response = Response::new(
            StatusCode::TOO_MANY_REQUESTS,
            {
                let mut h = HeaderMap::new();
                h.insert(
                    "retry-after",
                    HeaderValue::from_static("18446744073709551615"),
                );
                h
            },
            Bytes::new(),
        );
        assert_eq!(retry_after(&response), Some(Duration::from_secs(u64::MAX)));
    }

    #[tokio::test(start_paused = true)]
    async fn an_absurd_retry_after_date_returns_the_answer() {
        let fake = FakeTransport::new();
        fake.on(
            Method::GET,
            URL,
            FakeResponse::status(503)
                .header("retry-after", "Sun, 06 Nov 99999999999999999 00:00:00 GMT"),
        )
        .on(Method::GET, URL, FakeResponse::text("ok"));
        let http = Http::new(fake.clone(), fast(), &[]);
        // Unparseable: the normal backoff applies.
        assert_eq!(http.get(URL).await.unwrap().status(), 200);
        assert_eq!(fake.requests().len(), 2);
    }

    /// S4-02: errors and logs never carry the query string (keys) of a URL.
    #[test]
    fn urls_in_errors_lose_their_query_and_user_info() {
        assert_eq!(
            redact_url("https://user:pw@api.example.com:8443/v1/data?api_key=SECRET#frag"),
            "https://api.example.com:8443/v1/data"
        );
        assert_eq!(
            redact_url("http://127.0.0.1/x?key=SECRET"),
            "http://127.0.0.1/x"
        );
        assert!(!redact_url("not a url ?key=SECRET").contains("SECRET"));
    }

    #[tokio::test]
    async fn invalid_urls_are_not_echoed() {
        let http = Http::new(FakeTransport::new(), fast(), &[]);
        for url in ["not a url?key=SECRET", "data:text/plain,SECRET?key=SECRET"] {
            let err = http.get(url).await.unwrap_err();
            assert!(matches!(err, HttpError::InvalidUrl(_)), "{err:?}");
            assert!(!err.to_string().contains("SECRET"), "{err}");
        }
    }

    #[tokio::test]
    async fn reqwest_errors_carry_the_url_without_its_query() {
        // A port nothing listens on (bound, then closed).
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let http = Http::new(
            ReqwestTransport::new().unwrap(),
            HttpOptions {
                retries: 0,
                ..fast()
            },
            &[],
        );
        let err = http
            .get(&format!(
                "http://127.0.0.1:{port}/v1/data?api_key=SECRET_QUERY_KEY"
            ))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(matches!(err, HttpError::Connect(_)), "{err:?}");
        assert!(!text.contains("SECRET_QUERY_KEY"), "{text}");
        assert!(
            text.contains(&format!("http://127.0.0.1:{port}/v1/data")),
            "{text}"
        );
    }

    /// S4-03: a body over the limit is an error, whatever the transport.
    #[tokio::test]
    async fn bodies_over_the_limit_are_refused() {
        let fake = FakeTransport::new();
        fake.on(Method::GET, URL, FakeResponse::text(&"x".repeat(2048)));
        let http = Http::new(
            fake.clone(),
            HttpOptions {
                max_body: 1024,
                ..fast()
            },
            &[],
        );
        assert_eq!(http.get(URL).await.unwrap_err(), HttpError::TooLarge(1024));
        // Per request.
        let res = http.request(Method::GET, URL).max_body(4096).send().await;
        assert_eq!(res.unwrap().bytes().await.len(), 2048);
        assert_eq!(HttpOptions::default().max_body, 10 * 1024 * 1024);
    }

    /// A one-shot HTTP/1.1 server on 127.0.0.1 that answers every connection with `head` and then
    /// `body_chunks` chunks of 64 KiB (as long as the client reads), counting the bytes it wrote.
    async fn big_server(
        head: &'static str,
        body_chunks: usize,
    ) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&written);
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0_u8; 4096];
            let _ = socket.read(&mut buf).await;
            if socket.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let chunk = vec![b'x'; 64 * 1024];
            let chunked = head.contains("chunked");
            for _ in 0..body_chunks {
                let sent = if chunked {
                    let mut framed = format!("{:x}\r\n", chunk.len()).into_bytes();
                    framed.extend_from_slice(&chunk);
                    framed.extend_from_slice(b"\r\n");
                    socket.write_all(&framed).await
                } else {
                    socket.write_all(&chunk).await
                };
                if sent.is_err() {
                    return;
                }
                counter.fetch_add(chunk.len(), std::sync::atomic::Ordering::SeqCst);
            }
            if chunked {
                let _ = socket.write_all(b"0\r\n\r\n").await;
            }
        });
        (format!("http://{addr}/big"), written)
    }

    #[tokio::test]
    async fn the_real_transport_stops_reading_at_the_limit() {
        let http = Http::new(
            ReqwestTransport::new().unwrap(),
            HttpOptions {
                max_body: 256 * 1024,
                retries: 0,
                ..fast()
            },
            &[],
        );
        // Chunked, no length announced: 64 MiB offered, the read stops after the limit.
        let (url, written) = big_server(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
            1024,
        )
        .await;
        assert_eq!(
            http.get(&url).await.unwrap_err(),
            HttpError::TooLarge(256 * 1024)
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        let sent = written.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            sent < 16 * 1024 * 1024,
            "the server could write {sent} bytes"
        );
        // An announced length over the limit is refused before reading.
        let (url, _) = big_server("HTTP/1.1 200 OK\r\nContent-Length: 314572800\r\n\r\n", 4).await;
        assert_eq!(
            http.get(&url).await.unwrap_err(),
            HttpError::TooLarge(256 * 1024)
        );
        // Under the limit: read whole.
        let (url, _) = big_server("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n", 2).await;
        assert_eq!(
            http.get(&url).await.unwrap().bytes().await.len(),
            128 * 1024
        );
    }

    fn redirect(status: u16, to: &str) -> FakeResponse {
        FakeResponse::status(status).header("location", to)
    }

    /// S4-04 / S4-08: a cross-origin hop keeps no credentials, the rate limit of the target host
    /// applies, and the policy bounds the hops and the schemes.
    #[tokio::test(start_paused = true)]
    async fn redirects_follow_the_policy() {
        let fake = FakeTransport::new();
        fake.on(
            Method::GET,
            URL,
            redirect(302, "https://other.org/landing?x=1"),
        )
        .on(
            Method::GET,
            "https://other.org/landing?x=1",
            FakeResponse::text("there"),
        );
        let http = Http::new(
            fake.clone(),
            fast(),
            &[("other.org".to_owned(), 1.per_minute())],
        );
        let res = http
            .request(Method::GET, URL)
            .header("x-api-key", "SECRET")
            .bearer("TOKEN")
            .header("accept", "text/html")
            .send()
            .await
            .unwrap();
        assert_eq!(res.text().await.unwrap(), "there");
        let r = fake.requests();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].headers.get("x-api-key").unwrap(), "SECRET");
        assert!(r[1].headers.get("x-api-key").is_none());
        assert!(r[1].headers.get("authorization").is_none());
        assert_eq!(r[1].headers.get("accept").unwrap(), "text/html");
        assert!(r[1].headers.get("user-agent").is_some());
        assert!(r[1].headers.get("referer").is_none());
        // The second request to other.org waits for its bucket (1 per minute), redirect or not.
        let start = Instant::now();
        http.get(URL).await.unwrap();
        assert!(Instant::now() - start >= Duration::from_secs(59));

        // Same origin: headers stay.
        let fake = FakeTransport::new();
        fake.on(Method::GET, URL, redirect(301, "/y")).on(
            Method::GET,
            "https://example.com/y",
            FakeResponse::text("y"),
        );
        let http = Http::new(fake.clone(), fast(), &[]);
        http.request(Method::GET, URL)
            .header("x-api-key", "SECRET")
            .send()
            .await
            .unwrap();
        assert_eq!(
            fake.requests()[1].headers.get("x-api-key").unwrap(),
            "SECRET"
        );

        // https -> http, a non-http scheme, too many hops: refused.
        let fake = FakeTransport::new();
        fake.on(Method::GET, URL, redirect(302, "http://example.com/plain"));
        let http = Http::new(fake.clone(), fast(), &[]);
        assert!(matches!(http.get(URL).await, Err(HttpError::Redirect(_))));
        assert_eq!(fake.requests().len(), 1);
        let fake = FakeTransport::new();
        fake.on(Method::GET, URL, redirect(302, "file:///etc/passwd"));
        let http = Http::new(fake.clone(), fast(), &[]);
        assert!(matches!(http.get(URL).await, Err(HttpError::Redirect(_))));
        let fake = FakeTransport::new();
        fake.on(Method::GET, URL, redirect(302, URL));
        let http = Http::new(fake.clone(), fast(), &[]);
        let err = http.get(URL).await.unwrap_err();
        assert_eq!(err, HttpError::Redirect("more than 5 redirects".to_owned()));
        assert_eq!(fake.requests().len(), 6);

        // http -> https is an upgrade and followed.
        let fake = FakeTransport::new();
        fake.on(Method::GET, "http://example.com/x", redirect(301, URL))
            .on(Method::GET, URL, FakeResponse::text("ok"));
        let http = Http::new(fake.clone(), fast(), &[]);
        assert_eq!(
            http.get("http://example.com/x").await.unwrap().status(),
            200
        );

        // `SameOrigin` refuses another host; `None` returns the 3xx.
        let fake = FakeTransport::new();
        fake.on(Method::GET, URL, redirect(302, "https://other.org/"));
        let http = Http::new(fake.clone(), fast(), &[]);
        assert!(matches!(
            http.request(Method::GET, URL)
                .redirects(Redirects::SameOrigin)
                .send()
                .await,
            Err(HttpError::Redirect(_))
        ));
        let res = http
            .request(Method::GET, URL)
            .redirects(Redirects::None)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 302);
        assert_eq!(fake.requests().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn redirected_bodies_never_reach_another_origin() {
        // 307 keeps the method and the body: refused to another origin.
        let fake = FakeTransport::new();
        fake.on(Method::POST, URL, redirect(307, "https://other.org/steal"));
        let http = Http::new(fake.clone(), fast(), &[]);
        let err = http
            .post_json(URL, &serde_json::json!({"prompt": "secret"}))
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::Redirect(_)), "{err:?}");
        assert_eq!(fake.requests().len(), 1);
        // Same origin: replayed.
        let fake = FakeTransport::new();
        fake.on(Method::POST, URL, redirect(308, "/z")).on(
            Method::POST,
            "https://example.com/z",
            FakeResponse::text("ok"),
        );
        let http = Http::new(fake.clone(), fast(), &[]);
        http.post_json(URL, &serde_json::json!({"a": 1}))
            .await
            .unwrap();
        assert_eq!(fake.requests()[1].body.as_ref(), br#"{"a":1}"#);
        // 303 after a POST: a GET without the body.
        let fake = FakeTransport::new();
        fake.on(Method::POST, URL, redirect(303, "https://other.org/done"))
            .on(
                Method::GET,
                "https://other.org/done",
                FakeResponse::text("done"),
            );
        let http = Http::new(fake.clone(), fast(), &[]);
        http.post_json(URL, &serde_json::json!({"a": 1}))
            .await
            .unwrap();
        let r = fake.requests();
        assert_eq!((r[1].method.clone(), r[1].body.len()), (Method::GET, 0));
        assert!(r[1].headers.get("content-type").is_none());
    }

    #[test]
    fn reqwest_transport_builds() {
        // Builds the TLS config with ring and the platform verifier; sends nothing.
        assert!(ReqwestTransport::new().is_ok());
    }
}
