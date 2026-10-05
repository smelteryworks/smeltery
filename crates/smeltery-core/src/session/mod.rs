//! Sessions: per-visitor data kept between requests on web routes.
//!
//! The session middleware runs on every route from `routes/web.rs` (not on `/api` routes).
//! With `SESSION_DRIVER=cookie` (the default) the whole session lives in one cookie,
//! encrypted and authenticated with AES-256-GCM under a key derived from `APP_KEY`; with
//! `SESSION_DRIVER=database` the cookie holds only the encrypted session id and the data lives
//! in the `sessions` table (`id`, `payload`, `last_activity`); with `SESSION_DRIVER=file` the
//! cookie holds the encrypted id and the data lives in `storage/framework/sessions/<id>`, one
//! file per session whose modification time is its last activity.
//!
//! ```
//! use smeltery_core::http::Redirect;
//! use smeltery_core::session::Session;
//!
//! async fn save(session: Session) -> Redirect {
//!     session.insert("theme", "dark");
//!     session.flash("status", "Settings saved.");
//!     Redirect::to("/settings")
//! }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::error::Error;
use crate::validation::{Input, ValidationErrors};
use crate::view::ViewData;

pub(crate) mod store;
pub(crate) mod web;

/// Run the session stack of web routes around the rest of the request, as middleware: load the session, sign in
/// from a remember-me cookie, check the session's password-hash binding, check the CSRF token on every method but
/// `GET`, `HEAD`, `OPTIONS` and `TRACE`, run `next`, flash a failed validation and redirect back, save the session
/// and set the `XSRF-TOKEN` cookie when [`AppBuilder::xsrf_cookie`](crate::AppBuilder::xsrf_cookie) is on. It is the
/// same code every web route runs (there is no other copy), for routes outside `routes/web.rs` that must behave
/// like web routes.
///
/// ```
/// use smeltery_core::middleware::{Next, Request};
/// use smeltery_core::session::run_web_stack;
/// use smeltery_core::{App, AppBuilder, Response};
///
/// async fn page() -> &'static str { "with a session" }
///
/// async fn web(req: Request, next: Next) -> Response {
///     let app = req.extensions().get::<App>().cloned().expect("the app");
///     run_web_stack(app, req, next).await
/// }
///
/// fn build(app: AppBuilder) -> AppBuilder {
///     app.middleware("web", web)
///     .api_routes(|r| { r.get("/session-page", page).middleware("web"); })
/// }
/// # let _ = build;
/// ```
pub async fn run_web_stack(
    app: crate::App,
    req: crate::middleware::Request,
    next: crate::middleware::Next,
) -> crate::Response {
    web::web_stack(app, req, next).await
}

/// Read the visitor's session from the cookies in `headers` without changing anything: no save, no `Set-Cookie`,
/// no remember-me sign-in, no CSRF check. For routes outside the web stack (API routes, streams) that must know
/// which session a request belongs to. `None` when the request carries no valid session (missing, tampered, idle
/// or past `SESSION_ABSOLUTE_LIFETIME`) or the app has no `APP_KEY`. Changes made to the returned session are never
/// stored. [`Auth::peek`](crate::auth::Auth::peek) reads its sign-in.
///
/// ```
/// use smeltery_core::App;
/// use smeltery_core::session;
///
/// async fn binding_of(app: App, headers: http::HeaderMap) -> smeltery_core::Result<Option<String>> {
///     Ok(session::peek(&app, &headers).await?.map(|s| s.binding()))
/// }
/// # let _ = binding_of;
/// ```
///
/// # Errors
/// The session store failed (the database or the session file could not be read).
pub async fn peek(app: &App, headers: &http::HeaderMap) -> crate::Result<Option<Session>> {
    let Some(web) = app.web_config() else {
        return Ok(None);
    };
    let jar = store::request_jar(headers);
    Ok(store::peek(app, &web.keys, web.driver, &jar)
        .await?
        .map(Session::from_state))
}

/// Session keys Smeltery uses itself.
pub(crate) const TOKEN_KEY: &str = "_token";
pub(crate) const ERRORS_KEY: &str = "_errors";
pub(crate) const OLD_KEY: &str = "_old_input";
pub(crate) const AUTH_KEY: &str = "auth_id";
/// What the signed-in session is bound to (`auth::session_binding`).
pub(crate) const AUTH_HASH_KEY: &str = "_auth_hash";
/// The page the `auth` middleware turned a guest away from (`Auth::intended`).
pub(crate) const INTENDED_KEY: &str = "_intended_url";

/// What is stored: the values and which of them are flash values.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Payload {
    #[serde(default)]
    pub(crate) data: BTreeMap<String, serde_json::Value>,
    /// Flashed during this request: kept for the next one.
    #[serde(default)]
    pub(crate) flash_new: Vec<String>,
    /// Flashed during the previous request: removed after this one.
    #[serde(default)]
    pub(crate) flash_old: Vec<String>,
    /// Unix seconds after which the session is void (cookie driver).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expires: Option<u64>,
    /// The session id (cookie driver; the database driver keeps it in the cookie).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sid: Option<String>,
    /// Unix seconds when the session started (or its user signed in): the clock of
    /// `SESSION_ABSOLUTE_LIFETIME`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) issued: Option<u64>,
}

/// A cookie to set or remove with the response (the remember-me cookie).
#[derive(Clone)]
pub(crate) enum Queued {
    /// Set an encrypted cookie.
    Set {
        name: String,
        value: String,
        max_age: Duration,
    },
    /// Remove a cookie.
    Remove { name: String },
}

pub(crate) struct State {
    pub(crate) id: String,
    pub(crate) payload: Payload,
    /// The id before `regenerate` / `invalidate` (the database driver deletes its row).
    pub(crate) previous_id: Option<String>,
    pub(crate) queued: Vec<Queued>,
    /// A failure (no random source) that must stop the response from being saved.
    pub(crate) failed: Option<String>,
    /// Whether the session came with the request (else it was started for it).
    pub(crate) loaded: bool,
}

impl State {
    pub(crate) fn new(id: String, payload: Payload) -> Self {
        Self {
            id,
            payload,
            previous_id: None,
            queued: Vec::new(),
            failed: None,
            loaded: false,
        }
    }

    fn new_id(&mut self) -> String {
        match crate::crypto::random_token(40) {
            Ok(id) => id,
            Err(e) => {
                self.failed = Some(e.to_string());
                String::new()
            }
        }
    }
}

/// The current visitor's session, as a handler argument (`session: Session`).
///
/// Cheap to clone; every clone is the same session. Changes are saved after the handler
/// returns. Available on web routes only.
#[derive(Clone)]
pub struct Session {
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the id or the values: they are secrets.
        let state = self.lock();
        f.debug_struct("Session")
            .field("keys", &state.payload.data.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn from_state(state: State) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The value under `key`, if it is there and has this type.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let value = self.lock().payload.data.get(key).cloned()?;
        serde_json::from_value(value).ok()
    }

    /// Store `value` under `key` (a value that cannot be serialized is skipped with a
    /// warning naming the key).
    pub fn insert(&self, key: &str, value: impl Serialize) {
        match serde_json::to_value(value) {
            Ok(value) => {
                self.lock().payload.data.insert(key.to_owned(), value);
            }
            Err(_) => tracing::warn!(key, "a session value cannot be serialized"),
        }
    }

    /// Remove `key`.
    pub fn remove(&self, key: &str) {
        let mut state = self.lock();
        state.payload.data.remove(key);
        state.payload.flash_new.retain(|k| k != key);
        state.payload.flash_old.retain(|k| k != key);
    }

    /// Whether `key` holds a value.
    pub fn has(&self, key: &str) -> bool {
        self.lock().payload.data.contains_key(key)
    }

    /// Store `value` under `key` for the next request only.
    pub fn flash(&self, key: &str, value: impl Serialize) {
        self.insert(key, value);
        let mut state = self.lock();
        state.payload.flash_old.retain(|k| k != key);
        if !state.payload.flash_new.iter().any(|k| k == key) {
            state.payload.flash_new.push(key.to_owned());
        }
    }

    /// Flash validation `errors` and the form's `input` for the next request, as a failed validation does before
    /// its redirect back: the next page shows them with `@error` / `old()` (Mold), `errors` (Alloy) and
    /// [`Session::errors`] / [`Session::old`]. The input is filtered like every flashed input: no field that looks
    /// like a secret, none named with [`AppBuilder::dont_flash`](crate::AppBuilder::dont_flash), at most 100 fields
    /// of at most 16 KiB (64 KiB in all). Answer with a redirect to any target afterwards.
    ///
    /// ```
    /// use smeltery_core::http::Redirect;
    /// use smeltery_core::session::Session;
    /// use smeltery_core::validation::{Input, ValidationErrors};
    /// use smeltery_core::App;
    ///
    /// async fn refuse(app: App, session: Session) -> Redirect {
    ///     let mut errors = ValidationErrors::new();
    ///     errors.add("code", "The code is invalid.");
    ///     let input = Input::from([("email".to_owned(), "ada@example.com".to_owned())]);
    ///     session.flash_errors(&app, &errors, &input);
    ///     Redirect::to("/two-factor-challenge")
    /// }
    /// # let _ = refuse;
    /// ```
    pub fn flash_errors(&self, app: &crate::App, errors: &ValidationErrors, input: &Input) {
        self.flash(ERRORS_KEY, errors);
        self.flash(OLD_KEY, crate::validation::old_input(app, input));
    }

    /// Keep this request's flash values for one more request.
    pub fn reflash(&self) {
        let mut state = self.lock();
        let old = std::mem::take(&mut state.payload.flash_old);
        for key in old {
            if !state.payload.flash_new.contains(&key) {
                state.payload.flash_new.push(key);
            }
        }
    }

    /// Give the session a new id and keep its data (on login, against session fixation).
    pub fn regenerate(&self) {
        let mut state = self.lock();
        let new = state.new_id();
        let old = std::mem::replace(&mut state.id, new);
        if state.previous_id.is_none() {
            state.previous_id = Some(old);
        }
    }

    /// Give the session a new id and drop all its data (on logout), its CSRF token included.
    pub fn invalidate(&self) {
        self.regenerate();
        self.lock().payload = Payload::default();
    }

    /// Start the absolute-lifetime clock again (on sign-in).
    pub(crate) fn restart_clock(&self) {
        self.lock().payload.issued = Some(store::now_secs());
    }

    /// Whether the session holds nothing at all: no value, no flash value. The web stack does
    /// not store such a session for a visitor who came without one.
    pub(crate) fn is_empty(&self) -> bool {
        let state = self.lock();
        state.payload.data.is_empty()
            && state.payload.flash_new.is_empty()
            && state.payload.flash_old.is_empty()
            && state.queued.is_empty()
    }

    /// The session id (a secret: never log it).
    pub fn id(&self) -> String {
        self.lock().id.clone()
    }

    /// The session's CSRF secret, created on first use. Pages never show it: `@csrf`,
    /// `csrf_token()` and the Sparks meta tag render [`Session::csrf_token`].
    pub fn token(&self) -> String {
        let mut state = self.lock();
        if let Some(serde_json::Value::String(token)) = state.payload.data.get(TOKEN_KEY) {
            return token.clone();
        }
        let token = state.new_id();
        state.payload.data.insert(
            TOKEN_KEY.to_owned(),
            serde_json::Value::String(token.clone()),
        );
        token
    }

    /// The CSRF token to put in a page or send back in a form, `_token` field or
    /// `X-CSRF-TOKEN` header: the session's token masked with a fresh random pad, so it reads
    /// differently every time (a compressed HTTPS page cannot leak it through its length,
    /// "BREACH"); the CSRF check accepts every masked form, and the unmasked [`Session::token`].
    ///
    /// # Errors
    /// The OS random source fails.
    pub fn csrf_token(&self) -> crate::Result<String> {
        crate::crypto::mask_csrf(&self.token())
    }

    /// The values the previous request flashed with [`Session::flash`], by key, leaving out
    /// Smeltery's own keys (those starting with `_`: errors, old input, internal values).
    ///
    /// Alloy sends these to the browser as the page's `flash` object, so anything flashed under
    /// a key without `_` is public there: never flash a secret.
    pub fn flashed(&self) -> serde_json::Map<String, serde_json::Value> {
        let state = self.lock();
        state
            .payload
            .flash_old
            .iter()
            .filter(|key| !key.starts_with('_'))
            .filter_map(|key| {
                let value = state.payload.data.get(key)?;
                Some((key.clone(), value.clone()))
            })
            .collect()
    }

    /// The validation errors flashed by the previous request.
    pub fn errors(&self) -> ValidationErrors {
        self.get(ERRORS_KEY).unwrap_or_default()
    }

    /// The input the previous request flashed back for `field`.
    pub fn old(&self, field: &str) -> Option<String> {
        self.get::<Input>(OLD_KEY)?.remove(field)
    }

    /// 24 hex characters naming this session without revealing its CSRF secret: the first 12 bytes of the
    /// secret's SHA-256 (the secret is created when the session has none). It changes at sign-in and sign-out.
    /// What signed state is bound to (Sparks snapshots and uploads) and what a signed-in session's
    /// [`Principal::key`](crate::auth::Principal::key) names (`session:<binding>`).
    pub fn binding(&self) -> String {
        let mut hash = crate::crypto::sha256_hex(&self.token());
        hash.truncate(24);
        hash
    }

    /// Remove every key that starts with one of `prefixes`.
    pub(crate) fn remove_prefixed(&self, prefixes: &[&str]) {
        let mut state = self.lock();
        let gone = |k: &String| prefixes.iter().any(|p| k.starts_with(p));
        state.payload.data.retain(|k, _| !gone(k));
        state.payload.flash_new.retain(|k| !gone(k));
        state.payload.flash_old.retain(|k| !gone(k));
    }

    pub(crate) fn auth_id(&self) -> Option<i64> {
        self.get(AUTH_KEY)
    }

    pub(crate) fn queue(&self, cookie: Queued) {
        self.lock().queued.push(cookie);
    }

    pub(crate) fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.lock())
    }

    /// Whether the session has a CSRF token already.
    pub(crate) fn has_token(&self) -> bool {
        self.lock().payload.data.contains_key(TOKEN_KEY)
    }

    /// What templates read: token, signed-in state, flashed errors and input, scalar values.
    /// With `page` (the response is a view) a session without a CSRF token gets one; otherwise
    /// the view data carries a token only when the session has one already, so a response that
    /// shows no page does not create one (and with it a stored session).
    pub(crate) fn view_data(&self, page: bool) -> ViewData {
        // One fresh mask per response (see `csrf_token`).
        let token = if page || self.has_token() {
            match self.csrf_token() {
                Ok(token) => Some(token),
                Err(e) => {
                    // Fails the request (the web stack checks `failed`), like a session id would.
                    self.lock().failed = Some(e.to_string());
                    Some(String::new())
                }
            }
        } else {
            None
        };
        let errors = self.errors();
        let old = self.get::<Input>(OLD_KEY).unwrap_or_default();
        let state = self.lock();
        let mut session = HashMap::new();
        for (key, value) in &state.payload.data {
            if key.starts_with('_') {
                continue;
            }
            let text = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            session.insert(key.clone(), text);
        }
        let mut data = ViewData::default();
        data.csrf_token = token;
        data.authenticated = state.payload.data.contains_key(AUTH_KEY);
        data.errors = errors.into_map().into_iter().collect();
        data.old = old.into_iter().collect();
        data.session = session;
        data
    }

    /// End of request: drop the previous request's flash values, keep this one's for the next.
    pub(crate) fn age_flash(&self) {
        let mut state = self.lock();
        let old = std::mem::take(&mut state.payload.flash_old);
        for key in old {
            state.payload.data.remove(&key);
        }
        state.payload.flash_old = std::mem::take(&mut state.payload.flash_new);
    }
}

impl axum::extract::FromRequestParts<App> for Session {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or_else(|| {
            Error::internal("the session is available on web routes only (routes/web.rs)")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::from_state(State::new("id-1".into(), Payload::default()))
    }

    #[test]
    fn values_flash_and_aging() {
        let s = session();
        s.insert("n", 3);
        assert_eq!(s.get::<i32>("n"), Some(3));
        assert_eq!(s.get::<String>("n"), None);
        assert!(s.has("n"));
        s.remove("n");
        assert!(!s.has("n"));

        s.flash("status", "Saved");
        s.age_flash(); // end of the request that flashed
        assert_eq!(s.get::<String>("status").as_deref(), Some("Saved"));
        s.age_flash(); // end of the next request
        assert!(!s.has("status"));

        s.flash("a", 1);
        s.age_flash();
        s.reflash();
        s.age_flash();
        assert!(s.has("a"), "reflash keeps it one more request");
        s.age_flash();
        assert!(!s.has("a"));
    }

    #[test]
    fn flashed_holds_the_previous_requests_public_flash_values() {
        let s = session();
        s.flash("status", "Saved");
        s.flash("count", 2);
        s.flash("user", serde_json::json!({ "name": "Ada" }));
        s.flash(ERRORS_KEY, ValidationErrors::new());
        s.flash(OLD_KEY, Input::new());
        s.flash("_internal", "x");
        s.insert("plain", "not flashed");
        assert!(
            s.flashed().is_empty(),
            "flashed during this request: next one's"
        );
        s.age_flash(); // end of the request that flashed
        s.flash("later", "next request's");
        let flashed = s.flashed();
        assert_eq!(
            serde_json::Value::Object(flashed),
            serde_json::json!({ "status": "Saved", "count": 2, "user": { "name": "Ada" } })
        );
        s.age_flash();
        assert_eq!(
            serde_json::Value::Object(s.flashed()),
            serde_json::json!({ "later": "next request's" })
        );
        s.remove("later");
        assert!(s.flashed().is_empty());
    }

    #[test]
    fn the_binding_is_the_hash_of_the_csrf_secret() {
        // The value Sparks has always bound snapshots and uploads to: 12 bytes of SHA-256(secret), as hex.
        let s = session();
        let expected = crate::crypto::sha256_hex(&s.token());
        assert_eq!(s.binding(), expected[..24]);
        s.invalidate();
        assert_ne!(s.binding(), expected[..24], "a new secret, a new binding");
    }

    #[test]
    fn regenerate_and_invalidate() {
        let s = session();
        s.insert("k", "v");
        let token = s.token();
        assert_eq!(token.len(), 40);
        assert_eq!(s.token(), token, "stable once created");
        s.regenerate();
        assert_ne!(s.id(), "id-1");
        assert!(s.has("k"));
        s.invalidate();
        assert!(!s.has("k"));
        assert_ne!(s.token(), token);
        s.with_state(|st| assert_eq!(st.previous_id.as_deref(), Some("id-1")));
        assert!(!format!("{s:?}").contains("id-1"));
    }

    #[test]
    fn only_pages_create_a_csrf_token() {
        let s = session();
        assert!(s.is_empty());
        let data = s.view_data(false);
        assert_eq!(data.csrf_token, None);
        assert!(!s.has_token() && s.is_empty(), "nothing to store");
        let data = s.view_data(true);
        assert!(data.csrf_token.is_some() && s.has_token());
        // Once there is a token, every response carries it (masked).
        assert!(s.view_data(false).csrf_token.is_some());
        assert!(!s.is_empty());
    }

    #[test]
    fn view_data_holds_scalars_errors_and_old_input() {
        let s = session();
        let mut errors = ValidationErrors::new();
        errors.add("email", "bad");
        s.insert(ERRORS_KEY, &errors);
        s.insert(
            OLD_KEY,
            Input::from([("email".to_owned(), "x@y".to_owned())]),
        );
        s.insert("status", "ok");
        s.insert("count", 2);
        s.insert("list", [1, 2]);
        s.insert(AUTH_KEY, 5);
        let data = s.view_data(true);
        assert!(data.csrf_token.is_some());
        assert_eq!(data.errors["email"], ["bad"]);
        assert_eq!(data.old["email"], "x@y");
        assert_eq!(data.session["status"], "ok");
        assert_eq!(data.session["count"], "2");
        assert!(!data.session.contains_key("list"));
        assert!(!data.session.contains_key("_token"));
        assert!(data.authenticated);
        assert_eq!(s.errors().first("email"), Some("bad"));
        assert_eq!(s.old("email").as_deref(), Some("x@y"));
    }
}
