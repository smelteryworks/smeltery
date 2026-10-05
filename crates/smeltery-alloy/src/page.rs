//! The page a handler returns: a component name, its props and per-page options.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use serde::Serialize;
use smeltery_core::Result;

use crate::props::Props;

/// What a page answer says when no Alloy middleware finished it.
const UNFINISHED: &str =
    "Alloy pages need web routes (routes/web.rs) and `.alloy(…)` in bootstrap/app.rs";

/// An Alloy page: the React / Vue component to show and its props. Build it with [`render`] (or a
/// `#[derive(Alloy)]` struct) and return it from a handler of a web route.
///
/// ```
/// use smeltery::alloy::{self, Page};
///
/// async fn dashboard() -> Page {
///     alloy::render("dashboard")
///         .with("greeting", "Welcome back")
///         .defer("activity", || async { Ok(vec!["Signed in"]) })
/// }
/// ```
///
/// The response leaves the handler unfinished: the Alloy middleware answers an Inertia visit with the page object as
/// JSON and a first visit with the root Mold template. A page returned anywhere else (an API route, an app without
/// `.alloy(…)`) answers 500 with a message saying so.
pub struct Page {
    pub(crate) component: String,
    pub(crate) props: Props,
    pub(crate) status: StatusCode,
    pub(crate) encrypt_history: Option<bool>,
    pub(crate) clear_history: bool,
    pub(crate) error: Option<String>,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("component", &self.component)
            .field("props", &self.props.keys().collect::<Vec<_>>())
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// A page showing `component` (the client resolves it to a file under `resources/js/pages/`, such as
/// `posts/index`), with no props yet.
pub fn render(component: impl Into<String>) -> Page {
    Page {
        component: component.into(),
        props: Props::new(),
        status: StatusCode::OK,
        encrypt_history: None,
        clear_history: false,
        error: None,
    }
}

/// A typed page: `#[derive(Serialize, Alloy)] #[alloy("posts/index")]` implements it and makes the struct a response.
/// The struct must serialize to a JSON object; its fields become the props.
///
/// ```
/// use smeltery::alloy::Component as _;
///
/// #[derive(serde::Serialize, smeltery::Alloy)]
/// #[alloy("posts/index")]
/// struct PostsIndex {
///     titles: Vec<String>,
/// }
///
/// async fn index() -> smeltery::alloy::Page {
///     PostsIndex { titles: vec!["Hello".into()] }
///         .into_page()
///         .defer("stats", || async { Ok(3) })
/// }
/// ```
pub trait Component: Serialize + Sized {
    /// The component name, such as `posts/index`.
    const NAME: &'static str;

    /// The page builder with this struct's fields as props, to add lazy props.
    fn into_page(self) -> Page {
        let mut page = render(Self::NAME);
        match serde_json::to_value(&self) {
            Ok(serde_json::Value::Object(fields)) => {
                for (key, value) in fields {
                    page.props = page.props.with(key, value);
                }
            }
            Ok(_) => {
                page.error = Some(format!(
                    "the Alloy component `{}` must serialize to a JSON object (a struct with named fields)",
                    Self::NAME
                ));
            }
            Err(e) => {
                page.error = Some(format!(
                    "the props of `{}` cannot be serialized: {e}",
                    Self::NAME
                ));
            }
        }
        page
    }
}

impl Page {
    /// The component name.
    pub fn component(&self) -> &str {
        &self.component
    }

    /// See [`Props::with`].
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.props = self.props.with(key, value);
        self
    }

    /// See [`Props::with_lazy`].
    #[must_use]
    pub fn with_lazy<F, Fut, T>(mut self, key: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize,
    {
        self.props = self.props.with_lazy(key, f);
        self
    }

    /// See [`Props::optional`].
    #[must_use]
    pub fn optional<F, Fut, T>(mut self, key: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize,
    {
        self.props = self.props.optional(key, f);
        self
    }

    /// See [`Props::defer`].
    #[must_use]
    pub fn defer<F, Fut, T>(mut self, key: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize,
    {
        self.props = self.props.defer(key, f);
        self
    }

    /// See [`Props::defer_in`].
    #[must_use]
    pub fn defer_in<F, Fut, T>(
        mut self,
        group: impl Into<String>,
        key: impl Into<String>,
        f: F,
    ) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize,
    {
        self.props = self.props.defer_in(group, key, f);
        self
    }

    /// See [`Props::merge`].
    #[must_use]
    pub fn merge(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.props = self.props.merge(key, value);
        self
    }

    /// See [`Props::prepend`].
    #[must_use]
    pub fn prepend(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.props = self.props.prepend(key, value);
        self
    }

    /// See [`Props::deep_merge`].
    #[must_use]
    pub fn deep_merge(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.props = self.props.deep_merge(key, value);
        self
    }

    /// See [`Props::match_on`].
    #[must_use]
    pub fn match_on(mut self, key: &str, field: impl Into<String>) -> Self {
        self.props = self.props.match_on(key, field);
        self
    }

    /// See [`Props::always`].
    #[must_use]
    pub fn always(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.props = self.props.always(key, value);
        self
    }

    /// The status of the HTML page and of the JSON answer (200 by default), e.g. a 404 page.
    #[must_use]
    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    /// Whether the client encrypts this page's history state (`encryptHistory`), overriding
    /// [`crate::Alloy::encrypt_history`]. Needs a secure context: HTTPS, or `localhost` / `127.0.0.1`.
    #[must_use]
    pub fn encrypt_history(mut self, encrypt: bool) -> Self {
        self.encrypt_history = Some(encrypt);
        self
    }

    /// Clear the client's history state with this page (`clearHistory`), so the back button cannot show earlier
    /// pages' props. See also [`crate::clear_history`].
    #[must_use]
    pub fn clear_history(mut self) -> Self {
        self.clear_history = true;
        self
    }
}

/// The page in the response extensions, until the Alloy middleware takes it (extensions must be `Clone + Sync`,
/// hence the shared slot).
#[derive(Clone)]
pub(crate) struct PageSlot(Arc<Mutex<Option<Page>>>);

impl PageSlot {
    pub(crate) fn take(&self) -> Option<Page> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// Logs a page nobody finished: its answer is the 500 of `into_response`, and the log says why.
impl Drop for PageSlot {
    fn drop(&mut self) {
        // Only the last handle sees the page still in the slot.
        if Arc::strong_count(&self.0) == 1
            && let Some(page) = self.take()
        {
            tracing::error!(component = %page.component, "{UNFINISHED}");
        }
    }
}

impl IntoResponse for Page {
    fn into_response(self) -> Response {
        // The Alloy middleware replaces this answer; when there is none, the 500 says what is missing.
        let mut response = (StatusCode::INTERNAL_SERVER_ERROR, UNFINISHED).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        response
            .extensions_mut()
            .insert(PageSlot(Arc::new(Mutex::new(Some(self)))));
        response
    }
}
