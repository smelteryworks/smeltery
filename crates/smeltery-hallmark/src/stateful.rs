//! The first-party rule of the same-origin SPA mode (D-463): which API requests the session cookie may
//! authenticate. Core runs its web stack (session, password binding, CSRF) around a request the guard calls
//! first-party ([`Guard::first_party`](smeltery_core::auth::Guard::first_party)).

use http::{HeaderMap, header};
use smeltery_core::cors::normalize_origin;
use smeltery_core::{Error, Result};

/// An origin as compared: core's normal form (`smeltery_core::cors::normalize_origin`, the framework's one origin
/// parser), limited to `http` and `https`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Origin(String);

impl Origin {
    /// `scheme://host[:port]` with an optional trailing `/`; `None` for anything else (`null`, `*`, a path, user info,
    /// a scheme other than `http` / `https`).
    pub(crate) fn parse(text: &str) -> Option<Self> {
        normalize_origin(text)
            .filter(|o| o.starts_with("http://") || o.starts_with("https://"))
            .map(Self)
    }

    /// The origin of a URL (`APP_URL`, a `Referer`): its scheme, host and port, whatever its path (a URL with user
    /// info has none).
    pub(crate) fn of_url(url: &str) -> Option<Self> {
        let (scheme, rest) = url.trim().split_once("://")?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = rest.get(..end)?;
        Self::parse(&format!("{scheme}://{authority}"))
    }
}

/// The first-party origins: `APP_URL`'s and the `HALLMARK_STATEFUL` entries.
#[derive(Debug, Clone)]
pub(crate) struct FirstParty {
    /// `APP_URL`'s origin.
    app: Option<Origin>,
    /// The `HALLMARK_STATEFUL` origins.
    listed: Vec<Origin>,
}

impl FirstParty {
    /// `APP_URL` and `HALLMARK_STATEFUL` (comma-separated `scheme://host[:port]`).
    ///
    /// # Errors
    /// An entry that is not an origin (`*`, `null`, a path, another scheme): the app stops at boot.
    pub(crate) fn new(app_url: &str, stateful: &str) -> Result<Self> {
        let mut listed = Vec::new();
        for entry in stateful.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let origin = Origin::parse(entry).ok_or_else(|| {
                Error::internal(format!(
                    "HALLMARK_STATEFUL: `{entry}` is not an origin; write scheme://host[:port], e.g. \
                     https://app.example.com (`*` and `null` are refused)"
                ))
            })?;
            listed.push(origin);
        }
        Ok(Self {
            app: Origin::of_url(app_url),
            listed,
        })
    }

    /// Whether a request with these headers is first-party:
    /// - `Sec-Fetch-Site: same-origin` (one header);
    /// - `Sec-Fetch-Site: same-site` or `cross-site`: only when the request's one `Origin` is a listed
    ///   (`HALLMARK_STATEFUL`) origin; `APP_URL`'s own origin needs `same-origin` (a browser on it sends that);
    /// - `none`, another value or two headers: never;
    /// - without `Sec-Fetch-Site` (older browsers): one `Origin`, else the `Referer`'s origin, equal to `APP_URL`'s or
    ///   a listed one.
    ///
    /// A request with none of these headers (an app or server calling with a token) is never first-party.
    pub(crate) fn allows(&self, headers: &HeaderMap) -> bool {
        let mut fetch_site = headers.get_all("sec-fetch-site").iter();
        if let Some(site) = fetch_site.next() {
            if fetch_site.next().is_some() {
                return false;
            }
            let site = site.as_bytes();
            if site.eq_ignore_ascii_case(b"same-origin") {
                return true;
            }
            if site.eq_ignore_ascii_case(b"same-site") || site.eq_ignore_ascii_case(b"cross-site") {
                return single_origin(headers).is_some_and(|o| self.listed.contains(&o));
            }
            return false;
        }
        let origin = match headers.get_all(header::ORIGIN).iter().count() {
            0 => headers
                .get(header::REFERER)
                .and_then(|r| r.to_str().ok())
                .and_then(Origin::of_url),
            1 => single_origin(headers),
            _ => return false,
        };
        origin.is_some_and(|o| self.app.as_ref() == Some(&o) || self.listed.contains(&o))
    }
}

/// The request's one `Origin`, parsed (`None` for none, two, `null` or garbage).
fn single_origin(headers: &HeaderMap) -> Option<Origin> {
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let one = origins.next()?;
    if origins.next().is_some() {
        return None;
    }
    one.to_str().ok().and_then(Origin::parse)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.append(*k, HeaderValue::from_str(v).unwrap());
        }
        map
    }

    fn rule() -> FirstParty {
        FirstParty::new(
            "https://example.com/app",
            "http://localhost:5173, https://admin.example.com",
        )
        .unwrap()
    }

    #[test]
    fn the_first_party_rule() {
        let r = rule();
        let yes: &[&[(&str, &str)]] = &[
            &[("sec-fetch-site", "same-origin")],
            &[
                ("sec-fetch-site", "Same-Origin"),
                ("origin", "https://evil.example"),
            ],
            &[("origin", "https://example.com")],
            &[("origin", "https://EXAMPLE.com:443")],
            &[("origin", "http://localhost:5173")],
            &[("referer", "https://admin.example.com/page?x=1")],
            &[
                ("sec-fetch-site", "same-site"),
                ("origin", "https://admin.example.com"),
            ],
            &[
                ("sec-fetch-site", "cross-site"),
                ("origin", "http://localhost:5173"),
            ],
        ];
        for h in yes {
            assert!(r.allows(&headers(h)), "{h:?}");
        }
        let no: &[&[(&str, &str)]] = &[
            &[],
            // A sibling subdomain is same-site, never first-party.
            &[
                ("sec-fetch-site", "same-site"),
                ("origin", "https://example.com"),
            ],
            &[("sec-fetch-site", "cross-site")],
            &[("sec-fetch-site", "none")],
            &[
                ("sec-fetch-site", "same-origin"),
                ("sec-fetch-site", "same-origin"),
            ],
            &[("origin", "https://api.example.com")],
            &[("origin", "http://example.com")],
            &[("origin", "https://example.com:8443")],
            &[("origin", "null")],
            &[
                ("origin", "https://example.com"),
                ("origin", "https://example.com"),
            ],
            &[("referer", "https://example.com.evil.test/")],
            &[
                ("origin", "https://evil.test"),
                ("referer", "https://example.com/"),
            ],
        ];
        for h in no {
            assert!(!r.allows(&headers(h)), "{h:?}");
        }
    }

    #[test]
    fn bad_stateful_entries_fail() {
        for bad in [
            "*",
            "null",
            "https://example.com/path",
            "ftp://x",
            "example.com",
            "https://u@x",
        ] {
            assert!(
                FirstParty::new("https://example.com", bad).is_err(),
                "{bad}"
            );
        }
        assert!(FirstParty::new("https://example.com", " ").is_ok());
        assert_eq!(
            Origin::parse("https://[::1]:8000"),
            Origin::of_url("https://[::1]:8000/x")
        );
    }
}
