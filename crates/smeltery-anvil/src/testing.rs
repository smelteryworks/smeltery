//! Test helpers: record what an app sends ([`AnvilSpy`]), connect a socket without a network ([`TestSocket`]) and
//! ask the auth endpoint for a signature ([`authorize`]).
//!
//! ```
//! use serde_json::json;
//! use smeltery::anvil::testing::{AnvilSpy, TestSocket};
//! use smeltery::anvil::{Anvil, AnvilExt as _, Channel};
//! use smeltery::testing::TestApp;
//!
//! let app = TestApp::new(|b| b.anvil(|c| { c.public("news"); }));
//! let spy = AnvilSpy::of(app.app());
//! let mut socket = TestSocket::connect(app.app());
//! assert_eq!(socket.subscribe("news", None)["event"], "pusher_internal:subscription_succeeded");
//!
//! let anvil = Anvil::of(app.app()).unwrap();
//! app.block_on(async { anvil.to(Channel::public("news")).event("posted").with(&json!({ "id": 1 })).await })?;
//! assert!(spy.sent_on("news", "posted"));
//! assert_eq!(socket.events()[0]["data"], r#"{"id":1}"#);
//! # Ok::<(), smeltery::Error>(())
//! ```

use std::future::Future;
use std::sync::Arc;

use futures_util::FutureExt as _;
use serde_json::Value;
use smeltery_core::App;
use smeltery_core::testing::{TestApp, TestResponse};
use tokio::sync::mpsc;

use crate::hub::Registration;
use crate::session::{Action, Now, Session};
use crate::{Anvil, CloseCode};

/// One event an app sent, as [`AnvilSpy`] recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Sent {
    /// The event name.
    pub name: String,
    /// The channels (full names, `private-orders.7`).
    pub channels: Vec<String>,
    /// The event's data, as JSON text.
    pub data: String,
}

impl Sent {
    pub(crate) fn new(name: &str, channels: &[String], data: &str) -> Self {
        Self {
            name: name.to_owned(),
            channels: channels.to_vec(),
            data: data.to_owned(),
        }
    }

    /// The data, parsed.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.data).unwrap_or(Value::Null)
    }
}

/// Records every event the app sends from the moment it is created (the last 10,000). For tests: an app that
/// creates one keeps recording.
#[derive(Debug, Clone)]
pub struct AnvilSpy {
    anvil: Anvil,
}

impl AnvilSpy {
    /// Start recording the app's sends.
    ///
    /// # Panics
    /// When the app has no Anvil (`.anvil(...)` was not called).
    #[allow(clippy::expect_used)]
    pub fn of(app: &App) -> Self {
        let anvil = Anvil::of(app).expect("the app has no Anvil: call `.anvil(...)`");
        anvil.record();
        Self { anvil }
    }

    /// Everything sent so far, in order.
    pub fn sent(&self) -> Vec<Sent> {
        self.anvil.recorded()
    }

    /// Whether an event named `name` went to `channel`.
    pub fn sent_on(&self, channel: &str, name: &str) -> bool {
        self.sent()
            .iter()
            .any(|s| s.name == name && s.channels.iter().any(|c| c == channel))
    }

    /// Whether nothing was sent.
    pub fn nothing_sent(&self) -> bool {
        self.sent().is_empty()
    }
}

/// A socket connected to the app's hub without a network: it speaks the protocol to the same session code the
/// endpoint runs, and receives the events the app sends in this process.
#[derive(Debug)]
pub struct TestSocket {
    anvil: Anvil,
    session: Session,
    registration: Option<Registration>,
    outbox: mpsc::Receiver<tungstenite::Utf8Bytes>,
    closed: Option<CloseCode>,
}

impl TestSocket {
    /// Connect to the app's hub (as a client without an address). The `pusher:connection_established` frame is
    /// taken: [`socket_id`](Self::socket_id) has the id.
    ///
    /// # Panics
    /// When the app has no Anvil, its secret is unknown (no `APP_KEY` outside `APP_ENV=testing`) or the hub is
    /// full.
    #[allow(clippy::expect_used, clippy::panic)]
    pub fn connect(app: &App) -> Self {
        let anvil = Anvil::of(app).expect("the app has no Anvil: call `.anvil(...)`");
        let config = anvil
            .inner
            .session
            .get()
            .cloned()
            .expect("Anvil has no channel secret (APP_KEY)");
        let socket_id = crate::protocol::new_socket_id().expect("random socket id");
        let registered = match anvil.inner.hub.register(&socket_id, None) {
            Ok(registered) => registered,
            Err(refusal) => panic!("the hub refused the test socket: {refusal:?}"),
        };
        let session = Session::new(Arc::clone(&config), socket_id, tokio::time::Instant::now());
        Self {
            anvil,
            session,
            registration: Some(registered.registration),
            outbox: registered.outbox,
            closed: None,
        }
    }

    /// The socket id the server assigned.
    pub fn socket_id(&self) -> &str {
        self.session.socket_id()
    }

    /// The close code, once the server closed the socket.
    pub fn closed(&self) -> Option<CloseCode> {
        self.closed
    }

    /// Send a text frame; the frames the server answers with, parsed.
    pub fn send(&mut self, text: &str) -> Vec<Value> {
        let actions = self.session.on_text(text, Now::real());
        self.apply(actions)
    }

    /// Subscribe to `channel` (with the `auth` string from the auth endpoint for a private channel); the server's
    /// answer (`pusher_internal:subscription_succeeded` or `pusher:subscription_error`).
    pub fn subscribe(&mut self, channel: &str, auth: Option<&str>) -> Value {
        let mut data = serde_json::json!({ "channel": channel });
        if let (Some(auth), Some(object)) = (auth, data.as_object_mut()) {
            object.insert("auth".into(), Value::String(auth.to_owned()));
        }
        let frame = serde_json::json!({ "event": "pusher:subscribe", "data": data }).to_string();
        self.send(&frame).pop().unwrap_or(Value::Null)
    }

    /// Subscribe to the presence channel `channel` with the `auth` and `channel_data` of the auth endpoint's
    /// answer ([`presence_of`]); the server's answer (with the members on success).
    pub fn subscribe_presence(&mut self, channel: &str, auth: &str, channel_data: &str) -> Value {
        let frame = serde_json::json!({
            "event": "pusher:subscribe",
            "data": { "channel": channel, "auth": auth, "channel_data": channel_data },
        })
        .to_string();
        self.send(&frame).pop().unwrap_or(Value::Null)
    }

    /// Send the client event `event` (`client-…`) with `data` on `channel`; the server's answers (an error, or
    /// nothing when it went to the other subscribers).
    pub fn whisper(&mut self, channel: &str, event: &str, data: &Value) -> Vec<Value> {
        let frame =
            serde_json::json!({ "event": event, "channel": channel, "data": data }).to_string();
        self.send(&frame)
    }

    /// Unsubscribe from `channel`.
    pub fn unsubscribe(&mut self, channel: &str) {
        let frame =
            serde_json::json!({ "event": "pusher:unsubscribe", "data": { "channel": channel } })
                .to_string();
        self.send(&frame);
    }

    /// The events delivered since the last call, parsed.
    pub fn events(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(frame) = self.outbox.try_recv() {
            out.push(serde_json::from_str(frame.as_str()).unwrap_or(Value::Null));
        }
        out
    }

    fn apply(&mut self, actions: Vec<Action>) -> Vec<Value> {
        let mut answers = Vec::new();
        for action in actions {
            match action {
                Action::Send(text) => {
                    answers.push(serde_json::from_str(&text).unwrap_or(Value::Null));
                }
                Action::Close(code, _) => {
                    self.closed = Some(code);
                    self.registration = None;
                }
                Action::Subscribe(channel, holder) => {
                    if let Some(r) = &self.registration
                        && !r.join(&channel, holder)
                    {
                        self.closed = Some(CloseCode::Reconnect);
                        self.registration = None;
                        return answers;
                    }
                }
                Action::Unsubscribe(channel) => {
                    if let Some(r) = &self.registration {
                        r.leave(&channel);
                    }
                }
                Action::JoinPresence {
                    channel,
                    member,
                    holder,
                } => {
                    let Some(r) = &self.registration else {
                        continue;
                    };
                    if !r.join(&channel, holder) {
                        self.closed = Some(CloseCode::Reconnect);
                        self.registration = None;
                        return answers;
                    }
                    let answer = ready(self.anvil.presence_join(
                        self.session.socket_id(),
                        &channel,
                        &member,
                    ));
                    let frame = match answer {
                        Ok(crate::live::JoinAnswer::In(frame)) => frame,
                        Ok(crate::live::JoinAnswer::Full) => {
                            r.leave(&channel);
                            self.session.forget(&channel);
                            crate::protocol::subscription_error(
                                &channel,
                                403,
                                "presence channel full",
                            )
                        }
                        Err(_) => {
                            r.leave(&channel);
                            self.session.forget(&channel);
                            crate::protocol::subscription_error(&channel, 500, "server error")
                        }
                    };
                    answers.push(serde_json::from_str(&frame).unwrap_or(Value::Null));
                }
                Action::ListPresence(channel) => {
                    let frame = ready(self.anvil.presence_list(&channel)).unwrap_or_else(|_| {
                        crate::protocol::subscription_error(&channel, 500, "server error")
                    });
                    answers.push(serde_json::from_str(&frame).unwrap_or(Value::Null));
                }
                Action::LeavePresence { channel, user_id } => {
                    ready(
                        self.anvil
                            .presence_leave(self.session.socket_id(), &channel, &user_id),
                    );
                }
                Action::Whisper {
                    channel,
                    event,
                    data,
                    user_id,
                } => {
                    let sent = self.anvil.whisper(
                        self.session.socket_id(),
                        None,
                        &channel,
                        &event,
                        &data,
                        user_id.as_deref(),
                    );
                    if let Err(message) = sent {
                        answers.push(serde_json::json!({
                            "event": "pusher:error",
                            "data": { "code": 4301, "message": message },
                        }));
                    }
                }
            }
        }
        answers
    }
}

/// The socket closes: it leaves its presence channels (the other members get `member_removed`).
impl Drop for TestSocket {
    fn drop(&mut self) {
        self.registration = None;
        ready(self.anvil.presence_leave_socket(self.session.socket_id()));
    }
}

/// Run a step that completes at once (the in-memory presence store and the `local` PubSub driver of a `TestApp`).
///
/// # Panics
/// When it would wait (a shared presence store): use real sockets for those.
#[allow(clippy::panic)]
fn ready<T>(future: impl Future<Output = T>) -> T {
    match Box::pin(future).now_or_never() {
        Some(value) => value,
        None => panic!(
            "TestSocket supports presence with the in-memory store (PUBSUB_DRIVER=local) only"
        ),
    }
}

/// The `auth` and `channel_data` of a successful presence answer of the auth endpoint.
pub fn presence_of(response: &TestResponse) -> Option<(String, String)> {
    if response.status() != 200 {
        return None;
    }
    let json = response.json();
    Some((
        json.get("auth")?.as_str()?.to_owned(),
        json.get("channel_data")?.as_str()?.to_owned(),
    ))
}

/// `POST /broadcasting/auth` for `socket_id` and `channel` as the [`TestApp`]'s browser (its cookies, its signed-in
/// user). With [`TestApp::with_csrf`], pass the page's CSRF token in `csrf`.
pub fn authorize(
    app: &TestApp,
    socket_id: &str,
    channel: &str,
    csrf: Option<&str>,
) -> TestResponse {
    let mut fields = vec![("socket_id", socket_id), ("channel_name", channel)];
    if let Some(token) = csrf {
        fields.push(("_token", token));
    }
    app.post_form("/broadcasting/auth", &fields)
}

/// The `auth` string of a successful [`authorize`] answer.
pub fn auth_of(response: &TestResponse) -> Option<String> {
    (response.status() == 200)
        .then(|| {
            response
                .json()
                .get("auth")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .flatten()
}
