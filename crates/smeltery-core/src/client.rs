//! Who sent a request: the client address, scheme and host, with `X-Forwarded-*` headers
//! honoured only from trusted proxies (`TRUSTED_PROXIES`).

use std::net::{IpAddr, SocketAddr};

use axum::extract::ConnectInfo;
use http::{HeaderMap, Uri, header};

/// The proxies whose `X-Forwarded-For`, `X-Forwarded-Proto` and `X-Forwarded-Host` headers
/// the app believes (`TRUSTED_PROXIES`).
///
/// The setting is a comma-separated list of IP addresses and CIDR ranges
/// (`127.0.0.1,10.0.0.0/8,::1`), or `*`. Empty (the default) trusts nobody: the client is
/// always the TCP peer and the forwarded headers are ignored. A range of every address
/// (`0.0.0.0/0`, `::/0`) is refused: use `*`.
///
/// `*` trusts the direct TCP peer, whatever its address, but not the addresses listed inside
/// `X-Forwarded-For`, so the client is the last address the peer appended. Use it only
/// when nothing but the proxy can reach the app (it listens on `127.0.0.1`, a private
/// network or a firewalled port): a client that reaches the app directly can then send any
/// `X-Forwarded-*` value.
///
/// ```
/// use std::net::IpAddr;
/// use smeltery_core::http::TrustedProxies;
///
/// let trusted = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8").unwrap();
/// assert!(trusted.trusts("10.1.2.3".parse::<IpAddr>().unwrap()));
/// assert!(!trusted.trusts("192.0.2.1".parse::<IpAddr>().unwrap()));
/// assert!(TrustedProxies::parse("10.0.0.0/33").is_err());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedProxies {
    peer: bool,
    nets: Vec<Net>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl Net {
    fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

impl TrustedProxies {
    /// Trust nobody (the default).
    pub fn none() -> Self {
        Self::default()
    }

    /// Parse a `TRUSTED_PROXIES` value: comma-separated IP addresses and CIDR ranges, or `*`
    /// (see the type docs). Blank entries are skipped.
    ///
    /// # Errors
    /// An entry is neither `*`, an IP address nor a CIDR range, or is a range of every address
    /// (`0.0.0.0/0`, `::/0`); the message names it.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut out = Self::default();
        for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            if entry == "*" {
                out.peer = true;
                continue;
            }
            let invalid =
                || format!("TRUSTED_PROXIES: `{entry}` is not an IP address or CIDR range");
            let (addr, prefix) = match entry.split_once('/') {
                Some((addr, prefix)) => (addr, Some(prefix)),
                None => (entry, None),
            };
            let written = addr.parse::<IpAddr>().map_err(|_| invalid())?;
            let addr = canonical(written);
            // An IPv4-mapped range (`::ffff:10.0.0.0/104`) is matched as its IPv4 range.
            let mapped = written.is_ipv6() && addr.is_ipv4();
            let max: u8 = if addr.is_ipv4() { 32 } else { 128 };
            let prefix = match prefix {
                Some(p) => {
                    if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(invalid());
                    }
                    let p = p.parse::<u8>().map_err(|_| invalid())?;
                    let p = if mapped {
                        p.checked_sub(96).ok_or_else(invalid)?
                    } else {
                        p
                    };
                    if p > max {
                        return Err(invalid());
                    }
                    // A range of every address would make every hop a proxy, so the client could
                    // name its own address in `X-Forwarded-For` (W1-02); `*` is the way to trust
                    // whatever peer connects.
                    if p == 0 {
                        return Err(format!(
                            "TRUSTED_PROXIES: `{entry}` trusts every address, so any client could choose its                              own IP; list the proxies' addresses, or use `*` to trust the connecting peer                              (only when nothing but the proxy can reach the app)"
                        ));
                    }
                    p
                }
                None => max,
            };
            out.nets.push(Net { addr, prefix });
        }
        Ok(out)
    }

    /// Whether `ip` is a listed proxy (an IPv4-mapped IPv6 address counts as its IPv4 address).
    /// `*` is about the direct peer only and is not consulted here.
    pub fn trusts(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.nets.iter().any(|n| n.contains(ip))
    }

    /// Whether no proxy is trusted.
    pub fn is_empty(&self) -> bool {
        !self.peer && self.nets.is_empty()
    }

    pub(crate) fn trusts_peer(&self, ip: IpAddr) -> bool {
        self.peer || self.trusts(ip)
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

/// The client of a request, after [`TrustedProxies`]: its IP address, whether it used HTTPS
/// and the host it asked for. Take it as a handler argument (`client: ClientInfo`), or read it
/// from the request extensions in middleware.
///
/// When the TCP peer is a trusted proxy:
/// - the IP is the rightmost address in `X-Forwarded-For` that is not itself a trusted proxy
///   (the rightmost address when all of them are trusted; `None` when the walk reaches a hop
///   that is not an IP address; the peer's own address when the header is missing);
/// - the scheme is the first value of `X-Forwarded-Proto` (`http` or `https`);
/// - the host is the first value of `X-Forwarded-Host`.
///
/// Otherwise the IP is the TCP peer's, the scheme is `http` (the server speaks plain HTTP)
/// and the host is the `Host` header; forwarded headers are ignored. The login throttle,
/// [`Auth::ip`](crate::auth::Auth::ip) and the request log use this IP.
///
/// ```
/// use smeltery_core::http::ClientInfo;
///
/// async fn whoami(client: ClientInfo) -> String {
///     client.ip().map_or_else(|| "unknown".to_owned(), |ip| ip.to_string())
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    peer: Option<SocketAddr>,
    ip: Option<IpAddr>,
    secure: bool,
    host: Option<String>,
    proxied: bool,
}

impl ClientInfo {
    /// Work out the client from the TCP peer and the request headers.
    ///
    /// ```
    /// use smeltery_core::http::{ClientInfo, HeaderMap, HeaderValue, TrustedProxies, Uri};
    ///
    /// let mut headers = HeaderMap::new();
    /// headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.7"));
    /// let peer = "127.0.0.1:50000".parse().ok();
    /// let uri = Uri::from_static("/");
    ///
    /// let direct = ClientInfo::resolve(&TrustedProxies::none(), peer, &headers, &uri);
    /// assert_eq!(direct.ip().unwrap().to_string(), "127.0.0.1");
    ///
    /// let trusted = TrustedProxies::parse("127.0.0.1").unwrap();
    /// let proxied = ClientInfo::resolve(&trusted, peer, &headers, &uri);
    /// assert_eq!(proxied.ip().unwrap().to_string(), "203.0.113.7");
    /// ```
    pub fn resolve(
        trusted: &TrustedProxies,
        peer: Option<SocketAddr>,
        headers: &HeaderMap,
        uri: &Uri,
    ) -> Self {
        let peer_ip = peer.map(|p| canonical(p.ip()));
        let direct_host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .or_else(|| uri.authority().map(|a| a.as_str().to_owned()))
            .filter(|h| valid_host(h));
        let direct_secure = uri.scheme_str() == Some("https");
        let proxied = peer_ip.is_some_and(|ip| trusted.trusts_peer(ip));
        if !proxied {
            return Self {
                peer,
                ip: peer_ip,
                secure: direct_secure,
                host: direct_host,
                proxied: false,
            };
        }
        let ip = match forwarded_ip(trusted, headers) {
            Hops::None => peer_ip,
            Hops::Client(ip) => Some(ip),
            Hops::Unreadable => None,
        };
        let secure = match first_value(headers, "x-forwarded-proto")
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            Some("https") => true,
            Some("http") => false,
            _ => direct_secure,
        };
        let host = first_value(headers, "x-forwarded-host")
            .filter(|h| valid_host(h))
            .map(str::to_owned)
            .or(direct_host);
        Self {
            peer,
            ip,
            secure,
            host,
            proxied: true,
        }
    }

    /// The client's IP address; `None` when the server passed no connection info (e.g. a
    /// [`TestApp`](crate::testing::TestApp) request without
    /// [`from_addr`](crate::testing::TestApp::from_addr)).
    pub fn ip(&self) -> Option<IpAddr> {
        self.ip
    }

    /// The TCP peer: the client itself, or the proxy in front of the app.
    pub fn peer(&self) -> Option<SocketAddr> {
        self.peer
    }

    /// Whether the client used HTTPS (from a trusted proxy's `X-Forwarded-Proto`).
    pub fn is_secure(&self) -> bool {
        self.secure
    }

    /// `https` or `http`.
    pub fn scheme(&self) -> &'static str {
        if self.secure { "https" } else { "http" }
    }

    /// The host the client asked for (a trusted proxy's `X-Forwarded-Host`, else `Host`); only
    /// host characters (letters, digits, `.`, `-`, `:`, `[`, `]`) are accepted.
    ///
    /// It is as trustworthy as the proxy's configuration: the proxy must set (overwrite)
    /// `X-Forwarded-Host` and `X-Forwarded-Proto` itself (nginx:
    /// `proxy_set_header X-Forwarded-Host $host;` and `proxy_set_header X-Forwarded-Proto
    /// $scheme;`; Caddy sets both), otherwise the client's own values pass through. Smeltery
    /// builds its own absolute URLs (password reset links, mail) from `APP_URL`, never from this.
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// Whether the request came through a trusted proxy (so the forwarded headers were used).
    pub fn is_proxied(&self) -> bool {
        self.proxied
    }

    /// The client of a request from its extensions: the value the HTTP stack stored, else the
    /// TCP peer without proxy trust (a router built outside the app's HTTP stack).
    pub fn from_parts(parts: &http::request::Parts) -> Self {
        stored_or_peer(&parts.extensions, &parts.headers, &parts.uri)
    }
}

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientInfo {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self::from_parts(parts))
    }
}

/// What `X-Forwarded-For` says about the client.
enum Hops {
    /// No address at all: the trusted peer made the request itself (e.g. a health probe).
    None,
    /// The client's address.
    Client(IpAddr),
    /// The walk reached an unreadable hop: the client is unknown.
    Unreadable,
}

/// Walk `X-Forwarded-For` from the right, skipping trusted proxies; the first address that is
/// not one is the client. When every hop is trusted, the rightmost one is (the address the
/// nearest proxy saw), never one further left, which the client may have written. Each hop is split
/// and decoded on its own (as bytes), so a byte the client wrote cannot hide the hops the
/// proxies appended. An unreadable hop reached by the walk stops it: the client is unknown,
/// never a trusted proxy's address.
fn forwarded_ip(trusted: &TrustedProxies, headers: &HeaderMap) -> Hops {
    let mut hops: Vec<&[u8]> = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        hops.extend(
            value
                .as_bytes()
                .split(|b| *b == b',')
                .map(<[u8]>::trim_ascii)
                .filter(|h| !h.is_empty()),
        );
    }
    let mut nearest = None;
    for hop in hops.iter().rev() {
        let Some(ip) = std::str::from_utf8(hop).ok().and_then(parse_hop) else {
            return Hops::Unreadable;
        };
        if !trusted.trusts(ip) {
            return Hops::Client(ip);
        }
        nearest.get_or_insert(ip);
    }
    nearest.map_or(Hops::None, Hops::Client)
}

/// `1.2.3.4`, `1.2.3.4:5678`, `::1` or `[::1]:5678`.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    if let Ok(ip) = hop.parse::<IpAddr>() {
        return Some(canonical(ip));
    }
    hop.parse::<SocketAddr>().ok().map(|s| canonical(s.ip()))
}

fn first_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn valid_host(host: &str) -> bool {
    host.len() <= 255
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

/// The [`ClientInfo`] the stack stored on `req`, else the TCP peer without proxy trust.
pub(crate) fn of_request(req: &axum::extract::Request) -> ClientInfo {
    stored_or_peer(req.extensions(), req.headers(), req.uri())
}

fn stored_or_peer(extensions: &http::Extensions, headers: &HeaderMap, uri: &Uri) -> ClientInfo {
    if let Some(found) = extensions.get::<ClientInfo>() {
        return found.clone();
    }
    let peer = extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
    ClientInfo::resolve(&TrustedProxies::none(), peer, headers, uri)
}

/// Stores the [`ClientInfo`] of every request in its extensions (outermost layer of the stack).
pub(crate) fn attach(
    trusted: &TrustedProxies,
    mut req: axum::extract::Request,
) -> axum::extract::Request {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let info = ClientInfo::resolve(trusted, peer, req.headers(), req.uri());
    req.extensions_mut().insert(info);
    req
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

    fn resolve(trusted: &str, peer: &str, pairs: &[(&'static str, &'static str)]) -> ClientInfo {
        ClientInfo::resolve(
            &TrustedProxies::parse(trusted).unwrap(),
            Some(peer.parse().unwrap()),
            &headers(pairs),
            &Uri::from_static("/"),
        )
    }

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn parses_addresses_ranges_and_star() {
        let t = TrustedProxies::parse(" 127.0.0.1 , 10.0.0.0/8,, fd00::/8, *").unwrap();
        assert!(t.peer);
        assert_eq!(t.nets.len(), 3);
        assert!(t.trusts("10.255.0.1".parse().unwrap()));
        assert!(t.trusts("fd12::1".parse().unwrap()));
        assert!(t.trusts("::ffff:127.0.0.1".parse().unwrap()), "IPv4-mapped");
        assert!(!t.trusts("11.0.0.1".parse().unwrap()));
        assert!(TrustedProxies::parse("").unwrap().is_empty());
        // A range of every address is refused (W1-02): `*` is the way to trust any peer.
        for every in [
            "0.0.0.0/0",
            "::/0",
            "::ffff:0.0.0.0/96",
            "10.0.0.1, 0.0.0.0/0",
        ] {
            let err = TrustedProxies::parse(every).unwrap_err();
            assert!(err.contains("`*`"), "{every}: {err}");
        }
        assert!(TrustedProxies::parse("0.0.0.0/1").is_ok());
        for bad in [
            "localhost",
            "10.0.0.0/33",
            "::/129",
            "1.2.3",
            "10.0.0.0/x",
            "10.0.0.0/+8",
            "10.0.0.0/",
            "::ffff:10.0.0.0/95",
        ] {
            let err = TrustedProxies::parse(bad).unwrap_err();
            assert!(err.contains(bad), "{err}");
        }
        // An IPv4-mapped range is its IPv4 range.
        let mapped = TrustedProxies::parse("::ffff:10.0.0.0/104").unwrap();
        assert!(mapped.trusts("10.9.9.9".parse().unwrap()));
        assert!(!mapped.trusts("11.0.0.1".parse().unwrap()));
    }

    #[test]
    fn bytes_that_are_not_utf8_stay_in_their_own_hop() {
        let mut h = HeaderMap::new();
        h.append(
            "x-forwarded-for",
            HeaderValue::from_bytes(b"\xff, 203.0.113.9").unwrap(),
        );
        let trusted = TrustedProxies::parse("127.0.0.1").unwrap();
        let peer = Some("127.0.0.1:1".parse().unwrap());
        let uri = Uri::from_static("/");
        assert_eq!(
            ClientInfo::resolve(&trusted, peer, &h, &uri).ip(),
            ip("203.0.113.9")
        );
        let mut h = HeaderMap::new();
        h.append("x-forwarded-for", HeaderValue::from_bytes(b"\xfe").unwrap());
        h.append("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        assert_eq!(
            ClientInfo::resolve(&trusted, peer, &h, &uri).ip(),
            ip("203.0.113.9")
        );
        let mut h = HeaderMap::new();
        h.append("x-forwarded-for", HeaderValue::from_bytes(b"\xfe").unwrap());
        assert_eq!(ClientInfo::resolve(&trusted, peer, &h, &uri).ip(), None);
    }

    #[test]
    fn a_direct_host_header_is_checked_too() {
        let c = resolve("", "198.51.100.4:4000", &[("host", "a b")]);
        assert_eq!(c.host(), None);
    }

    #[test]
    fn untrusted_peers_cannot_spoof_anything() {
        let c = resolve(
            "",
            "198.51.100.4:4000",
            &[
                ("x-forwarded-for", "203.0.113.9"),
                ("x-forwarded-proto", "https"),
                ("x-forwarded-host", "evil.test"),
                ("host", "app.test"),
            ],
        );
        assert_eq!(c.ip(), ip("198.51.100.4"));
        assert!(!c.is_secure() && !c.is_proxied());
        assert_eq!(c.host(), Some("app.test"));
        // A trusted list that does not include this peer changes nothing either.
        let c = resolve(
            "127.0.0.1",
            "198.51.100.4:4000",
            &[("x-forwarded-for", "203.0.113.9")],
        );
        assert_eq!(c.ip(), ip("198.51.100.4"));
    }

    #[test]
    fn a_trusted_proxy_gives_the_rightmost_untrusted_hop() {
        let pairs = [
            ("x-forwarded-for", "1.1.1.1, 203.0.113.9"),
            ("x-forwarded-for", "10.0.0.2"),
            ("x-forwarded-proto", "https, http"),
            ("x-forwarded-host", "example.com"),
            ("host", "127.0.0.1:8000"),
        ];
        let c = resolve("127.0.0.1,10.0.0.0/8", "127.0.0.1:5000", &pairs);
        // 10.0.0.2 is a trusted hop; 203.0.113.9 is the first one that is not. The spoofable
        // leftmost entry (1.1.1.1) is never used.
        assert_eq!(c.ip(), ip("203.0.113.9"));
        assert!(c.is_secure() && c.is_proxied());
        assert_eq!(c.scheme(), "https");
        assert_eq!(c.host(), Some("example.com"));
        assert_eq!(c.peer().unwrap().to_string(), "127.0.0.1:5000");
    }

    #[test]
    fn star_trusts_only_the_direct_peer() {
        let c = resolve(
            "*",
            "192.0.2.50:1",
            &[("x-forwarded-for", "1.1.1.1, 10.0.0.2")],
        );
        assert_eq!(
            c.ip(),
            ip("10.0.0.2"),
            "the hop the peer appended, nothing left of it"
        );
    }

    #[test]
    fn odd_forwarded_values_fall_back_safely() {
        // Every hop trusted: the rightmost (what the nearest proxy saw), never the leftmost,
        // which a client inside the trusted range may have written itself (W1-02).
        let c = resolve(
            "10.0.0.0/8",
            "10.0.0.1:1",
            &[("x-forwarded-for", "10.0.0.5, 10.0.0.6")],
        );
        assert_eq!(c.ip(), ip("10.0.0.6"));
        let c = resolve(
            "10.0.0.0/8",
            "10.0.0.1:1",
            &[
                ("x-forwarded-for", "10.9.9.9"),
                ("x-forwarded-for", "10.0.0.3"),
            ],
        );
        assert_eq!(c.ip(), ip("10.0.0.3"));
        // An unreadable hop stops the walk: the client is unknown, never the proxy itself.
        let c = resolve(
            "10.0.0.1",
            "10.0.0.1:1",
            &[("x-forwarded-for", "1.1.1.1, nonsense")],
        );
        assert_eq!(c.ip(), None);
        assert!(c.is_proxied());
        let c = resolve(
            "10.0.0.0/8",
            "10.0.0.1:1",
            &[("x-forwarded-for", "junk, 10.0.0.7")],
        );
        assert_eq!(c.ip(), None, "left of trusted hops: unknown, not 10.0.0.7");
        // Ports and brackets are accepted.
        let c = resolve(
            "::1",
            "[::1]:1",
            &[("x-forwarded-for", "[2001:db8::7]:443, 198.51.100.1:80")],
        );
        assert_eq!(c.ip(), ip("198.51.100.1"));
        // No header: the peer. Unknown proto and a bad host are ignored.
        let c = resolve(
            "127.0.0.1",
            "127.0.0.1:1",
            &[
                ("x-forwarded-proto", "gopher"),
                ("x-forwarded-host", "a b"),
                ("host", "h.test"),
            ],
        );
        assert_eq!(c.ip(), ip("127.0.0.1"));
        assert!(!c.is_secure());
        assert_eq!(c.host(), Some("h.test"));
    }

    #[test]
    fn without_connection_info_the_ip_is_unknown() {
        let c = ClientInfo::resolve(
            &TrustedProxies::parse("*").unwrap(),
            None,
            &headers(&[("x-forwarded-for", "203.0.113.9")]),
            &Uri::from_static("/"),
        );
        assert_eq!(c.ip(), None);
        assert!(!c.is_proxied());
    }
}
