//! Test helpers for Alloy pages, on top of `smeltery::testing::TestApp`.
//!
//! ```
//! use smeltery::alloy::testing::{AlloyAssertions as _, AlloyRequests as _};
//! use smeltery::alloy::{self, Alloy, AlloyExt as _, Page};
//! use smeltery::testing::TestApp;
//!
//! #[derive(smeltery::Mold, Default)]
//! #[mold("app")]
//! struct Root {}
//!
//! async fn dashboard() -> Page {
//!     alloy::render("dashboard")
//!         .with("greeting", "Hello")
//!         .defer("activity", || async { Ok(["Signed in"]) })
//! }
//!
//! let app = TestApp::new(|app| {
//!     app.alloy(Alloy::new().root::<Root>())
//!         .routes(|r| { r.get("/dashboard", dashboard); })
//! });
//! let page = app.get_alloy("/dashboard");
//! page.assert_component("dashboard")
//!     .assert_prop("greeting", "Hello")
//!     .assert_missing("activity")
//!     .assert_deferred("default", &["activity"]);
//! let partial = app.reload_alloy("/dashboard", "dashboard", &["activity"]);
//! partial.assert_prop("activity", ["Signed in"]);
//! // A first visit: the page object inside the HTML.
//! app.get("/dashboard").assert_component("dashboard");
//! ```

use axum::body::Body;
use http::{HeaderMap, HeaderValue, Method};
use serde::Serialize;
use serde_json::Value;
use smeltery_core::testing::{TestApp, TestResponse};

/// Inertia visits from a test: `X-Inertia` and the app's current asset version.
pub trait AlloyRequests {
    /// An Inertia visit to `path` (`GET`), answered with the page object as JSON.
    fn get_alloy(&self, path: &str) -> TestResponse;

    /// A partial reload of `component` at `path` asking only for the props `only`
    /// (`router.reload({ only })`).
    fn reload_alloy(&self, path: &str, component: &str, only: &[&str]) -> TestResponse;

    /// A form post the way Inertia's client sends one (`form.post(path)`): `X-Inertia`, the JSON body `data` and
    /// `Accept: text/html, application/xhtml+xml`. A failed validation answers a redirect back with the errors in
    /// the session, never 422 JSON.
    fn post_alloy(&self, path: &str, data: &Value) -> TestResponse;
}

fn inertia_headers(app: &TestApp) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-inertia", HeaderValue::from_static("true"));
    headers.insert(
        "x-requested-with",
        HeaderValue::from_static("XMLHttpRequest"),
    );
    let version = crate::version(app.app()).unwrap_or_default();
    if let Ok(v) = HeaderValue::from_str(&version) {
        headers.insert("x-inertia-version", v);
    }
    headers
}

impl AlloyRequests for TestApp {
    fn get_alloy(&self, path: &str) -> TestResponse {
        self.request(Method::GET, path, inertia_headers(self), Body::empty())
    }

    fn reload_alloy(&self, path: &str, component: &str, only: &[&str]) -> TestResponse {
        let mut headers = inertia_headers(self);
        if let Ok(v) = HeaderValue::from_str(component) {
            headers.insert("x-inertia-partial-component", v);
        }
        if let Ok(v) = HeaderValue::from_str(&only.join(",")) {
            headers.insert("x-inertia-partial-data", v);
        }
        self.request(Method::GET, path, headers, Body::empty())
    }

    fn post_alloy(&self, path: &str, data: &Value) -> TestResponse {
        let mut headers = inertia_headers(self);
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            http::header::ACCEPT,
            HeaderValue::from_static("text/html, application/xhtml+xml"),
        );
        self.request(Method::POST, path, headers, Body::from(data.to_string()))
    }
}

/// Reading and checking the Alloy page of a response: the JSON of an Inertia visit, or the page object embedded in
/// the HTML of a first visit.
pub trait AlloyAssertions {
    /// The page object.
    ///
    /// # Panics
    /// The response holds no Alloy page.
    fn alloy_page(&self) -> Value;

    /// The prop at the dot path `path` (`auth.user.name`, `posts.0.title`).
    fn prop(&self, path: &str) -> Option<Value>;

    /// Assert the component name.
    ///
    /// # Panics
    /// It differs.
    fn assert_component(&self, component: &str) -> &Self;

    /// Assert the prop at `path` equals `expected`.
    ///
    /// # Panics
    /// It differs or is missing.
    fn assert_prop(&self, path: &str, expected: impl Serialize) -> &Self;

    /// Assert there is no prop at `path`.
    ///
    /// # Panics
    /// There is one.
    fn assert_missing(&self, path: &str) -> &Self;

    /// Assert deferred group `group` lists exactly `keys`.
    ///
    /// # Panics
    /// It differs.
    fn assert_deferred(&self, group: &str, keys: &[&str]) -> &Self;
}

fn lookup(value: &Value, path: &str) -> Option<Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = match current {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current.clone())
}

/// The page object embedded in a first visit's HTML (`<script data-page="…" type="application/json">`).
fn embedded(html: &str) -> Option<Value> {
    let start = html.find("<script data-page=\"")?;
    let rest = html.get(start..)?;
    let open = rest.find('>')? + 1;
    let body = rest.get(open..)?;
    let end = body.find("</script>")?;
    serde_json::from_str(body.get(..end)?).ok()
}

impl AlloyAssertions for TestResponse {
    #[allow(clippy::panic)]
    fn alloy_page(&self) -> Value {
        if self.header("x-inertia") == Some("true") {
            return self.json();
        }
        embedded(&self.text()).unwrap_or_else(|| {
            panic!(
                "no Alloy page in this response (status {}): {}",
                self.status(),
                self.text()
            )
        })
    }

    fn prop(&self, path: &str) -> Option<Value> {
        lookup(&self.alloy_page(), &format!("props.{path}"))
    }

    #[allow(clippy::panic)]
    fn assert_component(&self, component: &str) -> &Self {
        let page = self.alloy_page();
        if page.get("component").and_then(Value::as_str) != Some(component) {
            panic!("expected component `{component}`, the page is {page}");
        }
        self
    }

    #[allow(clippy::panic)]
    fn assert_prop(&self, path: &str, expected: impl Serialize) -> &Self {
        let expected = serde_json::to_value(expected).unwrap_or(Value::Null);
        match self.prop(path) {
            Some(actual) if actual == expected => self,
            Some(actual) => panic!("prop `{path}` is {actual}, expected {expected}"),
            None => panic!("no prop `{path}`; the page is {}", self.alloy_page()),
        }
    }

    #[allow(clippy::panic)]
    fn assert_missing(&self, path: &str) -> &Self {
        if let Some(actual) = self.prop(path) {
            panic!("prop `{path}` is there ({actual}), expected none");
        }
        self
    }

    #[allow(clippy::panic)]
    fn assert_deferred(&self, group: &str, keys: &[&str]) -> &Self {
        let page = self.alloy_page();
        let listed = lookup(&page, &format!("deferredProps.{group}")).unwrap_or(Value::Null);
        if listed != serde_json::json!(keys) {
            panic!("deferred group `{group}` is {listed}, expected {keys:?}");
        }
        self
    }
}

/// Assert the client has a page file for `component`: `resources/js/pages/<component>.{tsx,jsx,vue,svelte,ts,js}`
/// under the app root (`SMELTERY_ROOT`, else the working directory). A missing file is only an error in the
/// browser, so the app's tests check it.
///
/// # Panics
/// No such file.
#[allow(clippy::panic)]
pub fn assert_page_file_exists(component: &str) {
    let pages = smeltery_core::config::root_dir()
        .join("resources")
        .join("js")
        .join("pages");
    let exists = ["tsx", "jsx", "vue", "svelte", "ts", "js"]
        .iter()
        .any(|ext| pages.join(format!("{component}.{ext}")).is_file());
    if !exists {
        panic!(
            "no page file for the Alloy component `{component}` in {}",
            pages.display()
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn lookups_and_the_embedded_page() {
        let v = serde_json::json!({"props": {"posts": [{"title": "A"}], "n": 1}});
        assert_eq!(lookup(&v, "props.posts.0.title"), Some(Value::from("A")));
        assert_eq!(lookup(&v, "props.n.x"), None);
        let html = "<head></head><script data-page=\"app\" type=\"application/json\">\
                    {\"component\":\"x\",\"s\":\"\\u003c\\/script\\u003e\"}</script><div id=\"app\"></div>";
        let page = embedded(html).unwrap();
        assert_eq!(page["component"], "x");
        assert_eq!(page["s"], "</script>");
        assert!(embedded("<p>no page</p>").is_none());
    }
}
