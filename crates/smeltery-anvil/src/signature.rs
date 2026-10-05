//! Channel signatures (D-400, ANVIL.md §10 A8).
//!
//! A private subscription carries the `auth` string the auth endpoint gave out:
//! `<app key>:<grant>:<hex>`, where the grant names who was authorized and until when
//! (`<user id or g>.<credential key with `~` for `:`, or ->.<issued>.<expiry>`, Unix seconds) and `hex` is
//! HMAC-SHA256(secret, `<socket id>:<channel>:<grant>`) in lowercase hex. Pusher clients pass `auth` on unchanged
//! (pusher-js `channel.ts`, pusher-websocket-java `PrivateChannelImpl`, pusher-websocket-swift
//! `PusherConnection.handlePrivateChannelAuth`), so the grant travels with the subscription and the socket's
//! process learns who subscribed without any message between processes. A signature is bound to one socket id
//! (the server assigns ids, so a leaked `auth` is useless on another socket), one channel and one grant. A presence
//! subscription's signature also covers its `channel_data` (the member):
//! HMAC-SHA256(secret, `<socket id>:<channel>:<grant>:<channel_data>`), so a client cannot change who it appears as;
//! the channel's prefix (`private-` / `presence-`) is inside every signed message, so a signature of one kind never
//! verifies as the other.

use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// How long a grant may be used to subscribe after the auth endpoint gave it out. The client subscribes right
/// after the authorization; the processes' clocks must agree within this.
pub(crate) const GRANT_LIFETIME: Duration = Duration::from_secs(300);

/// `hex(HMAC-SHA256(secret, message))`, the Pusher signature.
pub(crate) fn sign(secret: &str, message: &str) -> String {
    hex(&mac(secret, message))
}

fn mac(secret: &str, message: &str) -> Vec<u8> {
    // HMAC accepts keys of any length.
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return Vec::new();
    };
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The credential a grant was made for: core's `Principal::key`, `<guard>:session:<binding>` or
/// `<guard>:token:<id>`, as revocation events name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Credential(String);

impl Credential {
    /// The key.
    pub(crate) fn key(&self) -> &str {
        &self.0
    }

    /// From a principal's key; `None` for any other shape. The guard name follows core's rule for guard names
    /// (lowercase ASCII letters, digits, `_` and `-`); a token id is an integer as core writes it (`5`, not `05` or
    /// `+5`).
    pub(crate) fn from_key(key: &str) -> Option<Self> {
        let mut parts = key.split(':');
        let (guard, kind, value) = (parts.next()?, parts.next()?, parts.next()?);
        let guard_ok = !guard.is_empty()
            && guard
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        let value_ok = match kind {
            "session" => {
                (1..=64).contains(&value.len()) && value.bytes().all(|c| c.is_ascii_hexdigit())
            }
            "token" => value.parse::<i64>().is_ok_and(|id| id.to_string() == value),
            _ => false,
        };
        (parts.next().is_none() && guard_ok && value_ok).then(|| Self(key.to_owned()))
    }

    /// The key inside a grant, where `:` separates the `auth` string's parts.
    fn encode(&self) -> String {
        self.0.replace(':', "~")
    }

    fn decode(text: &str) -> Option<Self> {
        Self::from_key(&text.replace('~', ":"))
    }
}

/// Who a subscription was authorized for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Grant {
    /// The signed-in user, `None` for a guest (`.guests()` patterns).
    pub(crate) user: Option<i64>,
    /// The credential it was made for, `None` when there was none (a guest without a session).
    pub(crate) credential: Option<Credential>,
    /// Unix seconds when the auth endpoint made it (a revocation after this time refuses it).
    pub(crate) issued: u64,
    /// Unix seconds after which the grant cannot subscribe.
    pub(crate) expires: u64,
}

impl Grant {
    fn encode(&self) -> String {
        let credential = self
            .credential
            .as_ref()
            .map_or_else(|| "-".to_owned(), Credential::encode);
        format!(
            "{}.{credential}.{}.{}",
            self.user
                .map_or_else(|| "g".to_owned(), |id| id.to_string()),
            self.issued,
            self.expires
        )
    }

    fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let (user, credential, issued, expires) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        let user = match user {
            "g" => None,
            id => Some(id.parse::<i64>().ok()?),
        };
        let credential = if credential == "-" {
            None
        } else {
            Some(Credential::decode(credential)?)
        };
        // A signed-in user's grant always names the credential (so a revocation can end it).
        if user.is_some() && credential.is_none() {
            return None;
        }
        Some(Self {
            user,
            credential,
            issued: issued.parse().ok()?,
            expires: expires.parse().ok()?,
        })
    }
}

/// The signed message: `<socket id>:<channel>:<grant>`, with `:<channel_data>` for a presence member.
fn message(socket_id: &str, channel: &str, grant: &str, channel_data: Option<&str>) -> String {
    match channel_data {
        Some(data) => format!("{socket_id}:{channel}:{grant}:{data}"),
        None => format!("{socket_id}:{channel}:{grant}"),
    }
}

/// The `auth` string for `grant` on `channel` of `socket_id` (and, on a presence channel, its `channel_data`).
pub(crate) fn authorize(
    app_key: &str,
    secret: &str,
    socket_id: &str,
    channel: &str,
    grant: &Grant,
    channel_data: Option<&str>,
) -> String {
    let grant = grant.encode();
    let signature = sign(secret, &message(socket_id, channel, &grant, channel_data));
    format!("{app_key}:{grant}:{signature}")
}

/// Why a subscription's `auth` was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Not `<key>:<grant>:<hex>`, another app key, or a wrong signature.
    Invalid,
    /// The grant's time ran out.
    Expired,
}

/// Check `auth` for `channel` on `socket_id` at `now` (Unix seconds), with the presence `channel_data` it must
/// cover; the grant when it holds.
pub(crate) fn verify(
    app_key: &str,
    secret: &str,
    socket_id: &str,
    channel: &str,
    auth: &str,
    channel_data: Option<&str>,
    now: u64,
) -> Result<Grant, Refused> {
    let mut parts = auth.splitn(3, ':');
    let (Some(key), Some(grant_text), Some(signature)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(Refused::Invalid);
    };
    if key != app_key || signature.len() != 64 {
        return Err(Refused::Invalid);
    }
    let expected = sign(
        secret,
        &message(socket_id, channel, grant_text, channel_data),
    );
    if !smeltery_core::crypto::constant_time_eq(&expected, signature) {
        return Err(Refused::Invalid);
    }
    let grant = Grant::decode(grant_text).ok_or(Refused::Invalid)?;
    // A grant dated too far ahead was made by a process whose clock is wrong: refused like an old one.
    let latest = now.saturating_add(GRANT_LIFETIME.as_secs().saturating_mul(2));
    if grant.expires < now || grant.expires > latest || grant.issued > latest {
        return Err(Refused::Expired);
    }
    Ok(grant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        // RFC 4231 test case 2.
        assert_eq!(
            sign("Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn signatures_match_pushers_documented_example() {
        // Pusher's channel authorization docs: key 278d425bdf160c739803, secret 7ad3773142a6692b25b8,
        // socket 1234.1234, channel private-foobar.
        assert_eq!(
            sign("7ad3773142a6692b25b8", "1234.1234:private-foobar"),
            "58df8b0c36d6982b82c3ecf6b4662e34fe8c25bba48f5369f135bf843651c3a4"
        );
    }

    fn grant(expires: u64) -> Grant {
        Grant {
            user: Some(7),
            credential: Credential::from_key("web:session:0a1b2c3d4e5f60718293a4b5"),
            issued: expires - 300,
            expires,
        }
    }

    #[test]
    fn an_auth_string_verifies_only_for_its_socket_channel_and_time() {
        let auth = authorize(
            "key",
            "secret",
            "1.2",
            "private-orders.7",
            &grant(1_000),
            None,
        );
        assert!(auth.starts_with("key:7.web~session~0a1b2c3d4e5f60718293a4b5.700.1000:"));
        let ok = verify("key", "secret", "1.2", "private-orders.7", &auth, None, 900).unwrap();
        assert_eq!(ok, grant(1_000));
        assert_eq!(
            verify("key", "secret", "1.3", "private-orders.7", &auth, None, 900),
            Err(Refused::Invalid),
            "another socket"
        );
        assert_eq!(
            verify("key", "secret", "1.2", "private-orders.8", &auth, None, 900),
            Err(Refused::Invalid),
            "another channel"
        );
        assert_eq!(
            verify("key", "other", "1.2", "private-orders.7", &auth, None, 900),
            Err(Refused::Invalid),
            "another secret"
        );
        assert_eq!(
            verify(
                "other",
                "secret",
                "1.2",
                "private-orders.7",
                &auth,
                None,
                900
            ),
            Err(Refused::Invalid),
            "another key"
        );
        assert_eq!(
            verify(
                "key",
                "secret",
                "1.2",
                "private-orders.7",
                &auth,
                None,
                1_001
            ),
            Err(Refused::Expired)
        );
        assert_eq!(
            verify("key", "secret", "1.2", "private-orders.7", &auth, None, 0),
            Err(Refused::Expired),
            "dated too far ahead"
        );
    }

    #[test]
    fn a_presence_signature_covers_the_member_and_never_passes_as_private() {
        let data = r#"{"user_id":"7","user_info":{"name":"Ada"}}"#;
        let auth = authorize(
            "key",
            "secret",
            "1.2",
            "presence-room.1",
            &grant(1_000),
            Some(data),
        );
        assert!(
            verify(
                "key",
                "secret",
                "1.2",
                "presence-room.1",
                &auth,
                Some(data),
                900
            )
            .is_ok()
        );
        let other = r#"{"user_id":"8"}"#;
        assert_eq!(
            verify(
                "key",
                "secret",
                "1.2",
                "presence-room.1",
                &auth,
                Some(other),
                900
            ),
            Err(Refused::Invalid),
            "another member"
        );
        assert_eq!(
            verify("key", "secret", "1.2", "presence-room.1", &auth, None, 900),
            Err(Refused::Invalid),
            "no member"
        );
        let private = authorize(
            "key",
            "secret",
            "1.2",
            "private-room.1",
            &grant(1_000),
            None,
        );
        assert_eq!(
            verify(
                "key",
                "secret",
                "1.2",
                "presence-room.1",
                &private,
                None,
                900
            ),
            Err(Refused::Invalid),
            "a private signature on the presence channel"
        );
    }

    #[test]
    fn an_edited_grant_breaks_the_signature() {
        let auth = authorize("key", "secret", "1.2", "private-x", &grant(1_000), None);
        let edited = auth.replacen(":7.", ":8.", 1);
        assert_eq!(
            verify("key", "secret", "1.2", "private-x", &edited, None, 900),
            Err(Refused::Invalid)
        );
        // A plain Pusher `key:hex` string has no grant: refused.
        let plain = format!("key:{}", sign("secret", "1.2:private-x"));
        assert_eq!(
            verify("key", "secret", "1.2", "private-x", &plain, None, 900),
            Err(Refused::Invalid)
        );
        assert_eq!(
            verify("key", "secret", "1.2", "private-x", "", None, 900),
            Err(Refused::Invalid)
        );
    }

    #[test]
    fn guests_and_sessionless_grants_round_trip() {
        let g = Grant {
            user: None,
            credential: None,
            issued: 1,
            expires: 5,
        };
        assert_eq!(Grant::decode(&g.encode()), Some(g));
        let t = Grant {
            user: Some(3),
            credential: Credential::from_key("hallmark:token:42"),
            issued: 1,
            expires: 5,
        };
        assert_eq!(t.encode(), "3.hallmark~token~42.1.5");
        assert_eq!(Grant::decode(&t.encode()), Some(t));
        assert_eq!(Grant::decode("7.web~session~zz.1.5"), None);
        assert_eq!(Grant::decode("7.-.1.5.6"), None);
        assert_eq!(Grant::decode("x.-.1.5"), None);
        assert_eq!(Grant::decode("7.web~token~x.1.5"), None);
        assert_eq!(Grant::decode("7.Web~token~1.1.5"), None);
        assert_eq!(Grant::decode("7.-.5"), None);
        assert_eq!(
            Grant::decode("7.-.1.5"),
            None,
            "a user's grant without a credential"
        );
        assert!(Credential::from_key("hallmark:token:05").is_none());
        assert!(Credential::from_key("hallmark:token:+5").is_none());
        assert!(Credential::from_key("hallmark:token:-5").is_some());
        assert!(Credential::from_key(":token:5").is_none());
        let long_guard = format!("{}:token:5", "g".repeat(100));
        assert!(
            Credential::from_key(&long_guard).is_some(),
            "core sets no length limit on guard names"
        );
        assert!(
            Credential::from_key("session:abc").is_none(),
            "the guard is part of the key"
        );
        assert!(Credential::from_key("web:session:abc:x").is_none());
        assert_eq!(
            Credential::from_key("web:session:abc").map(|c| c.key().to_owned()),
            Some("web:session:abc".to_owned())
        );
    }
}
