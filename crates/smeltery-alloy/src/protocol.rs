//! The Inertia protocol: the Alloy web middleware (D-274) that finishes pages, checks the asset version and adapts
//! redirects for Inertia visits.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use serde_json::{Map, Value};
use smeltery_core::auth::Auth;
use smeltery_core::http::{Back, ClientInfo, is_local_path};
use smeltery_core::middleware::{Next, Request};
use smeltery_core::session::Session;
use smeltery_core::view::PagePayload;
use smeltery_core::{App, BoxFuture, Error, Result};

use crate::page::{Page, PageSlot};
use crate::props::{Load, Merge, Prop, Props, Source};
use crate::vite::{Mode, Vite};

/// The session key `clear_history` sets for the next page.
pub(crate) const CLEAR_HISTORY_KEY: &str = "_alloy_clear_history";

pub(crate) type RootFn = Arc<dyn Fn(StatusCode) -> Response + Send + Sync>;
pub(crate) type ShareFn = Arc<dyn Fn(SharedCtx) -> BoxFuture<'static, Result<Props>> + Send + Sync>;
pub(crate) type VersionFn = Arc<dyn Fn(&App) -> String + Send + Sync>;

/// Everything `.alloy(…)` configured, shared by the middleware, the renderer and the boot check.
pub(crate) struct Inner {
    pub(crate) root: RootFn,
    pub(crate) root_name: &'static str,
    pub(crate) share: Option<ShareFn>,
    pub(crate) version: Option<VersionFn>,
    pub(crate) all_errors: bool,
    pub(crate) encrypt_history: bool,
    pub(crate) vite: Vite,
    /// The components `warn_missing_page` has looked up.
    pub(crate) checked: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Alloy")
            .field("root", &self.root_name)
            .field("vite", &self.vite)
            .finish_non_exhaustive()
    }
}

pub(crate) fn mode(app: &App) -> Mode {
    Mode {
        debug: cfg!(debug_assertions),
        testing: app.settings().env == "testing",
        production: app.settings().is_production(),
    }
}

impl Inner {
    /// The current asset version (blocking file access in debug builds: call off the async workers).
    pub(crate) fn version(&self, app: &App) -> String {
        match &self.version {
            Some(f) => f(app),
            None => self.vite.version(&app.settings().root, mode(app)),
        }
    }
}

/// The asset version, for the two places that need it (the Inertia GET / HEAD check and a finished page): straight
/// from the cached manifest in release builds, else off the async workers.
async fn version_off_thread(inner: &Arc<Inner>, app: &App) -> String {
    if inner.version.is_none()
        && let Some(version) = inner.vite.cached_version(mode(app))
    {
        return version;
    }
    let (inner, app) = (inner.clone(), app.clone());
    tokio::task::spawn_blocking(move || inner.version(&app))
        .await
        .unwrap_or_default()
}

/// What shared-props functions see: the request and its session (Alloy runs on web routes only).
#[derive(Clone)]
pub struct SharedCtx {
    app: App,
    session: Session,
    auth: Auth,
    headers: HeaderMap,
    uri: Uri,
    method: Method,
}

impl std::fmt::Debug for SharedCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedCtx")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .finish_non_exhaustive()
    }
}

impl SharedCtx {
    /// The app.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The request's session.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The signed-in user of the request.
    pub fn auth(&self) -> &Auth {
        &self.auth
    }

    /// The request headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The request URI (path and query).
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The request method.
    pub fn method(&self) -> &Method {
        &self.method
    }
}

/// The Inertia headers of a request.
#[derive(Debug, Default)]
pub(crate) struct Facts {
    pub(crate) inertia: bool,
    version: Option<String>,
    partial_component: Option<String>,
    only: Option<Vec<String>>,
    except: Vec<String>,
    reset: Vec<String>,
    error_bag: Option<String>,
    prefetch: bool,
    method: Method,
    /// The request's path and query, relative (never built from `Host`) and a local path (`local_url`): it goes into
    /// `X-Inertia-Location` and the page object's `url`, which the client resolves against the page's origin.
    url: String,
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_owned())
}

/// The most entries a partial-reload header list keeps: matching costs props × entries, so a request cannot make it
/// arbitrarily expensive.
const MAX_LIST: usize = 256;

fn header_list(headers: &HeaderMap, name: &str) -> Option<Vec<String>> {
    let text = header_text(headers, name)?;
    Some(
        text.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .take(MAX_LIST)
            .map(str::to_owned)
            .collect(),
    )
}

/// The request's own path as a target the client resolves on this site (S5-02): leading slashes and backslashes
/// become one `/` (`//evil.example/x` → `/evil.example/x`); anything still not a local path becomes `/`.
pub(crate) fn local_url(url: &str) -> String {
    if is_local_path(url) {
        return url.to_owned();
    }
    let path = format!("/{}", url.trim_start_matches(['/', '\\']));
    if is_local_path(&path) {
        path
    } else {
        "/".to_owned()
    }
}

impl Facts {
    pub(crate) fn read(method: &Method, uri: &Uri, headers: &HeaderMap) -> Self {
        let inertia =
            header_text(headers, "x-inertia").is_some_and(|v| v.eq_ignore_ascii_case("true"));
        Self {
            inertia,
            version: header_text(headers, "x-inertia-version"),
            partial_component: header_text(headers, "x-inertia-partial-component")
                .filter(|s| !s.is_empty()),
            only: header_list(headers, "x-inertia-partial-data").filter(|l| !l.is_empty()),
            except: header_list(headers, "x-inertia-partial-except").unwrap_or_default(),
            reset: header_list(headers, "x-inertia-reset").unwrap_or_default(),
            error_bag: header_text(headers, "x-inertia-error-bag").filter(|s| !s.is_empty()),
            prefetch: header_text(headers, "purpose")
                .is_some_and(|v| v.eq_ignore_ascii_case("prefetch")),
            method: method.clone(),
            url: local_url(
                uri.path_and_query()
                    .map_or_else(|| uri.path(), |pq| pq.as_str()),
            ),
        }
    }
}

/// A `409 Conflict` carrying `name: value` (Inertia's "do something else" answers), with an empty body (the
/// content type keeps the error-page layer from filling it).
fn conflict(name: &'static str, value: &str) -> Response {
    let mut response = StatusCode::CONFLICT.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    match HeaderValue::from_str(value) {
        Ok(v) => {
            response.headers_mut().insert(name, v);
        }
        Err(_) => {
            return Error::internal(format!("`{value}` is not a valid {name} header"))
                .into_response();
        }
    }
    response
}

/// `alloy::location`'s target, for the middleware.
#[derive(Clone, Debug)]
pub(crate) struct ExternalLocation(pub(crate) String);

/// The Alloy web middleware: every web route of an Alloy app runs inside it. (`Vary: X-Inertia` comes from the web
/// stack, `AppBuilder::vary_web_responses`, so the stack's own answers carry it too, D-280.)
pub(crate) async fn handle(inner: Arc<Inner>, req: Request, next: Next) -> Response {
    let inner = &inner;
    let (Some(app), Some(session), Some(auth)) = (
        req.extensions().get::<App>().cloned(),
        req.extensions().get::<Session>().cloned(),
        req.extensions().get::<Auth>().cloned(),
    ) else {
        // No session (no APP_KEY): nothing to do; a page answers its 500.
        return next.run(req).await;
    };
    let (parts, body) = req.into_parts();
    let facts = Facts::read(&parts.method, &parts.uri, &parts.headers);
    let back = Back::for_client(
        &parts.headers,
        &ClientInfo::from_parts(&parts),
        &app.settings().url,
    );
    let ctx = SharedCtx {
        app: app.clone(),
        session: session.clone(),
        auth,
        headers: parts.headers.clone(),
        uri: parts.uri.clone(),
        method: parts.method.clone(),
    };
    let req = Request::from_parts(parts, body);

    // D-279: an Inertia GET / HEAD with an old asset version reloads the page fully; the handler does not run.
    let reading = matches!(facts.method, Method::GET | Method::HEAD);
    let current = if facts.inertia && reading {
        Some(version_off_thread(inner, &app).await)
    } else {
        None
    };
    if let Some(current) = &current
        && facts.version.as_deref().unwrap_or("") != current
    {
        session.reflash();
        let mut response = conflict("x-inertia-location", &facts.url);
        if let Ok(v) = HeaderValue::from_str(current) {
            response.headers_mut().insert("x-inertia-version", v);
        }
        return response;
    }

    let mut response = next.run(req).await;
    if let Some(slot) = response.extensions_mut().remove::<PageSlot>()
        && let Some(page) = slot.take()
    {
        let version = match current {
            Some(version) => version,
            None => version_off_thread(inner, &app).await,
        };
        return match finish(inner, &facts, ctx, page, version).await {
            Ok(mut finished) => {
                keep_handler_headers(response.headers(), finished.headers_mut());
                finished
            }
            Err(e) => e.into_response(),
        };
    }
    if let Some(ExternalLocation(url)) = response.extensions_mut().remove::<ExternalLocation>() {
        return if facts.inertia {
            conflict("x-inertia-location", &url)
        } else {
            response
        };
    }
    if !facts.inertia {
        return response;
    }
    if reading && smeltery_core::view::is_view(&response) {
        // A Mold page (one not ported, the Watchfire dashboard): a full page load, not the client's error modal. Only
        // for GET / HEAD: reloading the URL of a POST / PUT / DELETE would lose the submitted data (D-285).
        session.reflash();
        return conflict("x-inertia-location", &facts.url);
    }
    let status = response.status();
    if status == StatusCode::FOUND
        && matches!(facts.method, Method::PUT | Method::PATCH | Method::DELETE)
    {
        *response.status_mut() = StatusCode::SEE_OTHER;
    }
    if status.is_redirection() && !facts.prefetch {
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if let Some(location) = location
            && location.contains('#')
        {
            return conflict("x-inertia-redirect", &location);
        }
    }
    // A Mold view is still unrendered here (an empty body); it is not an empty answer.
    let empty = !smeltery_core::view::is_view(&response)
        && response.headers().get(header::CONTENT_TYPE).is_none()
        && http_body::Body::size_hint(response.body()).exact() == Some(0);
    if status == StatusCode::OK && empty {
        return back.redirect().into_response();
    }
    response
}

/// Whether the partial-reload list `only` names `key` (or a path inside it).
fn named(only: &[String], key: &str) -> bool {
    only.iter().any(|p| {
        p == key
            || p.strip_prefix(key)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

/// The paths under `key` (`user.name` → `name` for `user`); `None` when `key` itself is listed.
fn sub_paths<'p>(paths: &'p [String], key: &str) -> Option<Vec<&'p str>> {
    let mut out = Vec::new();
    for p in paths {
        if p == key {
            return None;
        }
        if let Some(rest) = p.strip_prefix(key).and_then(|r| r.strip_prefix('.')) {
            out.push(rest);
        }
    }
    Some(out)
}

/// Keep only `paths` (dot paths) inside `value`; values that are not objects are kept whole.
fn keep_only(value: Value, paths: &[&str]) -> Value {
    let Value::Object(map) = value else {
        return value;
    };
    let mut out = Map::new();
    for (k, v) in map {
        let mut whole = false;
        let mut sub = Vec::new();
        for p in paths {
            if *p == k {
                whole = true;
            } else if let Some(rest) = p.strip_prefix(k.as_str()).and_then(|r| r.strip_prefix('.'))
            {
                sub.push(rest);
            }
        }
        if whole {
            out.insert(k, v);
        } else if !sub.is_empty() {
            out.insert(k, keep_only(v, &sub));
        }
    }
    Value::Object(out)
}

/// Remove the dot path `path` inside `value`.
fn remove_path(value: &mut Value, path: &str) {
    let Value::Object(map) = value else { return };
    match path.split_once('.') {
        None => {
            map.remove(path);
        }
        Some((first, rest)) => {
            if let Some(inner) = map.get_mut(first) {
                remove_path(inner, rest);
            }
        }
    }
}

/// In debug builds, one warning per component whose page file is missing (the client would fail with an opaque
/// "page not found" in the browser console). Each component is looked up once per process.
fn warn_missing_page(inner: &Inner, root: &Path, component: &str) {
    if !cfg!(debug_assertions)
        || !inner
            .checked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(component.to_owned())
    {
        return;
    }
    let pages = root.join("resources").join("js").join("pages");
    let exists = ["tsx", "jsx", "vue", "svelte", "ts", "js"]
        .iter()
        .any(|ext| pages.join(format!("{component}.{ext}")).is_file());
    if !exists {
        tracing::warn!(
            component,
            dir = %pages.display(),
            "no page file for this Alloy component"
        );
    }
}

/// Finish a page: shared props, prop filtering, lazy props, metadata, `errors`, `flash`, history flags; then JSON
/// for an Inertia visit or the root template for a first visit.
async fn finish(
    inner: &Arc<Inner>,
    facts: &Facts,
    ctx: SharedCtx,
    page: Page,
    version: String,
) -> Result<Response> {
    if let Some(e) = page.error {
        return Err(Error::internal(e));
    }
    let app = ctx.app.clone();
    let session = ctx.session.clone();
    let shared = match &inner.share {
        Some(share) => share(ctx).await?,
        None => Props::new(),
    };
    let shared_keys: Vec<String> = shared
        .props
        .iter()
        .map(|p| p.key.clone())
        .filter(|k| k != "errors")
        .collect();
    let mut props: Vec<Prop> = shared.props;
    for prop in page.props.props {
        props.retain(|p| p.key != prop.key);
        props.push(prop);
    }
    if props.iter().any(|p| p.key == "errors") {
        if cfg!(debug_assertions) {
            return Err(Error::internal(
                "a prop named `errors`: Alloy sets `errors` itself from the validation errors",
            ));
        }
        props.retain(|p| p.key != "errors");
    }

    let partial =
        facts.inertia && facts.partial_component.as_deref() == Some(page.component.as_str());
    let mut deferred: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut merges: [Vec<String>; 3] = Default::default();
    let mut match_on = Vec::new();
    let mut chosen = Vec::new();
    for prop in props {
        let included = match prop.load {
            Load::Always => true,
            Load::Regular | Load::Optional | Load::Deferred if partial => {
                facts
                    .only
                    .as_ref()
                    .is_none_or(|only| named(only, &prop.key))
                    && !facts.except.contains(&prop.key)
            }
            Load::Regular => true,
            Load::Optional => false,
            Load::Deferred => {
                deferred
                    .entry(prop.group.clone())
                    .or_default()
                    .push(prop.key.clone());
                false
            }
        };
        if !included {
            continue;
        }
        if let Some(kind) = prop.merge
            && !facts.reset.contains(&prop.key)
        {
            let list = match kind {
                Merge::Append => &mut merges[0],
                Merge::Prepend => &mut merges[1],
                Merge::Deep => &mut merges[2],
            };
            list.push(prop.key.clone());
            match_on.extend(prop.match_on.iter().map(|f| format!("{}.{f}", prop.key)));
        }
        chosen.push((prop.key, prop.source, prop.load == Load::Always));
    }

    // Lazy props of this response, computed concurrently; a failing one fails the request.
    let resolved = futures_util::future::try_join_all(chosen.into_iter().map(
        |(key, source, always)| async move {
            let value = match source {
                Source::Value(v) => v,
                Source::Lazy(f) => f().await?,
                Source::Failed(e) => {
                    return Err(Error::internal(format!(
                        "the prop `{key}` cannot be serialized: {e}"
                    )));
                }
            };
            Ok::<_, Error>((key, value, always))
        },
    ))
    .await?;

    let mut out = Map::new();
    for (key, mut value, always) in resolved {
        if partial && !always {
            if let Some(only) = &facts.only
                && let Some(sub) = sub_paths(only, &key)
                && !sub.is_empty()
            {
                value = keep_only(value, &sub);
            }
            for e in &facts.except {
                if let Some(rest) = e
                    .strip_prefix(key.as_str())
                    .and_then(|r| r.strip_prefix('.'))
                {
                    remove_path(&mut value, rest);
                }
            }
        }
        out.insert(key, value);
    }

    let errors = session.errors();
    let mut by_field = Map::new();
    for (field, messages) in errors.iter() {
        let value = if inner.all_errors {
            Value::from(messages.to_vec())
        } else {
            messages
                .first()
                .map_or(Value::Null, |m| Value::from(m.clone()))
        };
        by_field.insert(field.to_owned(), value);
    }
    let errors = match &facts.error_bag {
        Some(bag) if !by_field.is_empty() => {
            let mut wrapped = Map::new();
            wrapped.insert(bag.clone(), Value::Object(by_field));
            wrapped
        }
        _ => by_field,
    };
    out.insert("errors".to_owned(), Value::Object(errors));

    warn_missing_page(inner, &app.settings().root, &page.component);

    let mut object = Map::new();
    object.insert("component".to_owned(), Value::from(page.component.clone()));
    object.insert("props".to_owned(), Value::Object(out));
    object.insert("url".to_owned(), Value::from(facts.url.clone()));
    object.insert("version".to_owned(), Value::from(version));
    if !shared_keys.is_empty() {
        object.insert("sharedProps".to_owned(), Value::from(shared_keys));
    }
    for (name, list) in ["mergeProps", "prependProps", "deepMergeProps"]
        .iter()
        .zip(merges)
    {
        if !list.is_empty() {
            object.insert((*name).to_owned(), Value::from(list));
        }
    }
    if !match_on.is_empty() {
        object.insert("matchPropsOn".to_owned(), Value::from(match_on));
    }
    if !partial && !deferred.is_empty() {
        let groups: Map<String, Value> = deferred
            .into_iter()
            .map(|(g, keys)| (g, Value::from(keys)))
            .collect();
        object.insert("deferredProps".to_owned(), Value::Object(groups));
    }
    if page.encrypt_history.unwrap_or(inner.encrypt_history) {
        object.insert("encryptHistory".to_owned(), Value::Bool(true));
    }
    let clear_flag = session.get::<bool>(CLEAR_HISTORY_KEY).unwrap_or(false);
    if clear_flag {
        session.remove(CLEAR_HISTORY_KEY);
    }
    if page.clear_history || clear_flag {
        object.insert("clearHistory".to_owned(), Value::Bool(true));
    }
    let flash = session.flashed();
    if !flash.is_empty() {
        object.insert("flash".to_owned(), Value::Object(flash));
    }

    let object = Value::Object(object);
    if facts.inertia {
        let json = serde_json::to_string(&object).map_err(Error::other)?;
        let mut response = (page.status, Body::from(json)).into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert("x-inertia", HeaderValue::from_static("true"));
        return Ok(response);
    }
    let mut response = (inner.root)(page.status);
    response.extensions_mut().insert(PagePayload::new(&object));
    Ok(response)
}

/// The Alloy page element `@alloy` renders (D-282): `<script data-page="id" type="application/json">…</script>` and
/// the root `<div id="id">`.
pub(crate) fn page_element(
    page: Option<&PagePayload>,
    id: &str,
) -> std::result::Result<String, String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "invalid Alloy root id `{id}`: use letters, digits, `-` and `_`"
        ));
    }
    let page = page.ok_or_else(|| {
        "this response has no Alloy page: render the root template through `alloy::render(…)`, not directly".to_owned()
    })?;
    Ok(format!(
        "<script data-page=\"{id}\" type=\"application/json\">{}</script><div id=\"{id}\"></div>",
        page.json()
    ))
}

/// Headers the handler set on its page's response (`Cache-Control: no-store` on a page with secrets, cookies): the
/// finished page keeps every value of them (two `Set-Cookie` stay two), except names the protocol already set on the
/// finished page, and the headers that describe the old body (`Content-Type`, `Content-Length`, `Content-Encoding`,
/// `Content-Range`, `Content-Disposition`, `ETag`, `Last-Modified`) or the connection (`Connection`, `Keep-Alive`,
/// `Transfer-Encoding`, `Upgrade`, `TE`, `Trailer`), which would be wrong for the new body.
fn keep_handler_headers(from: &HeaderMap, to: &mut HeaderMap) {
    // Taken before the loop: a name the handler sets twice is not "already set" by its own first value.
    let finished: Vec<header::HeaderName> = to.keys().cloned().collect();
    for (name, value) in from {
        let skipped = matches!(
            *name,
            header::CONTENT_TYPE
                | header::CONTENT_LENGTH
                | header::CONTENT_ENCODING
                | header::CONTENT_RANGE
                | header::CONTENT_DISPOSITION
                | header::ETAG
                | header::LAST_MODIFIED
                | header::TRANSFER_ENCODING
                | header::CONNECTION
                | header::UPGRADE
                | header::TE
                | header::TRAILER
        ) || name.as_str() == "keep-alive";
        if !skipped && !finished.contains(name) {
            to.append(name.clone(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;
    use serde_json::json;

    #[test]
    fn dot_path_filters() {
        let only = vec![
            "user.name".to_owned(),
            "user.address.city".to_owned(),
            "posts".to_owned(),
        ];
        assert!(named(&only, "user"));
        assert!(named(&only, "posts"));
        assert!(!named(&only, "use"));
        assert!(!named(&only, "users"));
        assert_eq!(sub_paths(&only, "posts"), None);
        let sub = sub_paths(&only, "user").unwrap();
        assert_eq!(sub, ["name", "address.city"]);
        let user = json!({"name": "Ada", "email": "a@b", "address": {"city": "X", "zip": "1"}});
        assert_eq!(
            keep_only(user.clone(), &sub),
            json!({"name": "Ada", "address": {"city": "X"}})
        );
        let mut user = user;
        remove_path(&mut user, "address.zip");
        remove_path(&mut user, "email");
        remove_path(&mut user, "nope.deeper");
        assert_eq!(user, json!({"name": "Ada", "address": {"city": "X"}}));
    }

    #[test]
    fn the_page_element_needs_a_page_and_a_plain_id() {
        let page = PagePayload::new(&json!({}));
        assert_eq!(
            page_element(Some(&page), "app").unwrap(),
            "<script data-page=\"app\" type=\"application/json\">{}</script><div id=\"app\"></div>"
        );
        assert!(page_element(Some(&page), "a\"b").is_err());
        assert!(
            page_element(None, "app")
                .unwrap_err()
                .contains("no Alloy page")
        );
    }

    #[test]
    fn facts_read_the_inertia_headers() {
        let mut h = HeaderMap::new();
        h.insert("x-inertia", HeaderValue::from_static("true"));
        h.insert(
            "x-inertia-partial-data",
            HeaderValue::from_static("a, b ,,c"),
        );
        h.insert("x-inertia-partial-except", HeaderValue::from_static("b"));
        h.insert(
            "x-inertia-partial-component",
            HeaderValue::from_static("home"),
        );
        h.insert("purpose", HeaderValue::from_static("prefetch"));
        let uri: Uri = "/posts?page=2".parse().unwrap();
        let f = Facts::read(&Method::GET, &uri, &h);
        assert!(f.inertia && f.prefetch);
        assert_eq!(f.only.unwrap(), ["a", "b", "c"]);
        assert_eq!(f.except, ["b"]);
        assert_eq!(f.url, "/posts?page=2");
        assert_eq!(f.partial_component.as_deref(), Some("home"));
        let f = Facts::read(&Method::GET, &uri, &HeaderMap::new());
        assert!(!f.inertia && f.only.is_none());
    }

    #[test]
    fn partial_reload_lists_are_capped() {
        let many: Vec<String> = (0..1000).map(|i| format!("p{i}")).collect();
        let mut h = HeaderMap::new();
        for name in [
            "x-inertia-partial-data",
            "x-inertia-partial-except",
            "x-inertia-reset",
        ] {
            h.insert(name, HeaderValue::from_str(&many.join(",")).unwrap());
        }
        let f = Facts::read(&Method::GET, &"/".parse().unwrap(), &h);
        let only = f.only.unwrap();
        assert_eq!(only.len(), 256);
        assert_eq!(only[255], "p255");
        assert_eq!(f.except.len(), 256);
        assert_eq!(f.reset.len(), 256);
    }

    #[test]
    fn the_url_is_always_a_path_on_this_site() {
        for (raw, local) in [
            ("/posts?page=2", "/posts?page=2"),
            ("/", "/"),
            ("/?next=//evil.example", "/?next=//evil.example"),
            ("//evil.example/x", "/evil.example/x"),
            ("///evil.example/x?a=1", "/evil.example/x?a=1"),
            ("/\\evil.example/x", "/evil.example/x"),
            ("\\\\evil.example", "/evil.example"),
            ("/\\/\\evil.example", "/evil.example"),
            ("/\t/evil.example", "/"),
            ("//\t/evil.example", "/"),
            ("/\n/evil.example", "/"),
            ("/a\\b", "/"),
            ("/caf\u{e9}", "/"),
            ("", "/"),
        ] {
            let url = local_url(raw);
            assert_eq!(url, local, "{raw:?}");
            assert!(is_local_path(&url), "{raw:?} → {url:?}");
        }
        // Through the request: the path the router saw.
        for (path, local) in [
            ("//evil.example/x", "/evil.example/x"),
            ("/\\evil.example/x", "/evil.example/x"),
        ] {
            let uri: Uri = path.parse().unwrap();
            let f = Facts::read(&Method::GET, &uri, &HeaderMap::new());
            assert_eq!(f.url, local, "{path:?}");
        }
    }
}
