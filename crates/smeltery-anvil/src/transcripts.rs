//! The golden transcripts of `tests/golden/*.txt`, replayed against the sans-IO session (ANVIL.md §6.1).
//!
//! Lines: `# comment`; `set <name>=<value>` (before anything else: `max_subscriptions`, `max_presence_channels`,
//! `ping_interval` and
//! `max_age` in seconds); `open` (the first frame); `> <frame>` (a client text frame, `*n` at the end repeats it;
//! `{auth:<channel>}`, `{auth-other-socket:…}`, `{auth-other-secret:…}`, `{auth-plain:…}` and `{auth-expired:…}`
//! stand for `auth` strings); `binary`, `control` (a WebSocket ping or pong), `shutdown`; `+ <n>s` / `+ <n>ms`
//! (time passes, then a tick). The server's output follows as `< <frame>` (compared as JSON, a `data` string as
//! the JSON inside it), `= subscribe <channel>`, `= unsubscribe <channel>`, `= presence-join <channel> <user id>`,
//! `= presence-list <channel>`, `= presence-leave <channel> <user id>`, `= whisper <channel> <event> <data> <user
//! id or ->` and `x <close code>`; output that the transcript does not list fails it. `{auth-presence:<channel>}` is
//! the `auth` of [`MEMBER`] on a presence channel.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::string_slice
)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use crate::channels::Channels;
use crate::session::{Action, Config, Now, Session};
use crate::signature::{self, Grant};

const SOCKET: &str = "1234.1234";
const KEY: &str = "app-key";
const SECRET: &str = "app-secret";
/// The `channel_data` `{auth-presence:…}` signs.
const MEMBER: &str = r#"{"user_id":"7","user_info":{"name":"Ada"}}"#;
/// The Unix time the transcripts start at.
const START: u64 = 1_700_000_000;

fn config(settings: &[(String, u64)]) -> Config {
    let mut channels = Channels::new();
    channels.public("news");
    channels.public("scores.{game}");
    channels.private("orders.{order}", |_| async { Ok(true) });
    channels
        .private("chat.{chat}", |_| async { Ok(true) })
        .whispers();
    channels
        .presence("room.{room}", |_| async { Ok(None) })
        .whispers();
    channels.presence("hall.{hall}", |_| async { Ok(None) });
    let get = |name: &str, default: u64| {
        settings
            .iter()
            .find(|(n, _)| n == name)
            .map_or(default, |(_, v)| *v)
    };
    Config {
        app_key: KEY.into(),
        secret: SECRET.into(),
        channels: Arc::new(channels),
        activity_timeout: Duration::from_secs(30),
        ping_interval: Duration::from_secs(get("ping_interval", 60)),
        pong_timeout: Duration::from_secs(30),
        max_age: Duration::from_secs(get("max_age", 86_400)),
        max_subscriptions: usize::try_from(get("max_subscriptions", 100)).unwrap_or(100),
        max_presence_channels: usize::try_from(get("max_presence_channels", 10)).unwrap_or(10),
        frames_per_second: 20,
        frame_burst: 40,
        revocations: Arc::default(),
    }
}

/// `{auth…:<channel>}` placeholders replaced by auth strings.
fn fill(frame: &str, unix: u64) -> String {
    let mut out = frame.to_owned();
    while let Some(start) = out.find("{auth") {
        let end = start + out[start..].find('}').expect("closing brace");
        let (kind, channel) = out[start + 1..end]
            .split_once(':')
            .expect("{auth…:channel}");
        let grant = |expires: u64| Grant {
            user: Some(7),
            credential: crate::signature::Credential::from_key(
                "web:session:00112233445566778899aabb",
            ),
            issued: expires.saturating_sub(300),
            expires,
        };
        let auth = match kind {
            "auth" => signature::authorize(KEY, SECRET, SOCKET, channel, &grant(unix + 300), None),
            "auth-presence" => signature::authorize(
                KEY,
                SECRET,
                SOCKET,
                channel,
                &grant(unix + 300),
                Some(MEMBER),
            ),
            "auth-other-socket" => {
                signature::authorize(KEY, SECRET, "9.9", channel, &grant(unix + 300), None)
            }
            "auth-other-secret" => {
                signature::authorize(KEY, "other", SOCKET, channel, &grant(unix + 300), None)
            }
            "auth-plain" => format!(
                "{KEY}:{}",
                signature::sign(SECRET, &format!("{SOCKET}:{channel}"))
            ),
            "auth-expired" => {
                signature::authorize(KEY, SECRET, SOCKET, channel, &grant(unix - 1), None)
            }
            other => panic!("unknown placeholder {other}"),
        };
        out.replace_range(start..=end, &auth);
    }
    out
}

/// A frame for comparison: a `data` string holding JSON is replaced by that JSON.
fn normalize(mut frame: Value) -> Value {
    if let Some(data) = frame.get_mut("data")
        && let Some(text) = data.as_str()
        && let Ok(inner) = serde_json::from_str::<Value>(text)
    {
        *data = inner;
    }
    frame
}

fn describe(action: &Action) -> String {
    match action {
        Action::Send(text) => format!("< {text}"),
        Action::Close(code, _) => format!("x {}", code.code()),
        Action::Subscribe(c, _) => format!("= subscribe {c}"),
        Action::Unsubscribe(c) => format!("= unsubscribe {c}"),
        Action::JoinPresence {
            channel, member, ..
        } => format!("= presence-join {channel} {}", member.user_id()),
        Action::ListPresence(c) => format!("= presence-list {c}"),
        Action::LeavePresence { channel, user_id } => {
            format!("= presence-leave {channel} {user_id}")
        }
        Action::Whisper {
            channel,
            event,
            data,
            user_id,
        } => format!(
            "= whisper {channel} {event} {data} {}",
            user_id.as_deref().unwrap_or("-")
        ),
    }
}

fn expect(name: &str, line_no: usize, line: &str, pending: &mut VecDeque<Action>) {
    let Some(action) = pending.pop_front() else {
        panic!("{name}:{line_no}: expected `{line}`, the server sent nothing more");
    };
    let ok = match (line.split_at(1), &action) {
        (("<", rest), Action::Send(text)) => {
            let expected: Value = serde_json::from_str(rest.trim())
                .unwrap_or_else(|e| panic!("{name}:{line_no}: bad expected frame: {e}"));
            let actual: Value = serde_json::from_str(text).expect("server frames are JSON");
            normalize(expected) == normalize(actual)
        }
        (("x", rest), Action::Close(code, _)) => rest.trim() == code.code().to_string(),
        (("=", rest), Action::Subscribe(c, _)) => rest.trim() == format!("subscribe {c}"),
        (("=", rest), Action::Unsubscribe(c)) => rest.trim() == format!("unsubscribe {c}"),
        (("=", _), other) => line == describe(other),
        _ => false,
    };
    assert!(
        ok,
        "{name}:{line_no}: expected `{line}`, got `{}`",
        describe(&action)
    );
}

fn replay(name: &str, text: &str) {
    let mut settings = Vec::new();
    let mut session: Option<Session> = None;
    let start = Instant::now();
    let mut elapsed = Duration::ZERO;
    let mut pending: VecDeque<Action> = VecDeque::new();
    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(setting) = line.strip_prefix("set ") {
            assert!(session.is_none(), "{name}:{line_no}: `set` comes first");
            let (key, value) = setting.split_once('=').expect("set name=value");
            settings.push((key.to_owned(), value.parse().expect("a number")));
            continue;
        }
        if line.starts_with(['<', 'x', '=']) {
            expect(name, line_no, line, &mut pending);
            continue;
        }
        // An input: everything the server said before must have been listed.
        if let Some(extra) = pending.front() {
            panic!(
                "{name}:{line_no}: the server also sent `{}`",
                describe(extra)
            );
        }
        let session = session
            .get_or_insert_with(|| Session::new(Arc::new(config(&settings)), SOCKET.into(), start));
        let now = Now {
            at: start + elapsed,
            unix: START + elapsed.as_secs(),
        };
        let actions = if line == "open" {
            session.open()
        } else if line == "binary" {
            session.on_binary()
        } else if line == "control" {
            session.on_control(now.at)
        } else if line == "shutdown" {
            session.on_shutdown()
        } else if let Some(step) = line.strip_prefix("+ ") {
            elapsed += if let Some(ms) = step.strip_suffix("ms") {
                Duration::from_millis(ms.parse().expect("ms"))
            } else {
                Duration::from_secs(step.trim_end_matches('s').parse().expect("seconds"))
            };
            session.on_tick(start + elapsed)
        } else if let Some(frame) = line.strip_prefix("> ") {
            let (frame, times) = match frame.rsplit_once(" *") {
                Some((frame, n)) if n.parse::<usize>().is_ok() => {
                    (frame, n.parse::<usize>().unwrap_or(1))
                }
                _ => (frame, 1),
            };
            let frame = fill(frame, now.unix);
            (0..times)
                .flat_map(|_| session.on_text(&frame, now))
                .collect()
        } else {
            panic!("{name}:{line_no}: unknown line `{line}`");
        };
        pending.extend(actions);
    }
    if let Some(extra) = pending.front() {
        panic!(
            "{name}: the server also sent `{}` at the end",
            describe(extra)
        );
    }
}

#[test]
fn golden_transcripts() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/golden")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    files.sort();
    assert!(files.len() >= 12, "the transcripts are there: {files:?}");
    for file in files {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&file).expect("read the transcript");
        replay(&name, &text);
    }
}

#[test]
fn the_next_deadline_follows_silence_pings_and_age() {
    let start = Instant::now();
    let mut session = Session::new(Arc::new(config(&[])), SOCKET.into(), start);
    assert_eq!(session.next_deadline(), start + Duration::from_secs(60));
    let actions = session.on_tick(start + Duration::from_secs(60));
    assert_eq!(actions.len(), 1);
    assert_eq!(session.next_deadline(), start + Duration::from_secs(90));
}

#[test]
fn huge_timer_values_never_overflow() {
    let start = Instant::now();
    let mut config = config(&[]);
    config.max_age = Duration::MAX;
    config.ping_interval = Duration::MAX;
    let mut session = Session::new(Arc::new(config), SOCKET.into(), start);
    assert!(session.next_deadline() > start);
    assert!(session.on_tick(start + Duration::from_secs(10)).is_empty());
}
