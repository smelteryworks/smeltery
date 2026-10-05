//! Views: Mold templates returned from handlers.
//!
//! A handler returns a template struct (`#[derive(Mold)]` makes it a response) or calls [`view`]. The response
//! carries the template unrendered; the view middleware renders it with the request's data ([`ViewData`]): the
//! CSRF token for `@csrf`, the signed-in state for `@auth`, validation errors for `@error`, old input for `old()`,
//! and the app's routes for `route()`. In debug builds the template is read from `<root>/resources/views` (edits
//! show on the next request); in release builds the code compiled from it runs.
//!
//! ```
//! use smeltery_core::{Response, view::view};
//! # struct Home;
//! # impl smeltery_mold::Template for Home {
//! #     const NAME: &'static str = "home";
//! #     fn render_runtime(&self, _: &dyn smeltery_mold::Host) -> Result<String, smeltery_mold::Error> { Ok("hi".into()) }
//! #     fn render_compiled(&self, _: &dyn smeltery_mold::Host) -> Result<String, smeltery_mold::Error> { Ok("hi".into()) }
//! # }
//!
//! async fn home() -> Response {
//!     view(Home)
//! }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Body;
use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use smeltery_mold::{Engine, Host, Template, Value};

use crate::app::App;
use crate::auth::Auth;
use crate::error::Error;
use crate::middleware::Next;
use crate::session::Session;

/// A response that renders template `t` (status 200) when it leaves the app.
pub fn view<T: Template + Send + 'static>(t: T) -> Response {
    view_with_status(StatusCode::OK, t)
}

/// A response that renders template `t` with `status`.
pub fn view_with_status<T: Template + Send + 'static>(status: StatusCode, t: T) -> Response {
    let mut response = status.into_response();
    let view: Box<dyn ErasedView> = Box::new(t);
    response
        .extensions_mut()
        .insert(Deferred(Arc::new(Mutex::new(Some(view)))));
    response
}

/// The per-request data templates read: `@csrf`, `@auth`/`@guest`, `@error`, `old()` and `session()`.
///
/// Empty by default. Middleware that knows these values (sessions, validation) inserts a `ViewData` into the
/// request extensions, or into the response extensions when it learns them after the handler ran; the response's
/// copy wins.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ViewData {
    /// The session's CSRF token (`@csrf`, `csrf_token()`).
    pub csrf_token: Option<String>,
    /// Whether a user is signed in (`@auth`, `@guest`).
    pub authenticated: bool,
    /// Validation errors by field (`@error("field")`).
    pub errors: HashMap<String, Vec<String>>,
    /// Previously submitted input by field (`old("field")`).
    pub old: HashMap<String, String>,
    /// Session values as text, flash values included (`session("status")`).
    pub session: HashMap<String, String>,
    /// The request's session and signed-in user, on web routes (what `@spark` components mount with).
    web: Option<(Session, Auth)>,
}

impl ViewData {
    /// The request's session, on web routes. When a view renders, the session has already been saved, so
    /// changes made through this handle are not kept.
    pub fn session_handle(&self) -> Option<&Session> {
        self.web.as_ref().map(|(session, _)| session)
    }

    /// The request's [`Auth`], on web routes.
    pub fn auth(&self) -> Option<&Auth> {
        self.web.as_ref().map(|(_, auth)| auth)
    }

    pub(crate) fn set_web(&mut self, session: Session, auth: Auth) {
        self.web = Some((session, auth));
    }
}

/// Renders `@spark` components and `@sparksScripts` for the views of an app: installed as the service
/// `Arc<dyn SparkRenderer>` by the Sparks crate (`smeltery::sparks`). Without it, `@spark` is a template error
/// and `@sparksScripts` renders nothing.
pub trait SparkRenderer: Send + Sync + 'static {
    /// The HTML of component `name` mounted with `props`, rendered for the request behind `host`.
    ///
    /// # Errors
    /// A message for the template error page (an unknown component, a failing mount hook).
    fn render(&self, host: &RequestHost, name: &str, props: &Value) -> Result<String, String>;

    /// The tags that load the client runtime (`@sparksScripts`).
    fn scripts(&self, host: &RequestHost) -> String;
}

/// Renders `@alloy`, `@alloyHead` and `@vite` for the views of an app: installed as the service
/// `Arc<dyn AlloyRenderer>` by the Alloy crate (`smeltery::alloy`). Without it, `@alloy` and
/// `@vite` are template errors and `@alloyHead` renders nothing.
///
/// What a renderer returns is written into the page unescaped: it escapes whatever it embeds
/// (the page JSON, entry names).
pub trait AlloyRenderer: Send + Sync + 'static {
    /// The page element with root id `id` (`@alloy`, `@alloy("id")`), from the request's
    /// [`PagePayload`] ([`RequestHost::page_payload`]).
    ///
    /// # Errors
    /// A message for the template error page (no page in this response, for instance).
    fn page(&self, host: &RequestHost, id: &str) -> Result<String, String>;

    /// The head tags (`@alloyHead`). Empty by default.
    fn head(&self, host: &RequestHost) -> String {
        let _ = host;
        String::new()
    }

    /// The asset tags for `entries` (`@vite("a", …)`), or for the configured entries when
    /// `entries` is empty (`@vite`). The entries are the template's string literals as written.
    ///
    /// # Errors
    /// A message for the template error page (an entry missing from the manifest, no assets).
    fn vite(&self, host: &RequestHost, entries: &[&str]) -> Result<String, String>;
}

/// The page object of an Alloy (Inertia) response, serialized and escaped for the HTML page,
/// on its way from the Alloy middleware to the root template's `@alloy`: put it into the
/// response extensions next to the root view.
///
/// The only way to make one is [`PagePayload::new`], which escapes the JSON itself, so no prop
/// can close the `<script>` element the page sits in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagePayload(Arc<str>);

impl PagePayload {
    /// The page object `page`, serialized and escaped for a `<script type="application/json">`
    /// element: `<`, `>`, `&`, `/`, U+2028 and U+2029 become JSON escapes. They only occur
    /// inside JSON strings, so the parsed value stays the same.
    pub fn new(page: &serde_json::Value) -> Self {
        // A `Value` always serializes (its map keys are strings).
        let json = serde_json::to_string(page).unwrap_or_default();
        let mut out = String::with_capacity(json.len() + 16);
        for c in json.chars() {
            match c {
                '<' => out.push_str("\\u003c"),
                '>' => out.push_str("\\u003e"),
                '&' => out.push_str("\\u0026"),
                '/' => out.push_str("\\/"),
                '\u{2028}' => out.push_str("\\u2028"),
                '\u{2029}' => out.push_str("\\u2029"),
                c => out.push(c),
            }
        }
        Self(out.into())
    }

    /// The escaped page JSON.
    pub fn json(&self) -> &str {
        &self.0
    }
}

/// Whether `response` carries a view that is not rendered yet (from [`view`] or a
/// `#[derive(Mold)]` struct).
pub fn is_view(response: &Response) -> bool {
    response.extensions().get::<Deferred>().is_some()
}

/// The error of `@alloy` and `@vite` when the app has no [`AlloyRenderer`].
const ALLOY_NOT_ENABLED: &str = "Alloy is not enabled: call `.alloy(…)` in bootstrap/app.rs";

/// The [`Host`] a view renders with: the request's [`ViewData`] and the app's routes.
#[derive(Clone, Debug)]
pub struct RequestHost {
    app: App,
    data: ViewData,
    page: Option<PagePayload>,
}

impl RequestHost {
    /// A host for `app` with `data`.
    pub fn new(app: App, data: ViewData) -> Self {
        Self {
            app,
            data,
            page: None,
        }
    }

    /// The same host carrying `page` (what `@alloy` renders).
    #[must_use]
    pub fn with_page_payload(mut self, page: PagePayload) -> Self {
        self.page = Some(page);
        self
    }

    /// The Alloy page of this response, when it has one.
    pub fn page_payload(&self) -> Option<&PagePayload> {
        self.page.as_ref()
    }

    /// The app.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The request's view data.
    pub fn data(&self) -> &ViewData {
        &self.data
    }

    fn sparks(&self) -> Option<Arc<Arc<dyn SparkRenderer>>> {
        self.app.service::<Arc<dyn SparkRenderer>>()
    }

    fn alloy(&self) -> Option<Arc<Arc<dyn AlloyRenderer>>> {
        self.app.service::<Arc<dyn AlloyRenderer>>()
    }
}

impl Host for RequestHost {
    fn csrf_token(&self) -> Option<&str> {
        self.data.csrf_token.as_deref()
    }
    fn authenticated(&self) -> bool {
        self.data.authenticated
    }
    fn errors(&self, field: &str) -> &[String] {
        self.data.errors.get(field).map_or(&[], Vec::as_slice)
    }
    fn old(&self, field: &str) -> Option<&str> {
        self.data.old.get(field).map(String::as_str)
    }
    fn session(&self, key: &str) -> Option<String> {
        self.data.session.get(key).cloned()
    }
    fn route(&self, name: &str, params: &[(String, String)]) -> Result<String, String> {
        let params: Vec<(&str, &str)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        self.app.url(name, &params).map_err(|e| e.to_string())
    }
    fn spark(&self, name: &str, props: &Value) -> Result<String, String> {
        match self.sparks() {
            Some(sparks) => sparks.render(self, name, props),
            None => Err("Sparks are not enabled: call `.sparks(…)` in bootstrap/app.rs".to_owned()),
        }
    }
    fn sparks_scripts(&self) -> String {
        self.sparks()
            .map(|sparks| sparks.scripts(self))
            .unwrap_or_default()
    }
    fn alloy_page(&self, id: &str) -> Result<String, String> {
        match self.alloy() {
            Some(alloy) => alloy.page(self, id),
            None => Err(ALLOY_NOT_ENABLED.to_owned()),
        }
    }
    fn alloy_head(&self) -> String {
        self.alloy()
            .map(|alloy| alloy.head(self))
            .unwrap_or_default()
    }
    fn vite(&self, entries: &[&str]) -> Result<String, String> {
        match self.alloy() {
            Some(alloy) => alloy.vite(self, entries),
            None => Err(ALLOY_NOT_ENABLED.to_owned()),
        }
    }
}

/// A template with its type erased.
trait ErasedView: Send {
    fn render(&self, engine: &Engine, host: &dyn Host) -> Result<String, smeltery_mold::Error>;
}

impl<T: Template + Send> ErasedView for T {
    fn render(&self, engine: &Engine, host: &dyn Host) -> Result<String, smeltery_mold::Error> {
        if cfg!(debug_assertions) {
            self.render_runtime_with(engine, host)
        } else {
            self.render_compiled(host)
        }
    }
}

/// The unrendered view in the response extensions (extensions must be `Clone + Sync`, hence the shared slot).
#[derive(Clone)]
struct Deferred(Arc<Mutex<Option<Box<dyn ErasedView>>>>);

/// Renders deferred views. Sits inside the error pages and outside the global middleware.
pub(crate) async fn render_views(app: App, req: Request, next: Next) -> Response {
    let request_data = req.extensions().get::<ViewData>().cloned();
    let mut response = next.run(req).await;
    let Some(Deferred(slot)) = response.extensions_mut().remove::<Deferred>() else {
        return response;
    };
    let Some(view) = slot.lock().unwrap_or_else(PoisonError::into_inner).take() else {
        return response;
    };
    let data = response
        .extensions_mut()
        .remove::<ViewData>()
        .or(request_data)
        .unwrap_or_default();
    let debug = app.settings().debug;
    let mut host = RequestHost::new(app.clone(), data);
    if let Some(page) = response.extensions_mut().remove::<PagePayload>() {
        host = host.with_page_payload(page);
    }
    // Rendering reads template files in debug builds and is CPU work: keep it off the async workers.
    let rendered = tokio::task::spawn_blocking(move || view.render(app.views(), &host)).await;
    match rendered {
        Ok(Ok(html)) => {
            *response.body_mut() = Body::from(html);
            let headers = response.headers_mut();
            headers.remove(header::CONTENT_LENGTH);
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            response
        }
        Ok(Err(e)) => {
            if debug {
                tracing::error!(error = %e, "template error");
                let mut page = (StatusCode::INTERNAL_SERVER_ERROR, e.to_html()).into_response();
                page.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                );
                page
            } else {
                Error::internal(format!("template error: {e}")).into_response()
            }
        }
        Err(join) => {
            tracing::error!(error = %join, "rendering a view panicked");
            Error::internal("rendering a view panicked").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_payload_escapes_what_could_close_its_script_element() {
        let page = serde_json::json!({
            "component": "home",
            "props": {
                "a": "</script><script>alert(1)</script>",
                "b": "<!-- & -->",
                "c": "line\u{2028}para\u{2029}end",
                "d": "a/b",
            },
        });
        let payload = PagePayload::new(&page);
        let json = payload.json();
        for bad in ["<", ">", "&", "\u{2028}", "\u{2029}", "</"] {
            assert!(!json.contains(bad), "{bad:?} in {json}");
        }
        assert!(json.contains(r"\u003c\/script\u003e"), "{json}");
        assert!(json.contains(r"\u003c!--"), "{json}");
        assert!(
            json.contains(r"\u2028") && json.contains(r"\u2029"),
            "{json}"
        );
        let back: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
        assert_eq!(back, page, "the same value after parsing");
    }
}
