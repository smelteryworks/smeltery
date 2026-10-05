//! The HTTP stack and the server loop with graceful shutdown.

use std::convert::Infallible;
use std::io::IsTerminal as _;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::body::Body;
use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use http_body_util::BodyExt as _;
use tower::{Service, ServiceExt as _};
use tower_http::services::ServeDir;

use crate::app::App;
use crate::client::ClientInfo;
use crate::error::{Error, ErrorReport, Result, render_error, wants_json};
use crate::middleware::{ErasedMiddleware, Next, method_override};
use crate::routing::{RouteInfo, RouteTable};

/// Build the router with the framework layers, outermost last:
/// routes → static files fallback → global middleware → views → panics → error pages → limits, timeout,
/// tracing, panic catching.
pub(crate) fn http_stack(
    app: &App,
    table: RouteTable,
    global: &[ErasedMiddleware],
    web_middleware: &[ErasedMiddleware],
) -> Result<axum::Router> {
    let settings = app.settings();
    let not_found = tower::service_fn(|_req: Request| async {
        Ok::<_, Infallible>(Error::not_found().into_response())
    });
    let cache_control = static_cache_control(&settings.static_cache_control);
    let public = ServeDir::new(settings.public_dir())
        .append_index_html_on_directories(false)
        .call_fallback_on_method_not_allowed(true)
        .fallback(not_found)
        .map_response(move |mut res: Response<_>| {
            // Files (and their 304 / range answers) get the header; the 404 fallback does not.
            let status = res.status();
            if (status.is_success() || status == StatusCode::NOT_MODIFIED)
                && !res.headers().contains_key(header::CACHE_CONTROL)
            {
                res.headers_mut()
                    .insert(header::CACHE_CONTROL, cache_control.clone());
            }
            res
        });
    // Files under `/storage` are uploads: whatever got stored there must not run as a page of
    // this site. Decided from the file that was served, not from the spelling of the URL (a
    // Windows file system opens `storage.`, `STORAG~1` or `storage::$INDEX_ALLOCATION` as
    // `storage`), and file names a file system would read as another name are refused.
    let roots = Arc::new(UploadRoots::new(settings));
    let public = tower::service_fn(move |req: Request| {
        let files = public.clone();
        let roots = Arc::clone(&roots);
        async move {
            let path = req.uri().path().to_owned();
            if !is_plain_static_path(&path) {
                return Ok::<_, Infallible>(Error::not_found().into_response());
            }
            let mut res = files.oneshot(req).await?.into_response();
            if res.status() != StatusCode::NOT_FOUND
                && (is_storage_path(&path) || roots.serves_upload(&path).await)
            {
                harden_upload_response(&mut res);
            }
            Ok::<_, Infallible>(res)
        }
    });

    let web_app = app.clone();
    let web = (app.web_config().is_some()).then(|| {
        ErasedMiddleware::new(move |req: Request, next: Next| {
            crate::session::web::web_stack(web_app.clone(), req, next)
        })
    });
    let mut router = table
        .into_axum(web.as_ref(), web_middleware)?
        .fallback_service(public)
        .with_state(app.clone());
    // The first registered global middleware must run first, so it is applied last.
    for m in global.iter().rev() {
        router = m.wrap_router(router);
    }
    let cors = std::sync::Arc::new(crate::cors::Cors::from_settings(settings)?);
    let debug = settings.debug;
    let request_timeout = settings.request_timeout;
    let views_app = app.clone();
    let log_uri = Arc::new(LogUri::new(app.routes()));
    let trusted = app.trusted_proxies().clone();
    let security = Arc::new(SecurityHeaders::from_settings(settings)?);
    router = router
        // Outside the global middleware (it sees their responses), inside the error pages (render errors get one).
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| crate::view::render_views(views_app.clone(), req, next),
        ))
        // Inside the error pages, so a panic gets the same 500 page as any error.
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(
            |_: Box<dyn std::any::Any + Send>| {
                tracing::error!("a handler panicked");
                Error::internal("a handler panicked").into_response()
            },
        ))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| error_pages(req, next, debug),
        ))
        .layer(Extension(app.clone()))
        .layer(axum::extract::DefaultBodyLimit::max(settings.body_limit))
        // `REQUEST_TIMEOUT`, counted from when the server took the request: the `_method`
        // override before routing reads the body inside the same budget (see
        // `with_method_override`).
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| within_deadline(req, next, request_timeout),
        ));
    // CORS outside the routes, the app's global middleware, the panic handler, the error pages and the request
    // timeout: a preflight is answered before routing (no route answers OPTIONS), and every answer to a listed
    // origin gets its headers, the 408, a panic's 500 and the rendered error pages included.
    if cors.enabled() {
        router = router.layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let cors = std::sync::Arc::clone(&cors);
                async move { cors.handle(req, next).await }
            },
        ));
    }
    router = router
        // Server-sent events (`text/event-stream`) are never compressed, so they still stream
        // (tower-http 0.7.1 `src/compression/predicate.rs:122-128`); nor are formats that are
        // compressed already. Level 4: brotli's own default is 11 (compression-codecs 0.4.45
        // `src/brotli/params.rs:33-46`), far too slow for responses made per request.
        .layer(
            tower_http::compression::CompressionLayer::new()
                .quality(tower_http::CompressionLevel::Precise(4))
                .compress_when(compress_predicate()),
        )
        // Outside the compression: a compressed body is another representation, so its ETag
        // must not be the identity body's strong validator (RFC 9110 §8.8.3).
        .layer(tower::util::MapResponseLayer::new(weaken_compressed_etag))
        // Outside everything that answers (handlers, static files, error pages, the 408), so
        // every response gets them; a header already set wins.
        .layer(tower::util::MapResponseLayer::new(move |res: Response| {
            security.apply(res)
        }))
        .layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(move |req: &Request| {
                let client = req
                    .extensions()
                    .get::<ClientInfo>()
                    .and_then(ClientInfo::ip)
                    .map_or_else(|| "-".to_owned(), |ip| ip.to_string());
                tracing::debug_span!(
                    "request",
                    method = %req.method(),
                    uri = %log_uri.render(req.uri()),
                    version = ?req.version(),
                    client = %client,
                )
            }),
        )
        // Outside the trace layer, so the request span can name the client.
        .layer(tower::util::MapRequestLayer::new(move |req: Request| {
            crate::client::attach(&trusted, req)
        }))
        .layer(tower_http::request_id::PropagateRequestIdLayer::x_request_id())
        .layer(tower_http::request_id::SetRequestIdLayer::x_request_id(
            tower_http::request_id::MakeRequestUuid,
        ));
    Ok(router)
}

/// tower-http's default (not SSE, not images other than SVG, not gRPC, not under 32 bytes) and
/// not formats that are compressed already: compressing them again costs CPU, gains nothing and
/// drops `Content-Length` and range support from downloads.
fn compress_predicate() -> impl tower_http::compression::Predicate {
    use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
    DefaultPredicate::new()
        .and(NotForContentType::const_new("font/woff"))
        .and(NotForContentType::const_new("application/zip"))
        .and(NotForContentType::const_new("application/gzip"))
        .and(NotForContentType::const_new("application/x-gzip"))
        .and(NotForContentType::const_new("application/x-7z-compressed"))
        .and(NotForContentType::const_new("application/x-rar-compressed"))
        .and(NotForContentType::const_new("application/zstd"))
        .and(NotForContentType::const_new("application/pdf"))
        .and(NotForContentType::const_new("video/"))
        .and(NotForContentType::const_new("audio/"))
}

/// `ETag: "x"` becomes `W/"x"` on a response with a `Content-Encoding`.
fn weaken_compressed_etag<B>(mut res: http::Response<B>) -> http::Response<B> {
    if !res.headers().contains_key(header::CONTENT_ENCODING) {
        return res;
    }
    let weak = res
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .filter(|tag| tag.starts_with('"'))
        .and_then(|tag| HeaderValue::from_str(&format!("W/{tag}")).ok());
    if let Some(weak) = weak {
        res.headers_mut().insert(header::ETAG, weak);
    }
    res
}

/// The security headers every response gets unless it sets them itself (`SECURITY_HEADERS`,
/// `FRAME_OPTIONS`, `HSTS_MAX_AGE`).
#[derive(Debug, Default)]
struct SecurityHeaders {
    /// Added when the response has no header of that name.
    plain: Vec<(header::HeaderName, HeaderValue)>,
    /// `X-Frame-Options`, added when the response has none.
    frame: Option<HeaderValue>,
    /// `Content-Security-Policy: frame-ancestors …`, added when the response has neither a
    /// `Content-Security-Policy` nor an `X-Frame-Options`: a handler that decides about framing
    /// with either header decides alone.
    frame_ancestors: Option<HeaderValue>,
}

impl SecurityHeaders {
    fn from_settings(settings: &crate::config::Settings) -> Result<Self> {
        let mut headers = Self::default();
        if settings.security_headers {
            headers.plain.push((
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ));
            headers.plain.push((
                header::REFERRER_POLICY,
                HeaderValue::from_static("strict-origin-when-cross-origin"),
            ));
            let frame = settings.frame_options.trim();
            let (frame, ancestors) = if frame.eq_ignore_ascii_case("sameorigin") {
                (Some("SAMEORIGIN"), Some("frame-ancestors 'self'"))
            } else if frame.eq_ignore_ascii_case("deny") {
                (Some("DENY"), Some("frame-ancestors 'none'"))
            } else if frame.eq_ignore_ascii_case("off") {
                (None, None)
            } else {
                // A typo must not silently leave the pages frameable, nor block them.
                return Err(Error::internal(format!(
                    "FRAME_OPTIONS must be SAMEORIGIN, DENY or off, not `{frame}`"
                )));
            };
            headers.frame = frame.map(HeaderValue::from_static);
            headers.frame_ancestors = ancestors.map(HeaderValue::from_static);
        }
        let max_age = settings.hsts_max_age.as_secs();
        if max_age > 0 {
            if settings
                .url
                .get(..8)
                .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
            {
                let value = HeaderValue::from_str(&format!("max-age={max_age}"))
                    .map_err(|e| Error::internal(format!("HSTS_MAX_AGE: {e}")))?;
                headers
                    .plain
                    .push((header::STRICT_TRANSPORT_SECURITY, value));
            } else {
                tracing::warn!(
                    "HSTS_MAX_AGE is set but APP_URL is not https: no Strict-Transport-Security header is sent"
                );
            }
        }
        Ok(headers)
    }

    fn apply(&self, mut res: Response) -> Response {
        let headers = res.headers_mut();
        for (name, value) in &self.plain {
            if !headers.contains_key(name) {
                headers.insert(name.clone(), value.clone());
            }
        }
        let framing_set = headers.contains_key(header::X_FRAME_OPTIONS);
        if let Some(ancestors) = &self.frame_ancestors
            && !framing_set
            && !headers.contains_key(header::CONTENT_SECURITY_POLICY)
        {
            headers.insert(header::CONTENT_SECURITY_POLICY, ancestors.clone());
        }
        if let Some(frame) = &self.frame
            && !framing_set
        {
            headers.insert(header::X_FRAME_OPTIONS, frame.clone());
        }
        res
    }
}

/// Whether `path` (a request path) names a file under `public/storage`, the uploads link:
/// compared percent-decoded and ignoring ASCII case (a case-insensitive file system serves
/// `/STORAGE/…` from the same folder), with any leading `/` or `\`.
fn is_storage_path(path: &str) -> bool {
    let decoded = percent_decode(path).to_ascii_lowercase();
    let rest = decoded.trim_start_matches(['/', '\\']);
    rest.strip_prefix("storage")
        .is_some_and(|after| after.starts_with(['/', '\\']))
}

/// Whether a static-file path names files only by their plain names: after percent-decoding,
/// no segment ends with `.` or a space (Windows drops those, so `storage.` opens `storage`),
/// is `.`, or holds `:` (NTFS streams such as `::$DATA`, drive letters), `\` or a control
/// character. Refused on every OS, so a URL means the same file everywhere.
fn is_plain_static_path(path: &str) -> bool {
    let decoded = percent_decode(path);
    decoded.split('/').all(|segment| {
        segment.is_empty()
            || !(segment.ends_with(['.', ' '])
                || segment.contains([':', '\\'])
                || segment.chars().any(char::is_control))
    })
}

/// Where uploads live, to tell whether a static file that was served is one: anything whose
/// resolved path (links followed, as the OS names it) is inside `storage/app/public` or
/// `public/storage`.
struct UploadRoots {
    public: std::path::PathBuf,
    roots: [std::path::PathBuf; 2],
}

impl UploadRoots {
    fn new(settings: &crate::config::Settings) -> Self {
        let public = settings.public_dir();
        Self {
            roots: [
                settings.storage_dir().join("app").join("public"),
                public.join("storage"),
            ],
            public,
        }
    }

    /// Whether the file `path` (a request path, served from `public/`) resolves into an
    /// upload folder. A path that cannot be resolved counts as an upload: the safe answer.
    async fn serves_upload(&self, path: &str) -> bool {
        let Some(file) = static_file(&self.public, path) else {
            return true;
        };
        let roots = self.roots.clone();
        tokio::task::spawn_blocking(move || {
            let Ok(file) = std::fs::canonicalize(&file) else {
                return true;
            };
            roots
                .iter()
                .filter_map(|root| std::fs::canonicalize(root).ok())
                .any(|root| file.starts_with(root))
        })
        .await
        .unwrap_or(true)
    }
}

/// The file tower-http's `ServeDir` opens for `path` under `public` (the same decoding and the
/// same components), or `None` where it serves nothing.
fn static_file(public: &std::path::Path, path: &str) -> Option<std::path::PathBuf> {
    use std::path::{Component, Path};
    let decoded = percent_decode(path.trim_start_matches('/'));
    let mut file = public.to_path_buf();
    for component in Path::new(&decoded).components() {
        match component {
            Component::Normal(part) => file.push(part),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }
    Some(file)
}

/// Types shown inline from `/storage`; anything else is sent as a download.
const INLINE_UPLOAD_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/bmp",
    "image/x-icon",
    "image/vnd.microsoft.icon",
    "application/pdf",
    "text/plain",
];

/// An uploaded file's response: `X-Content-Type-Options: nosniff`, `Content-Security-Policy:
/// sandbox` (an HTML or SVG file that slipped through cannot run script or reach the site's
/// cookies and pages: the browser treats it as another origin), and `Content-Disposition:
/// attachment` for every type that is not a raster image, PDF, plain text, video or audio.
/// PDFs get no `sandbox` (browsers refuse to show a sandboxed PDF); their viewers do not run
/// scripts with the site's origin.
fn harden_upload_response(res: &mut Response) {
    let mime = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let headers = res.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if mime != "application/pdf" {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox"),
        );
    }
    let inline = INLINE_UPLOAD_TYPES.contains(&mime.as_str())
        || mime.starts_with("video/")
        || mime.starts_with("audio/");
    if !inline {
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
    }
}

/// `STATIC_CACHE_CONTROL` as a header value; an invalid one falls back to `no-cache`.
fn static_cache_control(value: &str) -> HeaderValue {
    HeaderValue::from_str(value.trim()).unwrap_or_else(|_| {
        tracing::warn!("STATIC_CACHE_CONTROL is not a valid header value; using `no-cache`");
        HeaderValue::from_static("no-cache")
    })
}

/// Route parameter names that carry secrets: their path segments never reach the log.
const SECRET_PARAMS: &[&str] = &["token", "secret", "signature", "password", "hash", "key"];

/// The request URI as the log shows it: secret path segments (route parameters named like
/// [`SECRET_PARAMS`], e.g. `/reset-password/{token}`), every query value and every query item
/// without a value replaced with `[redacted]`. Query parameter names stay, so the log still
/// tells requests apart.
struct LogUri {
    /// Routes with at least one secret parameter, split into segments.
    secret_routes: Vec<Vec<Segment>>,
}

enum Segment {
    Literal(String),
    Param { secret: bool },
    Rest { secret: bool },
}

const REDACTED: &str = "[redacted]";

impl LogUri {
    fn new(routes: &[RouteInfo]) -> Self {
        let secret_routes = routes
            .iter()
            .map(|r| {
                r.path
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(
                        |s| match s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                            Some(name) => {
                                let rest = name.starts_with('*');
                                let name = name.trim_start_matches('*').to_ascii_lowercase();
                                let secret = SECRET_PARAMS.iter().any(|p| name.contains(p));
                                if rest {
                                    Segment::Rest { secret }
                                } else {
                                    Segment::Param { secret }
                                }
                            }
                            None => Segment::Literal(s.to_owned()),
                        },
                    )
                    .collect::<Vec<_>>()
            })
            .filter(|segments| {
                segments.iter().any(|s| {
                    matches!(
                        s,
                        Segment::Param { secret: true } | Segment::Rest { secret: true }
                    )
                })
            })
            .collect();
        Self { secret_routes }
    }

    fn render(&self, uri: &http::Uri) -> String {
        let mut out = self.path(uri.path());
        if let Some(query) = uri.query() {
            out.push('?');
            // An item without `=` can be a secret too (`?<signature>`): only names with a
            // value are kept.
            let pairs: Vec<String> = query
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|pair| match pair.split_once('=') {
                    Some((name, _)) => format!("{name}={REDACTED}"),
                    None => REDACTED.to_owned(),
                })
                .collect();
            out.push_str(&pairs.join("&"));
        }
        out
    }

    /// The path with secret segments redacted. It is matched without empty segments and with
    /// literals compared ignoring ASCII case, so a mangled link (`//x`, a trailing `/`, other
    /// case) that the router answers with 404 is still redacted; a match is logged in that
    /// normalised form.
    fn path(&self, path: &str) -> String {
        // Matched percent-decoded (`%2D`, `%2F` …), so an encoded link is redacted too.
        let decoded = percent_decode(path);
        let parts: Vec<&str> = decoded.split('/').filter(|s| !s.is_empty()).collect();
        for route in &self.secret_routes {
            if let Some(redacted) = redact_match(route, &parts) {
                return redacted;
            }
        }
        path.to_owned()
    }
}

/// `%XX` escapes decoded (invalid escapes kept as they are), as lossy UTF-8.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |at: usize| bytes.get(at).and_then(|c| char::from(*c).to_digit(16));
        if b == b'%'
            && let (Some(hi), Some(lo)) = (hex(i + 1), hex(i + 2))
        {
            out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'%'));
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `/` + `parts` with the secret segments replaced when they match `route`, else `None`.
fn redact_match(route: &[Segment], parts: &[&str]) -> Option<String> {
    let mut out = Vec::with_capacity(parts.len());
    for (i, segment) in route.iter().enumerate() {
        match segment {
            Segment::Rest { secret } => {
                let rest = parts.get(i..).unwrap_or_default();
                if *secret {
                    out.push(REDACTED.to_owned());
                } else {
                    out.extend(rest.iter().map(|s| (*s).to_owned()));
                }
                return Some(format!("/{}", out.join("/")));
            }
            Segment::Literal(literal) => {
                let part = parts.get(i).filter(|p| p.eq_ignore_ascii_case(literal))?;
                out.push((*part).to_owned());
            }
            Segment::Param { secret } => {
                let part = parts.get(i)?;
                out.push(if *secret {
                    REDACTED.to_owned()
                } else {
                    (*part).to_owned()
                });
            }
        }
    }
    (out.len() == parts.len()).then(|| format!("/{}", out.join("/")))
}

/// Turn errors and empty error responses into pages (or JSON).
async fn error_pages(req: Request, next: Next, debug: bool) -> Response {
    let json = wants_json(req.headers());
    let response = next.run(req).await;
    let status = response.status();
    if let Some(report) = response.extensions().get::<ErrorReport>().cloned() {
        let detail = debug.then_some(&*report.detail);
        let mut rendered = render_error(report.status, &report.public, detail, json);
        copy_headers(&response, &mut rendered);
        return rendered;
    }
    let empty = response.headers().get(header::CONTENT_TYPE).is_none()
        && http_body::Body::size_hint(response.body()).exact() == Some(0);
    if (status.is_client_error() || status.is_server_error()) && empty {
        let public = crate::error::reason(status);
        let mut rendered = render_error(status, public, None, json);
        copy_headers(&response, &mut rendered);
        return rendered;
    }
    response
}

/// Keep headers like `Allow` or `Set-Cookie` from the original error response.
fn copy_headers(from: &Response, to: &mut Response) {
    for (name, value) in from.headers() {
        if name != header::CONTENT_TYPE && name != header::CONTENT_LENGTH {
            to.headers_mut().append(name.clone(), value.clone());
        }
    }
}

/// When the request must be answered: `REQUEST_TIMEOUT` after the server took it.
#[derive(Clone, Copy, Debug)]
struct Deadline(tokio::time::Instant);

/// Answer 408 once the request's [`Deadline`] passes (one set at the server's door covers the
/// `_method` body read before routing); without one, `timeout` from now.
async fn within_deadline(req: Request, next: Next, timeout: Duration) -> Response {
    let deadline = req
        .extensions()
        .get::<Deadline>()
        .map_or_else(|| tokio::time::Instant::now() + timeout, |d| d.0);
    // `timeout_at` polls the handler once even when the deadline has passed.
    if tokio::time::Instant::now() >= deadline {
        return StatusCode::REQUEST_TIMEOUT.into_response();
    }
    match tokio::time::timeout_at(deadline, next.run(req)).await {
        Ok(response) => response,
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}

/// Wrap the router so `_method` spoofing happens before routing. The request's deadline
/// (`REQUEST_TIMEOUT`) starts here, so a body that trickles in for the override is cut off
/// like a slow handler (408).
pub(crate) fn with_method_override(
    router: axum::Router,
    limit: usize,
    timeout: Duration,
) -> impl Service<Request, Response = Response, Error = Infallible, Future: Send> + Clone + Send + 'static
{
    tower::service_fn(move |req: Request| {
        let router = router.clone();
        async move {
            let deadline = tokio::time::Instant::now() + timeout;
            let (parts, body) = req.into_parts();
            let head = parts.clone();
            let mut req = match tokio::time::timeout_at(
                deadline,
                method_override(Request::from_parts(parts, body), limit),
            )
            .await
            {
                Ok(req) => req,
                // The deadline has passed: the stack answers 408 without running the route.
                Err(_) => Request::from_parts(head, Body::empty()),
            };
            req.extensions_mut().insert(Deadline(deadline));
            router.oneshot(req).await
        }
    })
}

/// Refuse to serve an app or run its background work under `APP_ENV=testing`: that environment turns the CSRF
/// check off and, without `APP_KEY`, signs cookies and Spark state with a key that is public. The one refusal for
/// `serve`, `work`, [`serve_on`] and [`work`]; the console checks it before building the app.
pub(crate) fn refuse_testing(settings: &crate::config::Settings) -> Result<()> {
    if settings.env == "testing" {
        return Err(Error::internal(
            "APP_ENV is `testing`, which turns off CSRF checks and, without APP_KEY, signs cookies and Spark state \
             with the public test key: it is for `cargo test` only. Set APP_ENV to `production` (or `local` for \
             development).",
        ));
    }
    Ok(())
}

/// Serve the app until Ctrl-C / SIGTERM or [`App::shutdown`], then drain within the
/// shutdown budget.
///
/// # Errors
/// The address cannot be bound, or see [`serve_on`].
pub async fn serve(app: App, router: axum::Router) -> Result<()> {
    let settings = app.settings();
    let addr = format!("{}:{}", settings.host, settings.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| Error::internal(format!("cannot listen on {addr}: {e}")))?;
    tracing::info!(address = %format!("http://{addr}"), "server listening");
    serve_on(app, router, listener).await
}

/// [`serve`] on an already bound listener (tests bind port 0).
///
/// Connections are limited: a client has `SERVER_HEADER_TIMEOUT` to send a request's headers
/// (a connection, HTTP/1 or HTTP/2, with no request running that long is closed), at most
/// `SERVER_MAX_CONNECTIONS` connections are open at once (further clients wait to be accepted),
/// one client address holds at most `SERVER_MAX_CONNECTIONS_PER_IP` connections and runs at most
/// that many requests at once across them (peers in `TRUSTED_PROXIES` are not counted; a request
/// past the share gets `429`), an HTTP/2 connection runs at most `SERVER_MAX_STREAMS` requests at
/// once, and each request has `REQUEST_TIMEOUT` from when the server took it. An upgraded
/// connection whose route took its [`UpgradeHold`] stays counted against both connection caps
/// until the hold is dropped; HTTP/2 extended `CONNECT` is not offered (a request that tries it
/// anyway is reset or answered `501 Not Implemented`).
///
/// # Errors
/// The app has web routes and no usable `APP_KEY`, `APP_ENV` is `testing`, or an `on_serve`
/// or start hook fails.
pub async fn serve_on(
    app: App,
    router: axum::Router,
    listener: tokio::net::TcpListener,
) -> Result<()> {
    if let Some(reason) = app.key_error() {
        return Err(Error::internal(reason.to_owned()));
    }
    refuse_testing(app.settings())?;
    // Before anything starts: a failure here leaves no background work or signal task behind.
    let security = Arc::new(SecurityHeaders::from_settings(app.settings())?);
    let token = app.shutdown_token().clone();
    let budget = app.settings().shutdown_timeout;
    // Background work (Watchfire's agents) starts before the server accepts requests and stops
    // within the same budget, on the same token.
    app.mark_serving();
    app.run_serve_hooks().await?;
    let background = app.start_background().await?;
    let signals = {
        let token = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = shutdown_signal() => {
                    tracing::info!("shutdown signal received");
                    token.cancel();
                }
                () = token.cancelled() => {}
            }
        })
    };
    let settings = app.settings();
    let service = with_method_override(router, settings.body_limit, settings.request_timeout);
    let limits = ConnectionLimits {
        header_timeout: settings.server_header_timeout,
        max_connections: settings.server_max_connections,
        max_per_ip: settings.server_max_connections_per_ip,
        max_streams: settings.server_max_streams,
        trusted: app.trusted_proxies().clone(),
        security,
    };
    let http = async {
        let mut connections = tokio::task::JoinSet::new();
        accept(&listener, &service, &limits, &token, &mut connections).await;
        drop(listener);
        // Open connections finish their current request; idle ones close now (each connection
        // task watches the token).
        let drain = async { while connections.join_next().await.is_some() {} };
        if tokio::time::timeout(budget, drain).await.is_err() {
            tracing::warn!(
                budget_secs = budget.as_secs(),
                "requests still running after the shutdown budget; closing"
            );
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    };
    tokio::join!(
        http,
        wait_background(&token, budget, background),
        wait_tasks(&app, &token, budget)
    );
    token.cancel();
    signals.abort();
    let _ = signals.await;
    Ok(())
}

/// The server's limits on connections.
#[derive(Clone, Debug)]
struct ConnectionLimits {
    header_timeout: Duration,
    max_connections: usize,
    /// Zero: no per-client limit. Also the most requests one client runs at once (HTTP/2 multiplexes
    /// many requests over one connection; over HTTP/1 a connection runs one at a time).
    max_per_ip: usize,
    /// The most requests (streams) one HTTP/2 connection runs at once.
    max_streams: u32,
    /// Peers not counted per client (a proxy carries many clients).
    trusted: crate::client::TrustedProxies,
    /// The security headers of answers the server gives itself, before the stack.
    security: Arc<SecurityHeaders>,
}

/// How long a connection asked to close (idle, or the server shutting down) may stay open
/// with no request running before it is dropped.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// Open connections per client: an IPv4 address, an IPv6 client by its /64.
#[derive(Default)]
struct PerClient {
    open: std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, usize>>,
}

/// One connection counted for its client; uncounted when dropped.
struct ClientSlot {
    clients: Arc<PerClient>,
    key: std::net::IpAddr,
}

impl PerClient {
    /// A slot for a connection from `ip`, or `None` when the client already holds `max`.
    fn claim(self: &Arc<Self>, ip: std::net::IpAddr, max: usize) -> Option<ClientSlot> {
        let key = client_key(ip);
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = open.entry(key).or_insert(0);
        if *count >= max {
            return None;
        }
        *count += 1;
        Some(ClientSlot {
            clients: Arc::clone(self),
            key,
        })
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        let mut open = self
            .clients
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = open.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                open.remove(&self.key);
            }
        }
    }
}

/// The client a connection counts for: an IPv4(-mapped) address as it is, an IPv6 address by
/// its /64 (one customer's network).
fn client_key(ip: std::net::IpAddr) -> std::net::IpAddr {
    match ip.to_canonical() {
        std::net::IpAddr::V6(v6) => {
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64)))
        }
        v4 => v4,
    }
}

/// Counts one request of a connection as running until it is dropped: with its response
/// body, so a streamed response (server-sent events) is a running request until it ends.
struct Running(Arc<tokio::sync::watch::Sender<usize>>);

impl Running {
    fn start(open: &Arc<tokio::sync::watch::Sender<usize>>) -> Self {
        open.send_modify(|n| *n += 1);
        Self(Arc::clone(open))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n = n.saturating_sub(1));
    }
}

/// The connection an HTTP/1.1 upgrade request came on, put into the request's extensions by
/// the server; turned into an [`UpgradeHold`] by [`UpgradeHold::take`].
#[derive(Clone)]
struct Upgradable {
    /// The connection's running requests (the idle rule).
    open: Arc<tokio::sync::watch::Sender<usize>>,
    /// The connection's holds (the connection task waits for them after the upgrade).
    held: Arc<tokio::sync::watch::Sender<usize>>,
}

/// Keeps an upgraded connection (a WebSocket, or any other HTTP/1.1 `Upgrade`) counted by the
/// server for as long as it is held.
///
/// When hyper hands an upgraded socket over, the HTTP connection ends; without a hold, the
/// server gives the connection's `SERVER_MAX_CONNECTIONS` permit and its
/// `SERVER_MAX_CONNECTIONS_PER_IP` slot back at that moment, while the socket lives on. A route
/// that serves upgraded sockets takes the hold from the upgrade request and moves it into the
/// task that owns the socket; until the hold is dropped:
///
/// - the permit and the client slot stay taken, so sockets count against both limits;
/// - the connection counts as a running request, so the idle rule (`SERVER_HEADER_TIMEOUT`)
///   never closes it;
/// - the server's shutdown drain waits for it, within `SHUTDOWN_TIMEOUT`. The owner closes the
///   socket when [`App::shutdown_token`](crate::App::shutdown_token) is cancelled; after the
///   budget the server stops counting it and returns.
///
/// Take the hold only on the path that answers `101 Switching Protocols`, after every check
/// (authorization, the handshake headers, rate limits): a handler that answers anything else
/// does not take it, or drops it before returning, because a hold out on a connection that
/// stays open for more requests keeps that connection counted as busy. Drop the hold when the
/// socket ends, or when the upgrade fails (`hyper::upgrade::on` returns an error, as it does
/// when the client goes away before the upgrade completes; the server also releases every hold
/// of a connection that ended without its upgrade). Routes that never take it work as they do
/// without one.
///
/// Only HTTP/1.1 requests that ask for an upgrade (or `CONNECT`) and came through
/// [`serve`] / [`serve_on`] offer a hold. HTTP/2 extended `CONNECT` (RFC 8441 WebSockets) is
/// not offered by the server (a request that tries it anyway is reset, or answered
/// `501 Not Implemented` before routing), so HTTP/2 requests never offer one; clients connect over
/// HTTP/1.1 (as Caddy and nginx do when they proxy WebSockets).
///
/// ```no_run
/// use smeltery_core::UpgradeHold;
/// use axum::{body::Body, extract::Request, response::Response};
///
/// async fn socket(mut req: Request) -> Response {
///     // … every check first; a refusal returns here without taking the hold …
///     let upgrade = hyper::upgrade::on(&mut req);
///     let hold = UpgradeHold::take(req.extensions_mut());
///     tokio::spawn(async move {
///         let _hold = hold; // counted until this task ends
///         if let Ok(upgraded) = upgrade.await {
///             // … serve the socket, and close it on the app's shutdown token …
///             # drop(upgraded);
///         }
///     });
///     Response::builder()
///         .status(101)
///         .header("upgrade", "websocket")
///         .header("connection", "upgrade")
///         .body(Body::empty())
///         .unwrap_or_default()
/// }
/// ```
#[must_use = "the connection is counted only while the hold is kept"]
pub struct UpgradeHold {
    _running: Running,
    _held: Running,
}

impl UpgradeHold {
    /// Take the hold out of an upgrade request's extensions (`req.extensions_mut()`, or
    /// `parts.extensions`). `None` when the request offers none: not an HTTP/1.1 upgrade or
    /// `CONNECT`, not served by [`serve`] / [`serve_on`] (the test client), or taken already.
    #[must_use = "the connection is counted only while the hold is kept"]
    pub fn take(extensions: &mut http::Extensions) -> Option<Self> {
        extensions
            .remove::<Upgradable>()
            .map(|Upgradable { open, held }| Self {
                _running: Running::start(&open),
                _held: Running::start(&held),
            })
    }
}

impl std::fmt::Debug for UpgradeHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpgradeHold").finish_non_exhaustive()
    }
}

/// The server's answer to HTTP/2 extended `CONNECT` (RFC 8441): not supported, use HTTP/1.1.
/// Built before the stack, so it gets the security headers here.
fn refuse_extended_connect(security: &SecurityHeaders) -> Response {
    let mut res = Response::new(Body::from(
        "WebSockets over HTTP/2 (extended CONNECT) are not supported; connect over HTTP/1.1\n",
    ));
    *res.status_mut() = StatusCode::NOT_IMPLEMENTED;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    security.apply(res)
}

/// The server's answer to a request past its client's share of running requests
/// (`SERVER_MAX_CONNECTIONS_PER_IP`, counted across the client's connections). Built before the
/// stack, so it gets the security headers here.
fn refuse_busy_client(security: &SecurityHeaders) -> Response {
    let mut res = Response::new(Body::from(
        "Too many requests running from this client at once
",
    ));
    *res.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    security.apply(res)
}

/// Whether `req` is an HTTP/2 extended `CONNECT` (it carries a `:protocol`).
fn is_extended_connect<B>(req: &http::Request<B>) -> bool {
    req.method() == http::Method::CONNECT
        && req.extensions().get::<hyper::ext::Protocol>().is_some()
}

/// Whether `req` may be upgraded: an HTTP/1.x request to which hyper attached an `OnUpgrade`
/// (an HTTP/1.1 request with an `Upgrade` header, or an HTTP/1.x `CONNECT`).
fn offers_upgrade<B>(req: &http::Request<B>) -> bool {
    req.version() <= http::Version::HTTP_11
        && req
            .extensions()
            .get::<hyper::upgrade::OnUpgrade>()
            .is_some()
}

/// Resolves once the connection has had no running request for `wait`.
async fn idle_for(open: &mut tokio::sync::watch::Receiver<usize>, wait: Duration) {
    loop {
        if open.wait_for(|n| *n == 0).await.is_err() {
            return;
        }
        if tokio::time::timeout(wait, open.wait_for(|n| *n > 0))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Accept connections until the token is cancelled, each served on its own task in
/// `connections`.
async fn accept<S>(
    listener: &tokio::net::TcpListener,
    service: &S,
    limits: &ConnectionLimits,
    token: &tokio_util::sync::CancellationToken,
    connections: &mut tokio::task::JoinSet<()>,
) where
    S: Service<Request, Response = Response, Error = Infallible, Future: Send>
        + Clone
        + Send
        + 'static,
{
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};

    let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        // Also the idle time between two requests (hyper restarts it when it waits for the
        // next request head).
        .header_read_timeout(limits.header_timeout);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(Some(limits.header_timeout))
        .keep_alive_timeout(Duration::from_secs(20))
        // One HTTP/2 connection runs at most this many requests at once (hyper's default is 200),
        // so it is not worth many HTTP/1 connections. Extended CONNECT (RFC 8441) is not enabled:
        // the server does not advertise it, so clients use HTTP/1.1 for WebSockets, and h2 resets a
        // request that sends `:protocol` anyway (D-402).
        .max_concurrent_streams(limits.max_streams);
    let permits = Arc::new(tokio::sync::Semaphore::new(limits.max_connections));
    let clients = Arc::new(PerClient::default());
    // Requests running per client, across all its connections: over HTTP/1 a connection runs one
    // request at a time, so this bounds what HTTP/2 multiplexing adds (W1-01).
    let requests = Arc::new(PerClient::default());
    loop {
        // Reap finished connections so the set stays as large as the open ones.
        while connections.try_join_next().is_some() {}
        let permit = tokio::select! {
            permit = Arc::clone(&permits).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return,
            },
            () = token.cancelled() => return,
        };
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) if is_connection_error(&e) => continue,
                Err(e) => {
                    // Out of file descriptors, for instance: wait instead of spinning.
                    tracing::error!(error = %e, "accepting a connection failed");
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(1)) => continue,
                        () = token.cancelled() => return,
                    }
                }
            },
            () = token.cancelled() => return,
        };
        // One client must not hold every slot: past its share, a connection is closed at once.
        let slot = if limits.max_per_ip == 0 || limits.trusted.trusts_peer(peer.ip()) {
            None
        } else {
            match clients.claim(peer.ip(), limits.max_per_ip) {
                Some(slot) => Some(slot),
                None => {
                    tracing::debug!(client = %client_key(peer.ip()), "too many connections from one client; closing");
                    drop(stream);
                    continue;
                }
            }
        };
        // A client counted per connection is also counted per running request.
        let per_request = slot
            .is_some()
            .then(|| (Arc::clone(&requests), limits.max_per_ip));
        let _ = stream.set_nodelay(true);
        let (open, mut open_rx) = tokio::sync::watch::channel(0usize);
        let open = Arc::new(open);
        let (held, mut held_rx) = tokio::sync::watch::channel(0usize);
        let held = Arc::new(held);
        let service = {
            let service = service.clone();
            let security = Arc::clone(&limits.security);
            tower::service_fn(move |req: http::Request<hyper::body::Incoming>| {
                let running = Running::start(&open);
                let security = Arc::clone(&security);
                // Past its share of running requests a client gets a 429 for this one (HTTP/2
                // streams; an HTTP/1 client cannot run more requests than it has connections).
                let (request_slot, busy) = match &per_request {
                    Some((requests, max)) => match requests.claim(peer.ip(), *max) {
                        Some(slot) => (Some(slot), false),
                        None => (None, true),
                    },
                    None => (None, false),
                };
                // Refused at the door (D-402): a WebSocket over HTTP/2 is a stream of a
                // multiplexed connection, which no hold or limit here accounts for. h2 itself
                // resets `:protocol` requests while extended CONNECT is not enabled; this stays
                // as the second line.
                let refused = is_extended_connect(&req);
                let mut req = req.map(Body::new);
                req.extensions_mut()
                    .insert(axum::extract::ConnectInfo(peer));
                if offers_upgrade(&req) {
                    req.extensions_mut().insert(Upgradable {
                        open: Arc::clone(&open),
                        held: Arc::clone(&held),
                    });
                }
                let response = (!refused && !busy).then(|| service.clone().oneshot(req));
                async move {
                    let res = match response {
                        Some(response) => response.await?,
                        None if busy => {
                            tracing::debug!(client = %client_key(peer.ip()), "too many requests running for one client; 429");
                            refuse_busy_client(&security)
                        }
                        None => {
                            tracing::debug!(client = %peer, "HTTP/2 extended CONNECT refused");
                            refuse_extended_connect(&security)
                        }
                    };
                    // The request runs (and keeps its client's request slot) until its body is
                    // sent or the client goes away.
                    Ok::<_, Infallible>(res.map(|body| {
                        Body::new(body.map_frame(move |frame| {
                            let _running = &running;
                            let _request_slot = &request_slot;
                            frame
                        }))
                    }))
                }
            })
        };
        let connection = builder
            .serve_connection_with_upgrades(
                TokioIo::new(stream),
                hyper_util::service::TowerToHyperService::new(service),
            )
            .into_owned();
        let header_timeout = limits.header_timeout;
        let token = token.clone();
        connections.spawn(async move {
            let _permit = permit;
            let _slot = slot;
            // The connection is served inside this block and dropped when it ends. Dropping
            // it matters for holds: a connection that ended before its upgrade completed (the
            // client went away, an error, a shutdown) still owns hyper's pending upgrade and
            // the handler future that has not finished; dropping them fails the route's
            // `hyper::upgrade::on` and drops a hold the handler kept, so every hold of an
            // upgrade that never happened is released.
            let serve = async move {
                tokio::pin!(connection);
                // A connection with no running request for `SERVER_HEADER_TIMEOUT` is
                // closed: before its first request (hyper's header timeout starts only with
                // the first byte), between HTTP/1 requests, and on HTTP/2, which has no idle
                // timeout of its own (its keep-alive pings keep a silent client's connection
                // open).
                tokio::select! {
                    result = connection.as_mut() => {
                        if let Err(e) = result {
                            tracing::trace!(error = %e, "connection ended with an error");
                        }
                        return;
                    }
                    () = idle_for(&mut open_rx, header_timeout) => {
                        tracing::debug!("no request within SERVER_HEADER_TIMEOUT; closing the connection");
                    }
                    () = token.cancelled() => {}
                }
                // HTTP/2 gets a GOAWAY, HTTP/1 closes after its current response; a request
                // still running finishes (shutdown: within the budget). Then the connection is
                // dropped once it has had no running request for a moment.
                connection.as_mut().graceful_shutdown();
                tokio::select! {
                    _ = connection.as_mut() => {}
                    () = idle_for(&mut open_rx, CLOSE_GRACE) => {}
                }
            };
            serve.await;
            // An upgraded socket outlives its HTTP connection: a route that took the
            // `UpgradeHold` keeps it counted (this task, and so the permit and the client slot,
            // lives until every hold is dropped; the shutdown drain waits for it within the
            // budget). Without a hold this returns at once.
            let _ = held_rx.wait_for(|n| *n == 0).await;
        });
    }
}

/// Errors of one connection, which leave the listener working (`axum::serve` skips them too).
fn is_connection_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
}

/// After the token is cancelled, wait for the app's request-spawned work (password reset
/// mails) at most `budget`.
async fn wait_tasks(app: &App, token: &tokio_util::sync::CancellationToken, budget: Duration) {
    token.cancelled().await;
    let tasks = app.tasks();
    tasks.close();
    if tokio::time::timeout(budget, tasks.wait()).await.is_err() {
        tracing::warn!(
            budget_secs = budget.as_secs(),
            "background mail still running after the shutdown budget; exiting"
        );
    }
}

/// Run the app's background work without the HTTP server (`work`) until Ctrl-C / SIGTERM or
/// [`App::shutdown`], then stop it within the shutdown budget. Returns `false` when the app has
/// no background work.
///
/// # Errors
/// `APP_ENV` is `testing`, or a start hook fails.
pub async fn work(app: &App) -> Result<bool> {
    refuse_testing(app.settings())?;
    let Some(background) = app.start_background().await? else {
        return Ok(false);
    };
    let token = app.shutdown_token().clone();
    let budget = app.settings().shutdown_timeout;
    tracing::info!(
        "{}",
        work_banner(std::io::stdin().is_terminal() || std::io::stdout().is_terminal())
    );
    tokio::select! {
        () = shutdown_signal() => {
            tracing::info!("shutdown signal received");
            token.cancel();
        }
        () = token.cancelled() => {}
    }
    tokio::join!(
        wait_background(&token, budget, Some(background)),
        wait_tasks(app, &token, budget)
    );
    Ok(true)
}

/// The line `work` logs at start: the Ctrl-C hint only for someone at a terminal (not under systemd or Docker, where
/// the service manager stops the process).
fn work_banner(interactive: bool) -> &'static str {
    if interactive {
        "working (background only); press Ctrl-C to stop"
    } else {
        "working (background only)"
    }
}

/// After the token is cancelled, wait for the background work at most `budget`.
async fn wait_background(
    token: &tokio_util::sync::CancellationToken,
    budget: Duration,
    background: Option<crate::app::Background>,
) {
    let Some(background) = background else {
        return;
    };
    let waiting = background.wait();
    tokio::pin!(waiting);
    // The work may also end on its own before shutdown; then there is nothing left to wait for.
    tokio::select! {
        () = &mut waiting => return,
        () = token.cancelled() => {}
    }
    if tokio::time::timeout(budget, waiting).await.is_err() {
        tracing::warn!(
            budget_secs = budget.as_secs(),
            "background work still running after the shutdown budget; exiting"
        );
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

/// Send one request through the full stack (method spoofing included) and collect the
/// response body. Used by the test client.
pub(crate) async fn call(
    router: &axum::Router,
    limit: usize,
    timeout: Duration,
    req: Request<Body>,
) -> (http::response::Parts, bytes::Bytes) {
    let service = with_method_override(router.clone(), limit, timeout);
    let response = match service.oneshot(req).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    let (parts, body) = response.into_parts();
    let bytes = body
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .unwrap_or_default();
    (parts, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_count_per_client_and_are_given_back() {
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        let clients = Arc::new(PerClient::default());
        let a = clients.claim(ip("192.0.2.1"), 2);
        let b = clients.claim(ip("::ffff:192.0.2.1"), 2);
        assert!(a.is_some() && b.is_some());
        assert!(
            clients.claim(ip("192.0.2.1"), 2).is_none(),
            "the mapped address is the same client"
        );
        // An IPv6 client counts by its /64.
        let c = clients.claim(ip("2001:db8:1:2::1"), 1);
        assert!(c.is_some());
        assert!(clients.claim(ip("2001:db8:1:2:ffff::9"), 1).is_none());
        assert!(clients.claim(ip("2001:db8:1:3::1"), 1).is_some());
        drop(a);
        assert!(clients.claim(ip("192.0.2.1"), 2).is_some());
        drop((b, c));
        assert_eq!(
            clients.len(),
            0,
            "clients without connections are forgotten"
        );
    }

    #[test]
    fn answers_given_before_the_stack_carry_the_security_headers() {
        let security = SecurityHeaders::from_settings(&crate::config::Settings::from_env())
            .expect("default settings");
        let busy = refuse_busy_client(&security);
        assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(busy.headers()[header::RETRY_AFTER], "1");
        let connect = refuse_extended_connect(&security);
        assert_eq!(connect.status(), StatusCode::NOT_IMPLEMENTED);
        for res in [busy, connect] {
            assert_eq!(res.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert!(res.headers().contains_key(header::REFERRER_POLICY));
        }
    }

    #[test]
    fn static_paths_with_aliasing_segments_are_refused() {
        for path in [
            "/storage./x.html",
            "/storage%2e/x.html",
            "/storage%20/x",
            "/storage::$DATA/x",
            "/storage%3a/x",
            "/./storage/x",
            "/a%5Cb",
            "/x.html.",
            "/x%00",
        ] {
            assert!(!is_plain_static_path(path), "{path}");
        }
        for path in [
            "/",
            "/storage/x.html",
            "/assets/app.v2.css",
            "//double",
            "/a%20b.txt",
        ] {
            assert!(is_plain_static_path(path), "{path}");
        }
    }

    #[test]
    fn a_request_that_did_not_come_through_the_server_offers_no_hold() {
        let mut extensions = http::Extensions::new();
        assert!(UpgradeHold::take(&mut extensions).is_none());
        let (open, _) = tokio::sync::watch::channel(0usize);
        let (held, held_rx) = tokio::sync::watch::channel(0usize);
        extensions.insert(Upgradable {
            open: Arc::new(open),
            held: Arc::new(held),
        });
        let hold = UpgradeHold::take(&mut extensions).unwrap();
        assert_eq!(*held_rx.borrow(), 1);
        assert!(UpgradeHold::take(&mut extensions).is_none(), "taken once");
        drop(hold);
        assert_eq!(*held_rx.borrow(), 0);
    }

    #[test]
    fn work_mentions_ctrl_c_only_at_a_terminal() {
        assert!(work_banner(true).contains("Ctrl-C"));
        assert!(!work_banner(false).contains("Ctrl-C"));
    }

    fn route(path: &str) -> RouteInfo {
        RouteInfo {
            methods: vec!["GET".into()],
            path: path.into(),
            name: None,
            middleware: Vec::new(),
            api: false,
        }
    }

    #[test]
    fn log_uris_hide_secret_segments_and_query_values() {
        let log = LogUri::new(&[
            route("/reset-password/{token}"),
            route("/email/verify/{id}/{hash}"),
            route("/files/{*apiKey}"),
            route("/posts/{post}"),
        ]);
        let show = |uri: &'static str| log.render(&http::Uri::from_static(uri));
        assert_eq!(
            show("/reset-password/abc123?email=a%40b.test"),
            "/reset-password/[redacted]?email=[redacted]"
        );
        assert_eq!(show("/email/verify/7/f00d"), "/email/verify/7/[redacted]");
        assert_eq!(show("/files/a/b/c"), "/files/[redacted]");
        // Query items without a value can be secrets too (`?<signature>`).
        assert_eq!(
            show("/posts/9?page=2&flag"),
            "/posts/9?page=[redacted]&[redacted]"
        );
        assert_eq!(
            show("/reset-password/abc?SECRET4"),
            "/reset-password/[redacted]?[redacted]"
        );
        // Mangled links (a 404 for the router) still match: extra slashes, other case.
        assert_eq!(
            show("/reset-password/SECRET1/"),
            "/reset-password/[redacted]"
        );
        assert_eq!(
            show("//reset-password/SECRET2"),
            "/reset-password/[redacted]"
        );
        assert_eq!(
            show("/Reset-Password/SECRET5"),
            "/Reset-Password/[redacted]"
        );
        // Percent-encoded literals and separators are decoded before matching.
        assert_eq!(
            show("/reset%2Dpassword/SECRET6"),
            "/reset-password/[redacted]"
        );
        assert_eq!(
            show("/reset-password%2FSECRET7"),
            "/reset-password/[redacted]"
        );
        assert_eq!(show("/files/a%2Fb"), "/files/[redacted]");
        // Paths that match no secret route are shown as they are.
        assert_eq!(show("/reset-password"), "/reset-password");
        assert_eq!(show("/reset-password/a/b"), "/reset-password/a/b");
        assert_eq!(show("/posts//9/"), "/posts//9/");
        assert_eq!(show("/"), "/");
    }
}
