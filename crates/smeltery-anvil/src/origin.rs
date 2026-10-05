//! The `Origin` policy of the socket endpoint (D-412).
//!
//! Browsers always send `Origin` on a WebSocket handshake, so an origin decides which pages may open sockets. A
//! request without one is a non-browser client (a native SDK, a backend) and is accepted: the socket carries no
//! identity, and private channels need a signature from the auth endpoint. The origins of `CORS_ALLOWED_ORIGINS` (the
//! app's hybrid clients, D-417) are allowed too; `null` (a sandboxed frame, a page opened from a file, some hybrid
//! shells) only when one of the two lists names it.

use smeltery_core::config::loopback_host;
use smeltery_core::cors::{normalize_origin, origin_of_url, origin_parts};
use smeltery_core::{Error, Result};

/// An origin as compared: core's normal form (`smeltery_core::cors::normalize_origin`, the framework's one origin
/// parser): lowercase scheme and host, the port unless it is the scheme's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Origin(String);

impl Origin {
    /// Parse `scheme://host[:port]` (no path, query, fragment, user info or `*`). `None` for anything else, `null`
    /// included.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        normalize_origin(text).filter(|o| o != "null").map(Self)
    }

    /// The origin of a URL: its scheme and authority (user info dropped), ignoring path, query and fragment.
    pub(crate) fn of_url(url: &str) -> Option<Self> {
        origin_of_url(url).map(Self)
    }

    fn loopback_alias_of(&self, other: &Self) -> bool {
        match (origin_parts(&self.0), origin_parts(&other.0)) {
            (Some((scheme, host, port)), Some((other_scheme, other_host, other_port))) => {
                scheme == other_scheme
                    && port == other_port
                    && loopback_host(host)
                    && loopback_host(other_host)
            }
            _ => false,
        }
    }
}

/// Which origins may open sockets.
#[derive(Debug, Clone)]
pub(crate) struct Policy {
    /// `APP_URL`'s origin and `ANVIL_ALLOWED_ORIGINS`.
    allowed: Vec<Origin>,
    /// `*` was listed.
    any: bool,
    /// `null` was listed.
    null: bool,
    /// Local development: loopback aliases of an allowed origin (`localhost` for `127.0.0.1`) count too.
    local: bool,
}

/// What the policy says about a handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// No `Origin` header: a non-browser client.
    Absent,
    /// A listed origin.
    Allowed,
    /// Not listed, `null` or unreadable.
    Refused,
}

impl Policy {
    /// The policy for `APP_URL`, `ANVIL_ALLOWED_ORIGINS` (`listed`) and `CORS_ALLOWED_ORIGINS` (`cors`, checked by
    /// core: exact origins or `null`).
    ///
    /// # Errors
    /// `APP_URL` has no origin (`scheme://host[:port]`), or an entry of `ANVIL_ALLOWED_ORIGINS` is not an origin,
    /// `null` or `*`.
    pub(crate) fn new(app_url: &str, listed: &[String], cors: &str, local: bool) -> Result<Self> {
        let app = Origin::of_url(app_url).ok_or_else(|| {
                 Error::internal(
                 "APP_URL is not a URL with a scheme and a host (https://app.example.com): Anvil compares browsers' \
                 Origin with it",
                 )
                 })?;
        let mut allowed = vec![app];
        let mut any = false;
        let mut null = false;
        for entry in listed {
            if entry == "*" {
                any = true;
                continue;
            }
            if entry == "null" {
                null = true;
                continue;
            }
            let origin = Origin::parse(entry).ok_or_else(|| {
                Error::internal(format!(
                    "ANVIL_ALLOWED_ORIGINS: `{entry}` is not an origin (scheme://host[:port], or *)"
                ))
            })?;
            allowed.push(origin);
        }
        for entry in cors.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            if entry == "null" {
                null = true;
            } else if let Some(origin) = Origin::parse(entry) {
                allowed.push(origin);
            }
        }
        Ok(Self {
            allowed,
            any,
            null,
            local,
        })
    }

    /// Whether `*` is listed.
    pub(crate) fn allows_any(&self) -> bool {
        self.any
    }

    /// The verdict for a handshake's `Origin` header values (none, one, or several).
    pub(crate) fn check<'a>(&self, mut values: impl Iterator<Item = &'a [u8]>) -> Verdict {
        let Some(first) = values.next() else {
            return Verdict::Absent;
        };
        // Two `Origin` headers: not a browser's handshake, and not one we can judge.
        if values.next().is_some() {
            return Verdict::Refused;
        }
        if first == b"null" {
            return if self.null {
                Verdict::Allowed
            } else {
                Verdict::Refused
            };
        }
        let Some(origin) = std::str::from_utf8(first).ok().and_then(Origin::parse) else {
            return Verdict::Refused;
        };
        if self.any
            || self.allowed.iter().any(|allowed| {
                *allowed == origin || (self.local && allowed.loopback_alias_of(&origin))
            })
        {
            Verdict::Allowed
        } else {
            Verdict::Refused
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(listed: &[&str], local: bool) -> Policy {
        let listed: Vec<String> = listed.iter().map(|s| (*s).to_owned()).collect();
        Policy::new("https://app.example.com", &listed, "", local).unwrap()
    }

    fn verdict(policy: &Policy, origin: Option<&str>) -> Verdict {
        policy.check(origin.map(str::as_bytes).into_iter())
    }

    #[test]
    fn origins_are_compared_by_scheme_host_and_port() {
        let p = policy(
            &["capacitor://localhost", "http://admin.example.com:8080"],
            false,
        );
        assert_eq!(verdict(&p, None), Verdict::Absent);
        assert_eq!(
            verdict(&p, Some("https://app.example.com")),
            Verdict::Allowed
        );
        assert_eq!(
            verdict(&p, Some("HTTPS://App.Example.com:443")),
            Verdict::Allowed
        );
        assert_eq!(
            verdict(&p, Some("http://app.example.com")),
            Verdict::Refused
        );
        assert_eq!(
            verdict(&p, Some("https://app.example.com:8443")),
            Verdict::Refused
        );
        assert_eq!(
            verdict(&p, Some("https://evil.example.com")),
            Verdict::Refused
        );
        assert_eq!(
            verdict(&p, Some("https://app.example.com.evil.net")),
            Verdict::Refused
        );
        assert_eq!(verdict(&p, Some("capacitor://localhost")), Verdict::Allowed);
        assert_eq!(
            verdict(&p, Some("http://admin.example.com:8080")),
            Verdict::Allowed
        );
        assert_eq!(verdict(&p, Some("null")), Verdict::Refused);
        assert_eq!(
            verdict(&p, Some("https://user@app.example.com")),
            Verdict::Refused
        );
        assert_eq!(verdict(&p, Some("")), Verdict::Refused);
    }

    #[test]
    fn hybrid_origins_come_from_cors_and_null_only_when_listed() {
        let p = Policy::new(
            "https://app.example.com",
            &[],
            "capacitor://localhost, tauri://localhost",
            false,
        )
        .unwrap();
        assert_eq!(verdict(&p, Some("capacitor://localhost")), Verdict::Allowed);
        assert_eq!(verdict(&p, Some("tauri://localhost")), Verdict::Allowed);
        assert_eq!(verdict(&p, Some("app://bundle")), Verdict::Refused);
        assert_eq!(verdict(&p, Some("null")), Verdict::Refused);
        let in_cors = Policy::new("https://app.example.com", &[], "null", false).unwrap();
        assert_eq!(verdict(&in_cors, Some("null")), Verdict::Allowed);
        let in_anvil = policy(&["null"], false);
        assert_eq!(verdict(&in_anvil, Some("null")), Verdict::Allowed);
        assert_eq!(
            verdict(&in_anvil, Some("https://evil.example")),
            Verdict::Refused
        );
    }

    #[test]
    fn two_origin_headers_are_refused() {
        let p = policy(&[], false);
        let values = [
            b"https://app.example.com".as_slice(),
            b"https://app.example.com".as_slice(),
        ];
        assert_eq!(p.check(values.into_iter()), Verdict::Refused);
    }

    #[test]
    fn a_star_allows_any_origin_but_null_and_garbage() {
        let p = policy(&["*"], false);
        assert!(p.allows_any());
        assert_eq!(
            verdict(&p, Some("https://anything.example")),
            Verdict::Allowed
        );
        assert_eq!(verdict(&p, Some("null")), Verdict::Refused);
    }

    #[test]
    fn local_development_accepts_loopback_aliases() {
        let local = Policy::new("http://localhost:8000", &[], "", true).unwrap();
        assert_eq!(
            verdict(&local, Some("http://127.0.0.1:8000")),
            Verdict::Allowed
        );
        assert_eq!(verdict(&local, Some("http://[::1]:8000")), Verdict::Allowed);
        assert_eq!(
            verdict(&local, Some("http://127.0.0.1:9000")),
            Verdict::Refused
        );
        let production = Policy::new("http://localhost:8000", &[], "", false).unwrap();
        assert_eq!(
            verdict(&production, Some("http://127.0.0.1:8000")),
            Verdict::Refused
        );
    }

    #[test]
    fn app_url_gives_its_origin_whatever_its_path() {
        let p = Policy::new(
            "https://user:pw@app.example.com:8443/sub/app?x=1#f",
            &[],
            "",
            false,
        )
        .unwrap();
        assert_eq!(
            verdict(&p, Some("https://app.example.com:8443")),
            Verdict::Allowed
        );
        let p = Policy::new("https://app.example.com/app", &[], "", false).unwrap();
        assert_eq!(
            verdict(&p, Some("https://app.example.com")),
            Verdict::Allowed
        );
        assert!(
            Policy::new("not a url", &[], "", false)
                .unwrap_err()
                .to_string()
                .contains("APP_URL")
        );
    }

    #[test]
    fn bad_entries_fail() {
        assert!(Policy::new("https://a.example", &["not an origin".into()], "", false).is_err());
        assert!(
            Policy::new(
                "https://a.example",
                &["https://a.example/path".into()],
                "",
                false
            )
            .is_err()
        );
    }
}
