//! HTTP types handlers use.
//!
//! The request extractors and response helpers are Axum's and `http`'s, re-exported on
//! purpose: they are the standard Rust web types and Smeltery adds nothing by wrapping
//! them.

pub use axum::Json;
pub use axum::extract::{Form, FromRequestParts, Path, Query, Request};
pub use axum::response::{Html, IntoResponse, Redirect};
pub use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header, request};

pub use crate::client::{ClientInfo, TrustedProxies};
pub use crate::error::wants_json;
pub use crate::session::web::is_inertia;
pub use crate::upload::{UploadedFile, is_safe_extension, stored_extension};

/// The previous page, as a handler argument: the `Referer` when it is on this site, else `/`.
///
/// ```
/// use smeltery_core::http::{Back, Redirect};
///
/// async fn cancel(back: Back) -> Redirect {
///     back.redirect()
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Back {
    url: String,
}

impl Back {
    /// The previous URL from these request headers: the `Referer` when its host is the `Host`
    /// header.
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        Self::with_hosts(headers, &[host])
    }

    /// The previous URL of a request whose client is `client`: the `Referer` when its host is
    /// the client's host ([`ClientInfo::host`], so `X-Forwarded-Host` from a trusted proxy) or
    /// the host of `app_url` (`APP_URL`).
    pub fn for_client(headers: &HeaderMap, client: &ClientInfo, app_url: &str) -> Self {
        let app_host = app_url
            .strip_prefix("https://")
            .or_else(|| app_url.strip_prefix("http://"))
            .and_then(|rest| rest.split(['/', '?', '#']).next())
            .unwrap_or("");
        Self::with_hosts(headers, &[client.host().unwrap_or(""), app_host])
    }

    fn with_hosts(headers: &HeaderMap, hosts: &[&str]) -> Self {
        let referer = headers
            .get(header::REFERER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        Self {
            url: same_origin_path(referer, hosts).unwrap_or_else(|| "/".to_owned()),
        }
    }

    /// The URL (a path with its query).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// A 303 redirect to it.
    pub fn redirect(&self) -> Redirect {
        Redirect::to(&self.url)
    }
}

/// Whether `url` is a path on this site that is safe as a redirect target: it starts with
/// exactly one `/` and holds only visible ASCII characters other than `\` (no whitespace, no
/// control character, nothing outside ASCII).
///
/// Browsers read `//host/x` and `/\host/x` as another host, and they drop tabs and line
/// breaks from a `Location` before reading it (so `/<TAB>/host/x` becomes `//host/x`); a URL
/// with a scheme (`https:`, `javascript:`) never starts with `/`. Request paths are ASCII
/// (anything else arrives percent-encoded), and browsers decode raw non-ASCII bytes in a
/// `Location` inconsistently, so only the percent-encoded form is accepted.
///
/// The framework's redirect helpers, the intended URL, Sparks redirects and Alloy share this rule.
///
/// ```
/// use smeltery_core::http::is_local_path;
///
/// assert!(is_local_path("/posts?page=2"));
/// assert!(!is_local_path("//evil.example/x"));
/// assert!(!is_local_path(r"/\evil.example/x"));
/// assert!(!is_local_path("/\t/evil.example/x"));
/// assert!(!is_local_path("https://evil.example/"));
/// ```
pub fn is_local_path(url: &str) -> bool {
    url.starts_with('/')
        && !url.starts_with("//")
        && url.bytes().all(|b| b.is_ascii_graphic() && b != b'\\')
}

/// The path and query of `referer` when it points at one of `hosts` (or is a local path).
fn same_origin_path(referer: &str, hosts: &[&str]) -> Option<String> {
    if referer.starts_with('/') {
        return is_local_path(referer).then(|| referer.to_owned());
    }
    let rest = referer
        .strip_prefix("https://")
        .or_else(|| referer.strip_prefix("http://"))?;
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (rest.get(..i)?, rest.get(i..)?),
        None => (rest, "/"),
    };
    if !hosts
        .iter()
        .any(|h| !h.is_empty() && authority.eq_ignore_ascii_case(h))
    {
        return None;
    }
    let path = path.split('#').next().unwrap_or("/");
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    // `//host/x` or `/\host/x` as a Location is protocol-relative: off-site.
    is_local_path(&path).then_some(path)
}

impl axum::extract::FromRequestParts<crate::App> for Back {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        app: &crate::App,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self::for_client(
            &parts.headers,
            &ClientInfo::from_parts(parts),
            &app.settings().url,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_stays_on_this_site() {
        let back = |referer: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, HeaderValue::from_static("app.test"));
            if !referer.is_empty() {
                h.insert(header::REFERER, HeaderValue::from_str(referer).unwrap());
            }
            Back::from_headers(&h).url().to_owned()
        };
        assert_eq!(
            back("http://app.test/posts/1/edit?x=1#f"),
            "/posts/1/edit?x=1"
        );
        assert_eq!(back("https://APP.test"), "/");
        assert_eq!(back("/register"), "/register");
        assert_eq!(back("https://evil.test/x"), "/");
        assert_eq!(back("//evil.test/x"), "/");
        // A same-site referer whose path is protocol-relative would redirect off-site.
        assert_eq!(back("https://app.test//evil.example/x"), "/");
        assert_eq!(back("https://app.test/\\evil.example/x"), "/");
        assert_eq!(back("https://app.test?//evil.example"), "/?//evil.example");
        assert_eq!(back("javascript:alert(1)"), "/");
        assert_eq!(back(""), "/");
        // Browsers drop tabs from a Location, so `/<TAB>/evil.example` would become
        // `//evil.example`; a backslash anywhere counts as a slash.
        assert_eq!(back("/\t/evil.example/x"), "/");
        assert_eq!(back("https://app.test/\t/evil.example/x"), "/");
        assert_eq!(back("/\\evil.example"), "/");
        assert_eq!(back("/a\\b"), "/");
    }

    #[test]
    fn local_paths_never_leave_the_site() {
        for ok in [
            "/",
            "/dashboard",
            "/posts/1?page=2&q=a%20b",
            "/?//x",
            "/a/b/../c",
        ] {
            assert!(is_local_path(ok), "{ok}");
        }
        for bad in [
            "",
            "dashboard",
            "//evil.example",
            "///evil.example",
            "/\\evil.example",
            "\\\\evil.example",
            "/\t/evil.example",
            "/\n/evil.example",
            "/\r\n/evil.example",
            "/ /evil.example",
            "/x\u{0}y",
            "/x\u{a0}y",
            "/\u{FF0F}evil.example",
            "/\u{2215}evil.example",
            "/caf\u{e9}",
            "https://evil.example/",
            "http:/evil.example",
            "javascript:alert(1)",
            "data:text/html,x",
            " /dashboard",
        ] {
            assert!(!is_local_path(bad), "{bad:?}");
        }
    }
}
