//! Snapshots: the component state carried by the page, signed with a key derived from `APP_KEY`.

use std::collections::BTreeMap;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use smeltery_core::auth::Auth;
use smeltery_core::session::Session;
use smeltery_core::{App, Error, Result};

use crate::PROTOCOL_VERSION;

/// The signing purpose of snapshot checksums.
const PURPOSE: &str = "sparks.snapshot";

/// An instance's children: key → (child id, child name).
pub(crate) type Children = BTreeMap<String, (String, String)>;

/// What the server remembers about an instance besides its state.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Memo {
    pub(crate) id: String,
    pub(crate) name: String,
    /// key → (child id, child name).
    #[serde(default)]
    pub(crate) children: Children,
    /// The session the snapshot was issued to: a hash of its CSRF secret (empty without a session).
    pub(crate) s: String,
    /// The signed-in user it was issued to.
    pub(crate) u: Option<i64>,
    /// When it was issued, Unix seconds.
    pub(crate) t: u64,
    /// The sequence number of the last listen message this instance ran (0: none), so a message runs once.
    #[serde(default)]
    pub(crate) l: u64,
}

impl Memo {
    /// The memo of instance `id` of component `name`, bound to `session` and the user signed in with `auth`, issued
    /// now.
    pub(crate) fn new(
        id: String,
        name: &str,
        children: Children,
        session: Option<&Session>,
        auth: Option<&Auth>,
    ) -> Self {
        Self {
            id,
            name: name.to_owned(),
            children,
            s: session.map(crate::upload::binding).unwrap_or_default(),
            u: auth.and_then(Auth::id),
            t: crate::upload::now_secs(),
            l: 0,
        }
    }

    /// Whether this snapshot may drive the request of `session` and `auth` now: issued to the same session and
    /// user, at most `ttl` ago. A snapshot is only as trustworthy as the request that produced it: copied to
    /// another browser, kept past a logout or a user switch, or kept too long, it answers 419 (the page reloads).
    pub(crate) fn check(&self, session: &Session, auth: &Auth, ttl: Duration) -> Result<()> {
        let reason = if !crate::upload::same_secret(&self.s, &crate::upload::binding(session)) {
            "another session"
        } else if self.u != auth.id() {
            "another user"
        } else if crate::upload::now_secs().saturating_sub(self.t) > ttl.as_secs() {
            "too old"
        } else {
            return Ok(());
        };
        tracing::warn!(component = %self.name, reason, "Sparks update rejected: snapshot");
        Err(expired())
    }
}

/// A verified snapshot.
#[derive(Debug)]
pub(crate) struct Opened {
    pub(crate) data: serde_json::Value,
    pub(crate) memo: Memo,
}

/// 419 "Page Expired": the client reloads.
pub(crate) fn expired() -> Error {
    let status = http::StatusCode::from_u16(419).unwrap_or(http::StatusCode::FORBIDDEN);
    Error::http(status, "Page Expired")
}

/// Canonical JSON: object keys sorted at every level, no whitespace, `serde_json`'s string and number forms.
pub(crate) fn canonical(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String(k.clone()).to_string());
                out.push(':');
                if let Some(v) = map.get(k) {
                    write_canonical(v, out);
                }
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(v, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The snapshot text for `data` and `memo`, with its checksum.
pub(crate) fn seal(app: &App, data: serde_json::Value, memo: &Memo) -> Result<String> {
    let mut snapshot = serde_json::json!({
        "v": PROTOCOL_VERSION,
        "data": data,
        "memo": memo,
    });
    let checksum = app.sign(PURPOSE, canonical(&snapshot).as_bytes())?;
    if let serde_json::Value::Object(map) = &mut snapshot {
        map.insert("checksum".to_owned(), serde_json::Value::String(checksum));
    }
    Ok(canonical(&snapshot))
}

/// Parse and verify a snapshot: a wrong version, bad JSON or a checksum that does not match is a 419.
pub(crate) fn open(app: &App, text: &str) -> Result<Opened> {
    let mut value: serde_json::Value = serde_json::from_str(text).map_err(|_| expired())?;
    let serde_json::Value::Object(map) = &mut value else {
        return Err(expired());
    };
    let Some(serde_json::Value::String(checksum)) = map.remove("checksum") else {
        return Err(expired());
    };
    if map.get("v").and_then(serde_json::Value::as_u64) != Some(u64::from(PROTOCOL_VERSION)) {
        return Err(expired());
    }
    if !app.verify_signature(PURPOSE, canonical(&value).as_bytes(), &checksum) {
        return Err(expired());
    }
    let serde_json::Value::Object(mut map) = value else {
        return Err(expired());
    };
    let memo: Memo = map
        .remove("memo")
        .and_then(|m| serde_json::from_value(m).ok())
        .ok_or_else(expired)?;
    let data = map.remove("data").ok_or_else(expired)?;
    Ok(Opened { data, memo })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_sorts_keys_at_every_level() {
        let v =
            serde_json::json!({"b": 1, "a": {"y": [1, {"d": 2, "c": 3}], "x": "é\"q"}, "c": null});
        assert_eq!(
            canonical(&v),
            r#"{"a":{"x":"é\"q","y":[1,{"c":3,"d":2}]},"b":1,"c":null}"#
        );
    }
}
