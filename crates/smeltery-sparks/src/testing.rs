//! Test helpers: drive a component on a page rendered by [`TestApp`], like the browser would.
//!
//! ```no_run
//! # mod smeltery { pub use smeltery_sparks as sparks; }
//! # use serde_json::json;
//! # let app = smeltery_core::testing::TestApp::new(|b| b);
//! use smeltery::sparks::testing::TestSpark;
//!
//! let page = app.get("/counter");
//! let mut counter = TestSpark::from_html(&page.text(), "counter").expect("the page shows the counter");
//! let res = counter.call("increment", json!([])).send(&app);
//! assert_eq!(res.status(), 200);
//! assert!(counter.html().contains("Count: 1"));
//! assert_eq!(counter.data()["count"], 1);
//! ```

use axum::body::Body;
use http::{HeaderMap, HeaderValue, Method, header};
use serde::Serialize;
use smeltery_core::testing::{TestApp, TestResponse};

/// One component instance on a test page: its snapshot, its HTML, and the updates and calls to send next.
#[derive(Debug, Clone)]
pub struct TestSpark {
    snapshot: String,
    html: String,
    csrf: Option<String>,
    updates: serde_json::Map<String, serde_json::Value>,
    calls: Vec<serde_json::Value>,
    effects: serde_json::Value,
}

/// Undo the HTML attribute escaping of [`smeltery_core::html::escape`].
fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The value of attribute `name` in the tag starting at `tag`.
fn attr(tag: &str, name: &str) -> Option<String> {
    let end = tag.find('>')?;
    let tag = tag.get(..end)?;
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = tag.get(start..)?;
    let stop = rest.find('"')?;
    Some(unescape(rest.get(..stop)?))
}

impl TestSpark {
    /// The first instance of component `name` in `html` (a page from `TestApp::get`), or `None`.
    pub fn from_html(html: &str, name: &str) -> Option<Self> {
        let marker = format!("wire:name=\"{name}\"");
        let at = html.find(&marker)?;
        let start = html.get(..at)?.rfind("<div ")?;
        let tag = html.get(start..)?;
        let snapshot = attr(tag, "wire:snapshot")?;
        let csrf = html
            .find("<meta name=\"csrf-token\" content=\"")
            .and_then(|i| {
                let rest = html.get(i + "<meta name=\"csrf-token\" content=\"".len()..)?;
                rest.find('"').and_then(|e| rest.get(..e)).map(unescape)
            });
        Some(Self {
            snapshot,
            html: tag.to_owned(),
            csrf,
            updates: serde_json::Map::new(),
            calls: Vec::new(),
            effects: serde_json::Value::Null,
        })
    }

    /// Queue a `wire:model` update of `field`.
    pub fn set(&mut self, field: &str, value: impl Serialize) -> &mut Self {
        let value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        self.updates.insert(field.to_owned(), value);
        self
    }

    /// Queue a call of `method` with `params` (a JSON array, e.g. `json!([3])`).
    pub fn call(&mut self, method: &str, params: serde_json::Value) -> &mut Self {
        let params = match params {
            serde_json::Value::Array(p) => p,
            serde_json::Value::Null => Vec::new(),
            one => vec![one],
        };
        self.calls
            .push(serde_json::json!({ "method": method, "params": params }));
        self
    }

    /// The request body of the queued updates and calls.
    pub fn request_body(&self) -> serde_json::Value {
        serde_json::json!({
            "v": crate::PROTOCOL_VERSION,
            "components": [{
                "snapshot": self.snapshot,
                "updates": self.updates,
                "calls": self.calls,
            }],
        })
    }

    /// Send the queued updates and calls to `POST /_sparks/update`. On success the snapshot, HTML and effects
    /// are the response's; the queue is cleared either way.
    pub fn send(&mut self, app: &TestApp) -> TestResponse {
        let body = self.request_body();
        self.updates.clear();
        self.calls.clear();
        let res = post_update(app, &body, self.csrf.as_deref());
        if res.status() == 200 {
            let json = res.json();
            let first = json
                .get("components")
                .and_then(|c| c.get(0))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if let Some(s) = first.get("snapshot").and_then(serde_json::Value::as_str) {
                self.snapshot = s.to_owned();
            }
            if let Some(h) = first.get("html").and_then(serde_json::Value::as_str) {
                self.html = h.to_owned();
            }
            self.effects = first
                .get("effects")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
        }
        res
    }

    /// The snapshot text (what `wire:snapshot` holds).
    pub fn snapshot(&self) -> &str {
        &self.snapshot
    }

    /// Replace the snapshot text (e.g. to test that a tampered one is refused).
    pub fn set_snapshot(&mut self, snapshot: impl Into<String>) {
        self.snapshot = snapshot.into();
    }

    /// The component's state from the snapshot.
    pub fn data(&self) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(&self.snapshot)
            .ok()
            .and_then(|v| v.get("data").cloned())
            .unwrap_or(serde_json::Value::Null)
    }

    /// The instance id.
    pub fn id(&self) -> String {
        serde_json::from_str::<serde_json::Value>(&self.snapshot)
            .ok()
            .and_then(|v| {
                v.pointer("/memo/id")
                    .and_then(|i| i.as_str().map(str::to_owned))
            })
            .unwrap_or_default()
    }

    /// The HTML: the page from the instance's root on at first, then the latest re-render.
    pub fn html(&self) -> &str {
        &self.html
    }

    /// The effects of the latest update (`{"redirect": …, "dispatches": […]}`).
    pub fn effects(&self) -> &serde_json::Value {
        &self.effects
    }

    /// Upload `bytes` as file `name` for upload field `field`; returns the response (`{"token": "…"}`).
    pub fn upload(
        &self,
        app: &TestApp,
        field: &str,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> TestResponse {
        let component = serde_json::from_str::<serde_json::Value>(&self.snapshot)
            .ok()
            .and_then(|v| {
                v.pointer("/memo/name")
                    .and_then(|n| n.as_str().map(str::to_owned))
            })
            .unwrap_or_default();
        let query =
            serde_urlencoded_pairs(&[("component", &component), ("field", field), ("name", name)]);
        let mut headers = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(mime) {
            headers.insert(header::CONTENT_TYPE, v);
        }
        if let Some(token) = self
            .csrf
            .as_deref()
            .and_then(|t| HeaderValue::from_str(t).ok())
        {
            headers.insert("x-csrf-token", token);
        }
        app.request(
            Method::POST,
            &format!("/_sparks/upload?{query}"),
            headers,
            Body::from(bytes),
        )
    }
}

/// A stream token for instance `id` of component `name`, as a render of a `#[spark(stream)]` component on a page
/// without a session writes it into `wire:stream` (for tests that open `GET /_sparks/stream?t=<token>` without a
/// page).
///
/// # Errors
/// The app cannot sign (no `APP_KEY`).
pub fn stream_token(
    app: &smeltery_core::App,
    name: &str,
    id: &str,
) -> smeltery_core::Result<String> {
    crate::broadcast::stream_token(
        app,
        name,
        id,
        std::time::Duration::from_secs(3600),
        crate::broadcast::Viewer::default(),
        Vec::new(),
    )
}

/// `POST /_sparks/update` with `body` (and the CSRF header when `csrf` is given).
pub fn post_update(app: &TestApp, body: &serde_json::Value, csrf: Option<&str>) -> TestResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    if let Some(token) = csrf.and_then(|t| HeaderValue::from_str(t).ok()) {
        headers.insert("x-csrf-token", token);
    }
    app.request(
        Method::POST,
        "/_sparks/update",
        headers,
        Body::from(body.to_string()),
    )
}

fn serde_urlencoded_pairs(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", percent(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_instance_and_unescapes() {
        let html = "<head><meta name=\"csrf-token\" content=\"tok\"></head><div wire:id=\"a1\" wire:name=\"counter\" wire:snapshot=\"{&quot;v&quot;:1,&quot;memo&quot;:{&quot;id&quot;:&quot;a1&quot;},&quot;data&quot;:{&quot;n&quot;:&quot;&lt;&amp;&#39;&quot;}}\"><p>x</p></div>";
        let s = TestSpark::from_html(html, "counter").unwrap();
        assert_eq!(s.data()["n"], "<&'");
        assert_eq!(s.id(), "a1");
        assert_eq!(s.csrf.as_deref(), Some("tok"));
        assert!(TestSpark::from_html(html, "other").is_none());
        assert_eq!(percent("a b/é"), "a%20b%2F%C3%A9");
    }
}

/// Records what a [`Broadcast`](crate::Broadcast) pushes, for testing jobs, agents and handlers that push to
/// live components. It listens like an open page, so it counts in [`Broadcast::streams`](crate::Broadcast::streams)
/// and in what `refresh()` / `emit()` return.
///
/// ```
/// use smeltery_sparks::Broadcast;
/// use smeltery_sparks::testing::BroadcastSpy;
///
/// let broadcast = Broadcast::new();
/// let mut spy = BroadcastSpy::new(&broadcast);
/// broadcast.to("live_counter").refresh();
/// let pushes = spy.pushes();
/// assert_eq!(pushes.len(), 1);
/// assert!(pushes[0].is_refresh_of("live_counter"));
/// assert!(spy.pushes().is_empty(), "pushes() drains");
///
/// broadcast.to("feed").emit("tick", serde_json::json!({ "n": 3 }));
/// let pushed = spy.pushes().remove(0);
/// assert_eq!((pushed.kind.as_str(), pushed.event.as_deref()), ("event", Some("tick")));
/// assert_eq!(pushed.payload, Some(serde_json::json!({ "n": 3 })));
/// ```
#[derive(Debug)]
pub struct BroadcastSpy {
    rx: tokio::sync::broadcast::Receiver<std::sync::Arc<crate::broadcast::Message>>,
}

/// One message a [`BroadcastSpy`] saw.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Pushed {
    /// The component name or instance id it was addressed to.
    pub target: String,
    /// `refresh` or `event`.
    pub kind: String,
    /// The browser event's name (`event` only).
    pub event: Option<String>,
    /// The browser event's payload (`event` only).
    pub payload: Option<serde_json::Value>,
}

impl Pushed {
    /// Whether this is a refresh of `target`.
    pub fn is_refresh_of(&self, target: &str) -> bool {
        self.kind == "refresh" && self.target == target
    }
}

impl BroadcastSpy {
    /// Start listening on `broadcast`; only later pushes are seen.
    pub fn new(broadcast: &crate::Broadcast) -> Self {
        Self {
            rx: broadcast.subscribe(),
        }
    }

    /// Listen on the app's broadcast (`None` when the app has no Sparks).
    pub fn of(app: &smeltery_core::App) -> Option<Self> {
        crate::Broadcast::of(app).map(|b| Self::new(&b))
    }

    /// Every message pushed since the spy was made or `pushes` was last called, oldest first.
    pub fn pushes(&mut self) -> Vec<Pushed> {
        use tokio::sync::broadcast::error::TryRecvError;
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(m) => out.push(Pushed {
                    target: m.target.clone(),
                    kind: m.kind.to_owned(),
                    event: m.event.clone(),
                    payload: m.payload.clone(),
                }),
                Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        out
    }
}
