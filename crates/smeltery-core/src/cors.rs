//! CORS for an explicit list of origins (`CORS_ALLOWED_ORIGINS`), on the paths of `CORS_PATHS` (D-417).
//!
//! It exists for clients that run on another origin and call the app's API with a bearer token: hybrid mobile and
//! desktop apps (Capacitor `capacitor://localhost`, Tauri `tauri://localhost` / `http://tauri.localhost`, Electron
//! `app://…`) and front ends on another host. It is deliberately small:
//! - origins are compared exactly (scheme, host, port); `*` is refused at boot; `null` only when listed literally;
//! - credentials are never allowed (no `Access-Control-Allow-Credentials`): a cross-origin page cannot use the
//!   session cookie, so the API is reached with a bearer token, which the page has to hold itself;
//! - a preflight (`OPTIONS` with `Access-Control-Request-Method`) from a listed origin to a covered path is answered
//!   `204` before routing, with the requested method (if it is a standard one) and a fixed list of request headers;
//! - other requests from a listed origin to a covered path run as usual and get `Access-Control-Allow-Origin` with
//!   that origin; every answer on a covered path gets `Vary: Origin`, whatever the origin, so a shared cache keeps
//!   one answer per origin;
//! - the layer sits outside the request timeout, the panic handler and the error pages, so their answers (408, 500,
//!   413, rendered errors) carry the header too.
//!
//! `CORS_PATHS` entries are path prefixes compared as text: end them with `/` (`/api/`, the default), or `/api`
//! would cover `/apiary` too (a prefix without one is warned about at boot).
//!
//! Without `CORS_ALLOWED_ORIGINS` nothing changes: no header is added and preflights are routed like any request.
//!
//! [`normalize_origin`] is the framework's one origin parser (Anvil's socket `Origin` policy uses it too).

use axum::response::{IntoResponse, Response};
use http::{HeaderMap, HeaderValue, Method, StatusCode, header};

use crate::error::{Error, Result};
use crate::middleware::{Next, Request};

/// The request headers a preflight may ask for (`Access-Control-Allow-Headers`).
pub const ALLOWED_HEADERS: &str =
    "Accept, Authorization, Content-Type, X-Requested-With, X-Socket-ID, X-CSRF-TOKEN";

/// How long a browser may cache a preflight answer (`Access-Control-Max-Age`, seconds).
const MAX_AGE: &str = "600";

/// The CORS rules of the app (`CORS_ALLOWED_ORIGINS`, `CORS_PATHS`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Cors {
    /// Listed origins, normalized (`scheme://host[:port]`, lowercase, default ports dropped), or `null`.
    origins: Vec<String>,
    /// Path prefixes the rules cover.
    paths: Vec<String>,
}

/// `text` as an origin in a normal form: lowercase `scheme://host` with `:port` unless it is the scheme's default
/// (80 for `http` / `ws`, 443 for `https` / `wss`); `null` as `null`; `None` for anything that is not exactly an
/// origin (a path, a query, user info, a `*` anywhere). What a list does with `null` and `*` is the list's rule:
/// `CORS_ALLOWED_ORIGINS` accepts `null` written literally and refuses `*`; Anvil's `ANVIL_ALLOWED_ORIGINS` accepts
/// both.
///
/// ```
/// use smeltery_core::cors::normalize_origin;
///
/// assert_eq!(normalize_origin("HTTPS://App.Example.com:443/").as_deref(), Some("https://app.example.com"));
/// assert_eq!(normalize_origin("capacitor://localhost").as_deref(), Some("capacitor://localhost"));
/// assert_eq!(normalize_origin("https://*.example.com"), None);
/// ```
pub fn normalize_origin(text: &str) -> Option<String> {
    let text = text.trim();
    if text == "null" {
        return Some("null".to_owned());
    }
    let (scheme, rest) = text.split_once("://")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let scheme_ok = scheme
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    if !scheme_ok
        || rest.is_empty()
        || rest
            .bytes()
            .any(|b| matches!(b, b'/' | b'?' | b'#' | b'@' | b'\\' | b'*') || !b.is_ascii_graphic())
    {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let (host, port) = if let Some(after) = rest.strip_prefix('[') {
        let (inside, tail) = after.split_once(']')?;
        let port = match tail {
            "" => None,
            p => Some(parse_port(p.strip_prefix(':')?)?),
        };
        (format!("[{inside}]"), port)
    } else {
        match rest.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), Some(parse_port(port)?)),
            None => (rest.to_owned(), None),
        }
    };
    // A host as browsers write it: no trailing dot, a `:` only inside IPv6 brackets.
    if host.is_empty()
        || host == "[]"
        // A trailing dot only for web schemes (a browser never sends one); app schemes may use any host, such as
        // Electron's `app://.`.
        || (host.ends_with('.') && matches!(scheme.as_str(), "http" | "https" | "ws" | "wss"))
        || (host.contains(':') && !host.starts_with('['))
    {
        return None;
    }
    let default = match scheme.as_str() {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    };
    let host = host.to_ascii_lowercase();
    Some(match port.filter(|p| Some(*p) != default) {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

/// A port as browsers write it: digits without a leading zero, 1 to 65535.
fn parse_port(text: &str) -> Option<u16> {
    if text.is_empty() || text.starts_with('0') || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|p| *p != 0)
}

/// The origin of a URL (`APP_URL`): its scheme and authority, user info dropped, path, query and fragment ignored;
/// normalized like [`normalize_origin`]. `None` when the URL has no scheme or host.
///
/// ```
/// use smeltery_core::cors::origin_of_url;
///
/// assert_eq!(
///     origin_of_url("https://user:pw@App.example.com:8443/sub?x#f").as_deref(),
///     Some("https://app.example.com:8443")
/// );
/// ```
pub fn origin_of_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..end)?;
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    normalize_origin(&format!("{scheme}://{host}")).filter(|o| o != "null")
}

/// The scheme, host and port of an origin [`normalize_origin`] made (`None` for `null`); the port is absent when it
/// is the scheme's default.
pub fn origin_parts(origin: &str) -> Option<(&str, &str, Option<u16>)> {
    let (scheme, rest) = origin.split_once("://")?;
    if rest.ends_with(']') || !rest.contains(':') {
        return Some((scheme, rest, None));
    }
    let (host, port) = rest.rsplit_once(':')?;
    Some((scheme, host, port.parse().ok()))
}

impl Cors {
    /// The rules for these settings.
    ///
    /// # Errors
    /// An entry of `CORS_ALLOWED_ORIGINS` is not an exact origin (`*` included), or an entry of `CORS_PATHS` does
    /// not start with `/`.
    pub(crate) fn from_settings(settings: &crate::config::Settings) -> Result<Self> {
        let mut origins = Vec::new();
        for entry in split(&settings.cors_allowed_origins) {
            let origin = normalize_origin(entry).ok_or_else(|| {
                Error::internal(format!(
                    "CORS_ALLOWED_ORIGINS: `{entry}` is not an origin (scheme://host[:port], or `null`); `*` is \
                     not accepted: list the origins"
                ))
            })?;
            if origin == "null" {
                tracing::warn!(
                    "CORS_ALLOWED_ORIGINS lists `null`: sandboxed frames and pages opened from files on any site \
                     send that origin too"
                );
            }
            origins.push(origin);
        }
        let mut paths = Vec::new();
        for entry in split(&settings.cors_paths) {
            if !entry.starts_with('/') {
                return Err(Error::internal(format!(
                    "CORS_PATHS: `{entry}` is not a path prefix (it starts with `/`)"
                )));
            }
            if !entry.ends_with('/') {
                tracing::warn!(
                    prefix = entry,
                    "CORS_PATHS: a prefix without a trailing `/` also covers paths that only start with the same \
                     letters (`/api` covers `/apiary`)"
                );
            }
            paths.push(entry.to_owned());
        }
        Ok(Self { origins, paths })
    }

    /// Whether any origin is listed.
    pub(crate) fn enabled(&self) -> bool {
        !self.origins.is_empty()
    }

    /// Whether the rules cover `path`.
    fn covers(&self, path: &str) -> bool {
        self.paths.iter().any(|p| path.starts_with(p.as_str()))
    }

    /// The listed origin a request comes from (one `Origin` header, listed).
    fn allowed_origin(&self, headers: &HeaderMap) -> Option<HeaderValue> {
        let mut values = headers.get_all(header::ORIGIN).iter();
        let first = values.next()?;
        if values.next().is_some() {
            return None;
        }
        let origin = normalize_origin(first.to_str().ok()?)?;
        self.origins.contains(&origin).then(|| first.clone())
    }

    /// The middleware: answer preflights, add the headers to answers.
    pub(crate) async fn handle(&self, req: Request, next: Next) -> Response {
        if !self.covers(req.uri().path()) {
            return next.run(req).await;
        }
        let Some(origin) = self.allowed_origin(req.headers()) else {
            // Not for this origin, but the answer depends on the origin all the same: caches must keep them apart.
            let mut res = next.run(req).await;
            res.headers_mut()
                .append(header::VARY, HeaderValue::from_static("Origin"));
            return res;
        };
        let requested = req
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|v| v.to_str().ok())
            .and_then(|m| m.parse::<Method>().ok());
        if req.method() == Method::OPTIONS
            && let Some(method) = requested
        {
            return preflight(&origin, &method);
        }
        let mut res = next.run(req).await;
        let headers = res.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.append(header::VARY, HeaderValue::from_static("Origin"));
        res
    }
}

fn split(list: &str) -> impl Iterator<Item = &str> {
    list.split(',').map(str::trim).filter(|e| !e.is_empty())
}

/// The answer to a preflight from an allowed origin.
fn preflight(origin: &HeaderValue, method: &Method) -> Response {
    let standard = [
        Method::GET,
        Method::HEAD,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
    ];
    let mut res = StatusCode::NO_CONTENT.into_response();
    let headers = res.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    if standard.contains(method)
        && let Ok(value) = HeaderValue::from_str(method.as_str())
    {
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, value);
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(ALLOWED_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static(MAX_AGE),
    );
    headers.append(header::VARY, HeaderValue::from_static("Origin"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_normalize_exactly() {
        for (text, expected) in [
            ("capacitor://localhost", Some("capacitor://localhost")),
            ("tauri://localhost", Some("tauri://localhost")),
            ("http://tauri.localhost", Some("http://tauri.localhost")),
            (
                "HTTPS://App.Example.com:443",
                Some("https://app.example.com"),
            ),
            ("https://app.example.com/", Some("https://app.example.com")),
            ("http://127.0.0.1:5173", Some("http://127.0.0.1:5173")),
            ("app://bundle", Some("app://bundle")),
            ("null", Some("null")),
            ("*", None),
            ("https://*.example.com", None),
            ("https://app.example.com/path", None),
            ("https://user@app.example.com", None),
            ("app.example.com", None),
            ("https://", None),
            ("wss://app.example.com:443", Some("wss://app.example.com")),
            ("1http://app.example.com", None),
            ("https://[::1]:8000", Some("https://[::1]:8000")),
            ("https://app.example.com:08443", None),
            ("https://app.example.com:0", None),
            ("https://app.example.com.", None),
            ("app://.", Some("app://.")),
            ("app://-", Some("app://-")),
            ("https://a:b:8000", None),
        ] {
            assert_eq!(normalize_origin(text).as_deref(), expected, "{text}");
        }
    }

    #[test]
    fn urls_give_their_origin_and_origins_their_parts() {
        assert_eq!(
            origin_of_url("https://app.example.com/app").as_deref(),
            Some("https://app.example.com")
        );
        assert_eq!(origin_of_url("not a url"), None);
        assert_eq!(
            origin_parts("https://app.example.com"),
            Some(("https", "app.example.com", None))
        );
        assert_eq!(
            origin_parts("http://127.0.0.1:8000"),
            Some(("http", "127.0.0.1", Some(8000)))
        );
        assert_eq!(
            origin_parts("http://[::1]:8000"),
            Some(("http", "[::1]", Some(8000)))
        );
        assert_eq!(origin_parts("http://[::1]"), Some(("http", "[::1]", None)));
        assert_eq!(origin_parts("null"), None);
    }
}
