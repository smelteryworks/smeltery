//! Listeners: component methods that run when a broadcast event reaches the page (`#[on("anvil:…", "…")]`).
//!
//! At render, each listener's channel template is resolved against the state and authorized for the viewer through
//! the app's [`ChannelAuthorizer`](smeltery_core::channels::ChannelAuthorizer); the allowed `(channel, event)` pairs go
//! into the stream token. The stream sends a matching event to the page as a `listen` message signed under the
//! purpose `sparks.listen` (instance, channel, event, the data's hash, expiry, sequence number), and the page calls
//! `$listen` with it. `$listen` runs the listener only for a valid, unexpired, unreplayed message whose channel the
//! state still names and the viewer may still receive (D-409, amendment A1).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use smeltery_core::auth::Auth;
use smeltery_core::{App, Error, Result};

use crate::component::{ListenerInfo, Spark};
use crate::snapshot::canonical;

/// The signing purpose of listen messages.
const PURPOSE: &str = "sparks.listen";

/// How long a listen message may wait before the page sends it back.
pub(crate) const MESSAGE_TTL: Duration = Duration::from_secs(60);

/// The longest channel name (Pusher's limit, prefixes included).
const MAX_CHANNEL: usize = 164;

/// The largest event data a listen message carries (larger events are not forwarded).
pub(crate) const MAX_DATA: usize = 64 * 1024;

/// The last sequence number this process gave a listen message.
static LAST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A fresh sequence number: microseconds since the epoch, strictly increasing in this process.
pub(crate) fn next_sequence() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
    let mut last = LAST_SEQUENCE.load(Ordering::Relaxed);
    loop {
        let next = now.max(last.saturating_add(1));
        match LAST_SEQUENCE.compare_exchange_weak(last, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

/// A byte a channel name may hold: letters, digits and `_ - = @ , . ;`.
fn channel_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'=' | b'@' | b',' | b'.' | b';')
}

/// One piece of a channel template.
enum Piece<'a> {
    Text(&'a str),
    Field(&'a str),
}

fn pieces(template: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('{')
            && let Some(end) = after.find('}')
        {
            out.push(Piece::Field(after.get(..end).unwrap_or_default()));
            rest = after.get(end + 1..).unwrap_or_default();
        } else {
            let len = rest
                .get(1..)
                .and_then(|r| r.find('{'))
                .map_or(rest.len(), |i| i + 1);
            out.push(Piece::Text(rest.get(..len).unwrap_or_default()));
            rest = rest.get(len..).unwrap_or_default();
        }
    }
    out
}

/// The text a state value gives a `{field}`: a string or an integer whose text is letters, digits and `_ - = @ , ;`
/// (no `.`: a value never adds a channel segment).
fn field_text(value: &serde_json::Value) -> Option<String> {
    let text = match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        _ => return None,
    };
    (!text.is_empty() && text.bytes().all(|b| channel_byte(b) && b != b'.')).then_some(text)
}

/// The channel `template` names for `state` (the component's state as JSON): `None` when a field is missing or its
/// value cannot be part of a channel name.
pub(crate) fn resolve(template: &str, state: &serde_json::Value) -> Option<String> {
    let mut out = String::new();
    for piece in pieces(template) {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Field(name) => out.push_str(&field_text(state.get(name)?)?),
        }
    }
    (!out.is_empty() && out.len() <= MAX_CHANNEL && out.bytes().all(channel_byte)).then_some(out)
}

/// Whether `channel` has the shape of `template` (each `{field}` standing for one or more characters without `.`).
pub(crate) fn fits(template: &str, channel: &str) -> bool {
    fn go(pieces: &[Piece<'_>], rest: &str) -> bool {
        match pieces.split_first() {
            None => rest.is_empty(),
            Some((Piece::Text(text), tail)) => rest.strip_prefix(text).is_some_and(|r| go(tail, r)),
            Some((Piece::Field(_), tail)) => {
                let max = rest.find('.').unwrap_or(rest.len());
                (1..=max).any(|n| rest.get(n..).is_some_and(|r| go(tail, r)))
            }
        }
    }
    go(&pieces(template), channel)
}

/// The fields `template` names.
pub(crate) fn fields(template: &str) -> Vec<&str> {
    pieces(template)
        .into_iter()
        .filter_map(|p| match p {
            Piece::Field(name) => Some(name),
            Piece::Text(_) => None,
        })
        .collect()
}

/// The `(channel, event)` pairs `T`'s listeners may receive for this viewer: each template resolved against
/// `state` and authorized through the app's channel authorizer. A listener whose channel does not resolve, is denied
/// or whose authorization fails gets nothing (failures are logged).
pub(crate) async fn grants<T: Spark>(
    app: &App,
    state: &serde_json::Value,
    auth: Option<&Auth>,
) -> Result<Vec<(String, String)>> {
    let listeners: &[ListenerInfo] = <T as crate::Actions>::LISTENERS;
    if listeners.is_empty() {
        return Ok(Vec::new());
    }
    let Some(authorizer) = app.channel_authorizer() else {
        return Err(no_authorizer(T::NAME));
    };
    let mut decided: Vec<(String, bool)> = Vec::new();
    let mut out: Vec<(String, String)> = Vec::new();
    for listener in listeners {
        let Some(channel) = resolve(listener.channel(), state) else {
            tracing::warn!(
                component = T::NAME,
                listener = listener.method(),
                "Sparks listener skipped: its channel does not resolve from the state"
            );
            continue;
        };
        let allowed = match decided.iter().find(|(c, _)| *c == channel) {
            Some((_, allowed)) => *allowed,
            None => {
                let allowed = match authorizer.authorize(app, &channel, auth).await {
                    Ok(allowed) => allowed,
                    Err(e) if !e.status().is_server_error() => false,
                    Err(e) => {
                        tracing::error!(
                            component = T::NAME,
                            listener = listener.method(),
                            error = %e,
                            "Sparks listener skipped: the channel authorization failed"
                        );
                        false
                    }
                };
                decided.push((channel.clone(), allowed));
                allowed
            }
        };
        let pair = (channel, listener.event().to_owned());
        if allowed && !out.contains(&pair) {
            out.push(pair);
        }
    }
    Ok(out)
}

/// The error of an app whose component declares listeners without a broadcasting crate.
pub(crate) fn no_authorizer(component: &str) -> Error {
    Error::internal(format!(
        "Spark `{component}` declares `#[on(\"anvil:…\")]` listeners, but Anvil is not installed: add `.anvil(…)` \
         in bootstrap/app.rs"
    ))
}

/// What a listen message's signature covers; `data_hash` is the hex SHA-256 of the event's data.
fn signed_text(
    id: &str,
    channel: &str,
    event: &str,
    data_hash: &str,
    exp: u64,
    seq: u64,
) -> String {
    canonical(&serde_json::json!({
        "i": id,
        "c": channel,
        "e": event,
        "d": data_hash,
        "x": exp,
        "n": seq,
    }))
}

/// A listen message for instance `id`: `(exp, seq, sig)`. `data_hash` is the hex SHA-256 of the event's data,
/// computed once per event for every listener it reaches.
pub(crate) fn sign(
    app: &App,
    id: &str,
    channel: &str,
    event: &str,
    data_hash: &str,
) -> Result<(u64, u64, String)> {
    let exp = crate::upload::now_secs().saturating_add(MESSAGE_TTL.as_secs());
    let seq = next_sequence();
    let sig = app.sign(
        PURPOSE,
        signed_text(id, channel, event, data_hash, exp, seq).as_bytes(),
    )?;
    Ok((exp, seq, sig))
}

/// Record that the listen message `seq` of instance `id` runs now: `Ok` the first time; 403 when it already ran
/// (with any snapshot of the instance: a seen-set in the app's cache, kept for the message's lifetime, shared by the
/// app's processes with a shared store). A cache failure refuses the message (fail closed).
pub(crate) async fn claim(app: &App, id: &str, seq: u64, component: &str) -> Result<()> {
    let key = format!("sparks.listen:{id}:{seq}");
    match app
        .cache()
        .add(&key, &1_u8, MESSAGE_TTL + Duration::from_secs(1))
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(refused(component, "already ran")),
        Err(e) => {
            tracing::error!(component, error = %e, "Sparks listen refused: the cache failed");
            Err(Error::http(
                http::StatusCode::SERVICE_UNAVAILABLE,
                "the listen message could not be checked",
            ))
        }
    }
}

/// A `$listen` call, as the page sends it back: `[channel, event, data, exp, seq, sig]`.
#[derive(Debug)]
pub(crate) struct ListenCall {
    pub(crate) channel: String,
    pub(crate) event: String,
    pub(crate) data: String,
    pub(crate) exp: u64,
    pub(crate) seq: u64,
    pub(crate) sig: String,
}

impl ListenCall {
    /// The call's parameters, when they have the message's shape (otherwise 400).
    pub(crate) fn parse(params: &[serde_json::Value]) -> Result<Self> {
        let text = |i: usize| {
            params
                .get(i)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let number = |i: usize| params.get(i).and_then(serde_json::Value::as_u64);
        match (
            params.len(),
            text(0),
            text(1),
            text(2),
            number(3),
            number(4),
            text(5),
        ) {
            (6, Some(channel), Some(event), Some(data), Some(exp), Some(seq), Some(sig)) => {
                Ok(Self {
                    channel,
                    event,
                    data,
                    exp,
                    seq,
                    sig,
                })
            }
            _ => {
                tracing::warn!("Sparks listen rejected: malformed message");
                Err(Error::bad_request("malformed `$listen` call"))
            }
        }
    }

    /// Whether the server signed this message for instance `id` and it has not expired.
    pub(crate) fn verify(&self, app: &App, id: &str) -> bool {
        let text = signed_text(
            id,
            &self.channel,
            &self.event,
            &smeltery_core::crypto::sha256_hex(&self.data),
            self.exp,
            self.seq,
        );
        app.verify_signature(PURPOSE, text.as_bytes(), &self.sig)
            && crate::upload::now_secs() <= self.exp
    }
}

/// 403 for a listen message the server refuses.
pub(crate) fn refused(component: &str, reason: &'static str) -> Error {
    tracing::warn!(component, reason, "Sparks listen rejected");
    Error::http(
        http::StatusCode::FORBIDDEN,
        format!("the listen message was refused: {reason}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_resolve_from_the_state() {
        let state = serde_json::json!({ "order_id": 7, "room": "lobby", "dotted": "a.b", "empty": "", "f": 1.5 });
        assert_eq!(
            resolve("private-orders.{order_id}", &state).as_deref(),
            Some("private-orders.7")
        );
        assert_eq!(
            resolve("chat-{room}.x", &state).as_deref(),
            Some("chat-lobby.x")
        );
        assert_eq!(resolve("news", &state).as_deref(), Some("news"));
        assert_eq!(
            resolve("a.{dotted}", &state),
            None,
            "a value never adds a segment"
        );
        assert_eq!(resolve("a.{empty}", &state), None);
        assert_eq!(resolve("a.{f}", &state), None);
        assert_eq!(resolve("a.{missing}", &state), None);
        assert_eq!(
            resolve(&format!("{}{{room}}", "x".repeat(160)), &state),
            None,
            "too long"
        );
        assert_eq!(fields("private-{a}.{b}x"), ["a", "b"]);
    }

    #[test]
    fn channels_fit_their_templates() {
        assert!(fits("private-orders.{order_id}", "private-orders.7"));
        assert!(fits("private-orders.{order_id}", "private-orders.abc"));
        assert!(!fits("private-orders.{order_id}", "private-orders."));
        assert!(!fits("private-orders.{order_id}", "private-orders.7.8"));
        assert!(!fits("private-orders.{order_id}", "private-users.7"));
        assert!(fits("a-{x}-{y}", "a-1-2-3"));
        assert!(fits("news", "news"));
        assert!(!fits("news", "news2"));
    }

    #[test]
    fn sequence_numbers_only_grow() {
        let a = next_sequence();
        let b = next_sequence();
        assert!(b > a);
    }
}
