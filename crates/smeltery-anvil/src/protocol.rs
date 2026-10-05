//! The Pusher Channels protocol 7 as Anvil speaks it: frames, channel names, socket ids, close codes (D-400,
//! D-407).

use serde::Deserialize;
use serde_json::{Value, json};

/// The protocol versions Anvil accepts in the handshake's `?protocol=`.
pub const PROTOCOLS: std::ops::RangeInclusive<u32> = 5..=7;

/// The longest channel name, prefix included (Pusher's limit).
pub const MAX_CHANNEL_NAME: usize = 164;

/// Close codes Anvil sends. pusher-js reconnects after codes below 4000 (with a back-off for 1002-1004),
/// refuses to reconnect after 4001-4099, backs off after 4100-4199 and reconnects at once after 4200-4299: a
/// temporary condition is never closed with a code that stops the client for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CloseCode {
    /// 1001: the server is shutting down (the client reconnects).
    GoingAway,
    /// 1003: a binary frame (the protocol is JSON text).
    Unsupported,
    /// 1009: a message larger than `ANVIL_MAX_MESSAGE_SIZE`.
    TooBig,
    /// 4007: an unsupported protocol version (the client does not reconnect).
    ProtocolUnsupported,
    /// 4008: no protocol version (the client does not reconnect).
    ProtocolMissing,
    /// 4009: the `Origin` is not allowed (the client does not reconnect).
    OriginRefused,
    /// 4100: over capacity, a slow reader or a flood (the client backs off, then reconnects).
    OverCapacity,
    /// 4200: the connection is too old (the client reconnects at once).
    Reconnect,
    /// 4201: the server's ping was not answered (the client reconnects at once).
    PongMissing,
}

impl CloseCode {
    /// The code on the wire.
    pub fn code(self) -> u16 {
        match self {
            Self::GoingAway => 1001,
            Self::Unsupported => 1003,
            Self::TooBig => 1009,
            Self::ProtocolUnsupported => 4007,
            Self::ProtocolMissing => 4008,
            Self::OriginRefused => 4009,
            Self::OverCapacity => 4100,
            Self::Reconnect => 4200,
            Self::PongMissing => 4201,
        }
    }

    /// The close reason when the hub closes a socket with this code (a revocation, a lagged auth stream, a slow
    /// reader), with the meaning protocol 7 gives the code.
    pub(crate) fn hub_reason(self) -> &'static str {
        match self {
            Self::GoingAway => "server shutting down",
            Self::Unsupported => "binary frames are not supported",
            Self::TooBig => "message too big",
            Self::ProtocolUnsupported => "unsupported protocol version",
            Self::ProtocolMissing => "no protocol version supplied",
            Self::OriginRefused => "origin not allowed",
            Self::OverCapacity => "over capacity",
            // The hub asks for 4200 only when an authorization ended (a revocation, or grants it can no longer check).
            Self::Reconnect => "authorization revoked",
            Self::PongMissing => "pong reply not received",
        }
    }
}

/// The kind of a channel, from its name's prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// No known prefix.
    Public,
    /// `private-`.
    Private,
    /// `presence-`.
    Presence,
    /// `private-encrypted-`: refused.
    Unsupported,
}

impl Kind {
    /// The kind of `name` and the name without its prefix.
    pub(crate) fn of(name: &str) -> (Self, &str) {
        if name.starts_with("private-encrypted-") {
            (Self::Unsupported, name)
        } else if let Some(rest) = name.strip_prefix("private-") {
            (Self::Private, rest)
        } else if let Some(rest) = name.strip_prefix("presence-") {
            (Self::Presence, rest)
        } else {
            (Self::Public, name)
        }
    }
}

/// Whether `name` is a channel name: 1 to 164 bytes of `A-Z a-z 0-9 _ - = @ , . ;`.
pub fn valid_channel(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_CHANNEL_NAME && name.bytes().all(channel_byte)
}

/// A byte of a channel name.
pub(crate) fn channel_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'=' | b'@' | b',' | b'.' | b';')
}

/// Whether `id` has the shape of a socket id: two numbers of 1 to 10 digits joined by `.`.
pub fn valid_socket_id(id: &str) -> bool {
    let Some((a, b)) = id.split_once('.') else {
        return false;
    };
    let digits = |s: &str| (1..=10).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    digits(a) && digits(b)
}

/// A new socket id: two random `u32`s from the OS random source.
pub(crate) fn new_socket_id() -> smeltery_core::Result<String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|e| smeltery_core::Error::internal(format!("no random source: {e}")))?;
    let [a0, a1, a2, a3, b0, b1, b2, b3] = bytes;
    Ok(format!(
        "{}.{}",
        u32::from_le_bytes([a0, a1, a2, a3]),
        u32::from_le_bytes([b0, b1, b2, b3])
    ))
}

/// A frame from a client: `{"event": …, "data": …, "channel"?: …}`.
#[derive(Debug, Deserialize)]
pub(crate) struct ClientFrame {
    pub(crate) event: String,
    #[serde(default)]
    pub(crate) data: Value,
    /// A client event's channel.
    #[serde(default)]
    pub(crate) channel: Option<String>,
}

/// The data of `pusher:subscribe` / `pusher:unsubscribe` (an object; some clients send it as a JSON string).
#[derive(Debug, Deserialize)]
pub(crate) struct SubscribeData {
    pub(crate) channel: String,
    #[serde(default)]
    pub(crate) auth: Option<String>,
    /// A presence subscription's member, as the auth endpoint signed it (JSON text).
    #[serde(default)]
    pub(crate) channel_data: Option<String>,
}

impl SubscribeData {
    pub(crate) fn from_value(data: Value) -> Option<Self> {
        match data {
            Value::String(text) => serde_json::from_str(&text).ok(),
            other => serde_json::from_value(other).ok(),
        }
    }
}

/// `pusher:connection_established`.
pub(crate) fn connection_established(socket_id: &str, activity_timeout: u64) -> String {
    let data = json!({ "socket_id": socket_id, "activity_timeout": activity_timeout }).to_string();
    json!({ "event": "pusher:connection_established", "data": data }).to_string()
}

/// `pusher:pong`.
pub(crate) fn pong() -> String {
    r#"{"event":"pusher:pong","data":"{}"}"#.to_owned()
}

/// `pusher:ping`.
pub(crate) fn ping() -> String {
    r#"{"event":"pusher:ping","data":"{}"}"#.to_owned()
}

/// `pusher_internal:subscription_succeeded`.
pub(crate) fn subscription_succeeded(channel: &str) -> String {
    json!({ "event": "pusher_internal:subscription_succeeded", "channel": channel, "data": "{}" })
        .to_string()
}

/// The largest `subscription_succeeded` frame of a presence channel of at most `members` members whose
/// `channel_data` is at most `member_bytes`: per member its id twice (at most 128 bytes, each byte escaped to at most
/// 2) and its `user_info` (at most `member_bytes`), all escaped once more because the data is a string in the frame.
pub(crate) fn presence_list_bound(members: usize, member_bytes: usize) -> usize {
    let id = 2 * crate::presence::MAX_USER_ID + 4;
    let per_member = member_bytes.saturating_add(2 * id).saturating_add(16);
    members
        .saturating_mul(per_member.saturating_mul(2))
        .saturating_add(512)
}

/// `pusher_internal:subscription_succeeded` on a presence channel: the members (`ids`, `hash` of `user_info`,
/// `count`).
pub(crate) fn presence_succeeded(channel: &str, members: &[crate::presence::Member]) -> String {
    let ids: Vec<&str> = members
        .iter()
        .map(crate::presence::Member::user_id)
        .collect();
    let hash: serde_json::Map<String, Value> = members
        .iter()
        .map(|m| {
            (
                m.user_id().to_owned(),
                m.user_info().cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
    let data =
        json!({ "presence": { "ids": ids, "hash": hash, "count": members.len() } }).to_string();
    json!({ "event": "pusher_internal:subscription_succeeded", "channel": channel, "data": data })
        .to_string()
}

/// The data of `pusher_internal:member_added` (JSON text).
pub(crate) fn member_added_data(member: &crate::presence::Member) -> String {
    let mut data = json!({ "user_id": member.user_id() });
    if let (Some(info), Some(object)) = (member.user_info(), data.as_object_mut()) {
        object.insert("user_info".into(), info.clone());
    }
    data.to_string()
}

/// The data of `pusher_internal:member_removed` (JSON text).
pub(crate) fn member_removed_data(user_id: &str) -> String {
    json!({ "user_id": user_id }).to_string()
}

/// A client event as the other subscribers get it: `data` as the sender sent it, `user_id` on presence channels.
pub(crate) fn client_event(
    name: &str,
    channel: &str,
    data: &Value,
    user_id: Option<&str>,
) -> String {
    let mut frame = json!({ "event": name, "channel": channel, "data": data });
    if let (Some(user_id), Some(object)) = (user_id, frame.as_object_mut()) {
        object.insert("user_id".into(), Value::String(user_id.to_owned()));
    }
    frame.to_string()
}

/// `pusher:subscription_error`: the subscription was refused; the socket stays open.
pub(crate) fn subscription_error(channel: &str, status: u16, error: &str) -> String {
    let data = json!({ "type": "AuthError", "error": error, "status": status }).to_string();
    json!({ "event": "pusher:subscription_error", "channel": channel, "data": data }).to_string()
}

/// `pusher:error` (an object `data`, as Pusher sends it).
pub(crate) fn error(code: u16, message: &str) -> String {
    json!({ "event": "pusher:error", "data": { "code": code, "message": message } }).to_string()
}

/// An event on a channel: `data` is the event's JSON as a string.
pub(crate) fn event(name: &str, channel: &str, data: &str) -> String {
    json!({ "event": name, "channel": channel, "data": data }).to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn hub_closes_give_the_reason_of_their_code() {
        use super::CloseCode;
        assert_eq!(CloseCode::Reconnect.hub_reason(), "authorization revoked");
        assert_eq!(CloseCode::OverCapacity.hub_reason(), "over capacity");
        assert_eq!(CloseCode::GoingAway.hub_reason(), "server shutting down");
        assert_eq!(
            CloseCode::PongMissing.hub_reason(),
            "pong reply not received"
        );
        assert_eq!(CloseCode::OriginRefused.hub_reason(), "origin not allowed");
        for code in [
            CloseCode::GoingAway,
            CloseCode::Unsupported,
            CloseCode::TooBig,
            CloseCode::ProtocolUnsupported,
            CloseCode::ProtocolMissing,
            CloseCode::OriginRefused,
            CloseCode::OverCapacity,
            CloseCode::Reconnect,
            CloseCode::PongMissing,
        ] {
            // A close frame's reason is at most 123 bytes.
            assert!(!code.hub_reason().is_empty() && code.hub_reason().len() <= 123);
        }
    }

    use super::*;

    #[test]
    fn channel_names_follow_the_pusher_rules() {
        assert!(valid_channel("private-orders.7"));
        assert!(valid_channel("a-b_c=d@e,f.g;h"));
        assert!(valid_channel(&"a".repeat(164)));
        assert!(!valid_channel(&"a".repeat(165)));
        assert!(!valid_channel(""));
        assert!(!valid_channel("orders#7"));
        assert!(!valid_channel("orders 7"));
        assert!(!valid_channel("ordérs"));
    }

    #[test]
    fn kinds_come_from_the_prefix() {
        assert_eq!(Kind::of("private-orders.7"), (Kind::Private, "orders.7"));
        assert_eq!(Kind::of("posts"), (Kind::Public, "posts"));
        assert_eq!(Kind::of("presence-room.1"), (Kind::Presence, "room.1"));
        assert_eq!(Kind::of("private-encrypted-x").0, Kind::Unsupported);
    }

    /// Sweep W5-03: the bound holds for the worst members the auth endpoint signs (ids and `user_info` made of
    /// characters JSON escapes, `channel_data` at its limit).
    #[test]
    fn the_member_list_bound_holds_for_the_worst_members() {
        use crate::presence::{MAX_USER_ID, Member};
        let member_bytes = 1024;
        let members: Vec<Member> = (0..100)
            .map(|n| {
                let id = format!("{n:03}{}", "\"".repeat(MAX_USER_ID - 3));
                let base = Member::new(&id).info(json!("")).channel_data().len();
                // Fill `user_info` with quotes up to the limit (each costs 2 bytes in `channel_data`).
                let room = (member_bytes - base) / 2;
                let member = Member::new(&id).info(json!("\"".repeat(room)));
                assert!(member.channel_data().len() <= member_bytes);
                member
            })
            .collect();
        let frame = presence_succeeded("presence-room.1", &members);
        assert!(
            frame.len() <= presence_list_bound(100, member_bytes),
            "{} > {}",
            frame.len(),
            presence_list_bound(100, member_bytes)
        );
    }

    #[test]
    fn socket_ids_have_the_expected_shape() {
        for _ in 0..50 {
            assert!(valid_socket_id(&new_socket_id().unwrap()));
        }
        assert!(valid_socket_id("1234.1234"));
        assert!(!valid_socket_id("1234"));
        assert!(!valid_socket_id("12345678901.1"));
        assert!(!valid_socket_id("1.2.3"));
        assert!(!valid_socket_id("a.1"));
        assert!(!valid_socket_id(".1"));
    }

    #[test]
    fn close_codes_never_stop_a_client_for_a_temporary_reason() {
        for code in [
            CloseCode::OverCapacity,
            CloseCode::Reconnect,
            CloseCode::PongMissing,
        ] {
            assert!((4100..4300).contains(&code.code()));
        }
        assert_eq!(CloseCode::GoingAway.code(), 1001);
    }

    #[test]
    fn server_frames_carry_data_as_a_string() {
        let frame: Value = serde_json::from_str(&connection_established("1.2", 30)).unwrap();
        let data: Value = serde_json::from_str(frame["data"].as_str().unwrap()).unwrap();
        assert_eq!(data["socket_id"], "1.2");
        assert_eq!(data["activity_timeout"], 30);
        let frame: Value = serde_json::from_str(&event("x", "posts", r#"{"a":1}"#)).unwrap();
        assert_eq!(frame["data"], r#"{"a":1}"#);
    }
}
