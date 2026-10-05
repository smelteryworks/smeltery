//! Who made a request: the [`Principal`] a [`Guard`] resolves, the `auth:<guards>` middleware family and the
//! [`Authenticated`] extractor.
//!
//! A guard reads a request's head and answers with the principal it proves, or `None` when the request does not
//! carry its kind of credential. Core registers the guard `web` (the signed-in session) with
//! [`AppBuilder::auth`](crate::AppBuilder::auth); other crates register theirs with
//! [`AppBuilder::guard`](crate::AppBuilder::guard). The principal is the one answer to "who is this" that the
//! `throttle:` and `verified` middleware and [`Authenticated`] read.
//!
//! ```
//! use smeltery_core::auth::Authenticated;
//! use smeltery_core::routing::Router;
//!
//! async fn me(who: Authenticated) -> String {
//!     format!("user {}", who.user_id)
//! }
//!
//! fn routes(r: &mut Router) {
//!     r.get("/me", me).middleware("auth:web");
//! }
//! # let _ = routes;
//! ```

use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use http::StatusCode;
use http::request::Parts;
use sea_orm::prelude::DateTimeUtc;

use super::{AuthUser, Authenticatable};
use crate::app::{App, BoxFuture};
use crate::error::{Error, Result};
use crate::middleware::{ErasedMiddleware, Next, Request};
use crate::session::Session;

/// The name of core's session guard.
pub const WEB_GUARD: &str = "web";

/// The credential a [`Principal`] was proven with.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Credential {
    /// The session cookie of a signed-in session. It holds every ability.
    #[non_exhaustive]
    Session {
        /// 24 hex characters: the first 12 bytes of the SHA-256 of the session's CSRF secret. It names this
        /// session (and changes at sign-in and sign-out) without revealing the secret.
        binding: String,
    },
    /// A bearer credential issued by a guard (an API token), with its abilities.
    #[non_exhaustive]
    Token {
        /// The credential's id at its guard.
        id: i64,
        /// What it may do: exact ability names, `*` for every ability.
        abilities: Vec<String>,
    },
}

impl Credential {
    /// A session credential with this binding (see [`Credential::Session`]).
    pub fn session(binding: impl Into<String>) -> Self {
        Self::Session {
            binding: binding.into(),
        }
    }

    /// A token credential with id `id` and these abilities (`*` = every ability).
    pub fn token<S: Into<String>>(id: i64, abilities: impl IntoIterator<Item = S>) -> Self {
        Self::Token {
            id,
            abilities: abilities.into_iter().map(Into::into).collect(),
        }
    }

    /// The kind of this credential.
    pub fn kind(&self) -> CredentialKind {
        match self {
            Self::Session { .. } => CredentialKind::Sessions,
            Self::Token { .. } => CredentialKind::Tokens,
        }
    }
}

/// Kinds of credentials, as revocation events name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CredentialKind {
    /// Sessions (and their remember-me cookies).
    Sessions,
    /// Tokens issued by a guard.
    Tokens,
    /// Every kind.
    Every,
}

/// The authenticated party of a request: a user, the guard that proved it and the credential.
#[derive(Clone)]
#[non_exhaustive]
pub struct Principal {
    /// The user's id.
    pub user_id: i64,
    /// The guard that resolved it (`web`, or a registered guard's name).
    pub guard: &'static str,
    /// The credential it was proven with.
    pub credential: Credential,
    /// When the credential stops working (a token's expiry); `None` for sessions, which follow the session rules.
    pub expires_at: Option<DateTimeUtc>,
    /// The user, loaded once per principal.
    user: Arc<tokio::sync::OnceCell<Option<AuthUser>>>,
}

impl std::fmt::Debug for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Principal")
            .field("user_id", &self.user_id)
            .field("guard", &self.guard)
            .field("credential", &self.credential.kind())
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Principal {
    /// User `user_id`, proven by `guard` with `credential`.
    pub fn new(user_id: i64, guard: &'static str, credential: Credential) -> Self {
        Self {
            user_id,
            guard,
            credential,
            expires_at: None,
            user: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    /// The same principal with a credential that stops working at `at`.
    #[must_use]
    pub fn expires_at(mut self, at: DateTimeUtc) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// The same principal with its user already loaded (`None`: the row is gone).
    #[must_use]
    pub(crate) fn with_loaded_user(self, user: Option<AuthUser>) -> Self {
        let _ = self.user.set(user);
        self
    }

    /// The same principal with `user` as its user, for a guard that loaded the user already (no second query).
    ///
    /// # Errors
    /// `user` is not the principal's user (another id).
    pub fn with_user(self, user: AuthUser) -> Result<Self> {
        if user.id() != self.user_id {
            return Err(Error::internal(format!(
                "the principal is user {}, not user {}",
                self.user_id,
                user.id()
            )));
        }
        Ok(self.with_loaded_user(Some(user)))
    }

    /// Whether the credential grants `ability`: a session grants every ability; a token grants the abilities it
    /// lists by exact name, or every ability when it lists `*`.
    pub fn can(&self, ability: &str) -> bool {
        match &self.credential {
            Credential::Session { .. } => true,
            Credential::Token { abilities, .. } => {
                abilities.iter().any(|a| a == "*" || a == ability)
            }
        }
    }

    /// What grants and revocation events name this credential by: `<guard>:session:<binding>` or
    /// `<guard>:token:<id>` (`web:session:3f…`, `hallmark:token:12`), so two guards' ids never collide.
    pub fn key(&self) -> String {
        match &self.credential {
            Credential::Session { binding } => credential_key(self.guard, "session", binding),
            Credential::Token { id, .. } => credential_key(self.guard, "token", &id.to_string()),
        }
    }

    /// The user, without its type (loaded once per principal; `None` when the row is gone).
    ///
    /// # Errors
    /// No user model is registered, or the query fails.
    pub async fn auth_user(&self, app: &App) -> Result<Option<AuthUser>> {
        self.user
            .get_or_try_init(|| app.find_user(self.user_id))
            .await
            .cloned()
    }

    /// The user as the app's model `U` (loaded once per principal).
    ///
    /// # Errors
    /// No user model is registered, the query fails, or `U` is not the registered model.
    pub async fn user<U: Authenticatable>(&self, app: &App) -> Result<Option<U>> {
        match self.auth_user(app).await? {
            None => Ok(None),
            Some(user) => user.downcast::<U>().map(Some).ok_or_else(|| {
                Error::internal(format!(
                    "`{}` is not the user model registered with `.auth::<…>()`",
                    std::any::type_name::<U>()
                ))
            }),
        }
    }
}

/// The key of a credential: `<guard>:<kind>:<id>`, the format of [`Principal::key`] (`web:session:<binding>`,
/// `hallmark:token:12`). Guard crates name the credentials they revoke with it.
pub fn credential_key(guard: &str, kind: &str, id: &str) -> String {
    format!("{guard}:{kind}:{id}")
}

/// The key of the signed-in session with this binding (the `web` guard's session credential).
pub(crate) fn session_key(binding: &str) -> String {
    credential_key(WEB_GUARD, "session", binding)
}

/// Resolves the principal of a request from its head. Register one with
/// [`AppBuilder::guard`](crate::AppBuilder::guard); routes use it as `auth:<name>`.
///
/// ```
/// use smeltery_core::auth::{Credential, Guard, Principal};
/// use smeltery_core::http::request::Parts;
/// use smeltery_core::{App, BoxFuture, Result};
///
/// /// Accepts `X-Demo-User: <id>` (a test double, never a real credential).
/// struct DemoGuard;
///
/// impl Guard for DemoGuard {
///     fn name(&self) -> &'static str { "demo" }
///     fn stateless(&self) -> bool { true }
///     fn authenticate<'a>(&'a self, _app: &'a App, parts: &'a mut Parts)
///         -> BoxFuture<'a, Result<Option<Principal>>> {
///         Box::pin(async move {
///             let id = parts.headers.get("x-demo-user")
///                 .and_then(|v| v.to_str().ok())
///                 .and_then(|v| v.parse::<i64>().ok());
///             Ok(id.map(|id| Principal::new(id, "demo", Credential::token(0, ["*"]))))
///         })
///     }
/// }
///
/// fn build(app: smeltery_core::AppBuilder) -> smeltery_core::AppBuilder {
///     app.guard(DemoGuard)
/// }
/// # let _ = build;
/// ```
pub trait Guard: Send + Sync + 'static {
    /// The guard's name, as `auth:<name>` names it: lowercase ASCII letters, digits, `_` and `-`.
    fn name(&self) -> &'static str;

    /// Whether it reads only request headers, never cookies or the session (a bearer token). Stateless guards
    /// never run on web routes (`routes/web.rs`), so a bearer credential never stands in for the session and its
    /// CSRF check there.
    fn stateless(&self) -> bool;

    /// The request's principal, `Ok(None)` when the request carries none of this guard's credentials (or an
    /// invalid one). An error fails the request. Return a new [`Principal`] for every request, never one kept from
    /// an earlier request: a principal keeps the user it loaded, so a kept one would never see the user's row
    /// change.
    fn authenticate<'a>(
        &'a self,
        app: &'a App,
        parts: &'a mut Parts,
    ) -> BoxFuture<'a, Result<Option<Principal>>>;

    /// Whether a request to an API route that lists this guard (`auth:<name>`) comes from the app's own pages (a
    /// first-party browser app), so the session authenticates it: the `auth:` middleware then runs the whole web
    /// session stack around the request ([`session::run_web_stack`](crate::session::run_web_stack): session,
    /// remember-me, password-hash binding and the CSRF check on every state-changing method, which cannot be left
    /// out) and lets a signed-in session through. Without a signed-in session the guards still run, after that
    /// CSRF check. `false` (the default): no session on API routes.
    fn first_party(&self, app: &App, parts: &Parts) -> bool {
        let _ = (app, parts);
        false
    }
}

/// Core's `web` guard: the principal the web stack set for a signed-in session.
pub(crate) struct WebGuard;

impl Guard for WebGuard {
    fn name(&self) -> &'static str {
        WEB_GUARD
    }

    fn stateless(&self) -> bool {
        false
    }

    fn authenticate<'a>(
        &'a self,
        _app: &'a App,
        parts: &'a mut Parts,
    ) -> BoxFuture<'a, Result<Option<Principal>>> {
        let found = parts
            .extensions
            .get::<Principal>()
            .filter(|p| p.guard == WEB_GUARD)
            .cloned();
        Box::pin(async move { Ok(found) })
    }
}

/// Which guards [`authenticate`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardSet {
    /// Every stateless guard, in registration order.
    Stateless,
}

/// Run the guards of `which` on a request head, in registration order: the first principal found, `Ok(None)` when
/// none of them finds one. Nothing is stored in the request. On a web request (one with a session) it answers
/// `Ok(None)` without running any guard: stateless guards never run on web routes.
///
/// # Errors
/// A guard fails.
pub async fn authenticate(
    app: &App,
    parts: &mut Parts,
    which: GuardSet,
) -> Result<Option<Principal>> {
    if parts.extensions.get::<Session>().is_some() {
        return Ok(None);
    }
    for guard in app.guards() {
        let selected = match which {
            GuardSet::Stateless => guard.stateless(),
        };
        if selected && let Some(principal) = guard.authenticate(app, parts).await? {
            return Ok(Some(principal));
        }
    }
    Ok(None)
}

/// Whether `name` is a valid guard name.
pub(crate) fn valid_guard_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The `auth:<g1>[,<g2>…]` middleware for one route: the guards in order, the first principal stored in the request.
/// `auth:web` is the plain `auth` middleware.
pub(crate) fn auth_family(guards: &[Arc<dyn Guard>], args: &str) -> Result<ErasedMiddleware> {
    let mut selected: Vec<Arc<dyn Guard>> = Vec::new();
    for name in args.split(',').map(str::trim) {
        let guard = guards.iter().find(|g| g.name() == name).ok_or_else(|| {
            let known: Vec<&str> = guards.iter().map(|g| g.name()).collect();
            Error::internal(format!(
                "invalid middleware `auth:{args}`: no guard named `{name}` (registered: {})",
                if known.is_empty() {
                    "none; `.auth::<User>()` registers `web`".to_owned()
                } else {
                    known.join(", ")
                }
            ))
        })?;
        if !selected.iter().any(|g| g.name() == name) {
            selected.push(Arc::clone(guard));
        }
    }
    if selected.iter().all(|g| g.name() == WEB_GUARD) {
        return Ok(ErasedMiddleware::new(super::require_auth));
    }
    let selected: Arc<[Arc<dyn Guard>]> = selected.into();
    Ok(ErasedMiddleware::new(move |req: Request, next: Next| {
        let selected = Arc::clone(&selected);
        async move { run_guards(&selected, req, next).await }
    }))
}

async fn run_guards(guards: &[Arc<dyn Guard>], req: Request, next: Next) -> Response {
    let Some(app) = req.extensions().get::<App>().cloned() else {
        return Error::internal("the `auth:` middleware runs outside the app").into_response();
    };
    let on_web = req.extensions().get::<Session>().is_some();
    if !on_web {
        let (parts, body) = req.into_parts();
        let stateful = guards.iter().any(|g| g.first_party(&app, &parts));
        let req = Request::from_parts(parts, body);
        if stateful {
            // The session (with its CSRF check) authenticates a first-party request; the guards run inside it.
            let guards: Vec<Arc<dyn Guard>> = guards.to_vec();
            return crate::session::web::web_stack_then(app.clone(), req, move |req| async move {
                stateful_guards(&app, &guards, req, next).await
            })
            .await;
        }
        return run_listed(&app, guards, req, next, false).await;
    }
    run_listed(&app, guards, req, next, true).await
}

/// Inside the web stack of a first-party API request: the session's principal, else the listed guards (the CSRF
/// check has passed), else the bearer 401.
async fn stateful_guards(
    app: &App,
    guards: &[Arc<dyn Guard>],
    req: Request,
    next: Next,
) -> Response {
    if req
        .extensions()
        .get::<Principal>()
        .is_some_and(|p| p.guard == WEB_GUARD)
    {
        return next.run(req).await;
    }
    let (mut parts, body) = req.into_parts();
    for guard in guards {
        match guard.authenticate(app, &mut parts).await {
            Ok(Some(principal)) => {
                parts.extensions.insert(principal);
                return next.run(Request::from_parts(parts, body)).await;
            }
            Ok(None) => {}
            Err(e) => return e.into_response(),
        }
    }
    unauthenticated_bearer()
}

async fn run_listed(
    app: &App,
    guards: &[Arc<dyn Guard>],
    req: Request,
    next: Next,
    on_web: bool,
) -> Response {
    let app = app.clone();
    let (mut parts, body) = req.into_parts();
    for guard in guards {
        // A bearer credential never replaces the session (and its CSRF check) on a web route.
        if guard.stateless() && on_web {
            continue;
        }
        match guard.authenticate(&app, &mut parts).await {
            Ok(Some(principal)) => {
                parts.extensions.insert(principal);
                return next.run(Request::from_parts(parts, body)).await;
            }
            Ok(None) => {}
            Err(e) => return e.into_response(),
        }
    }
    let req = Request::from_parts(parts, body);
    // On API routes a list with a stateless guard is a bearer endpoint; on web routes the session is the only
    // credential, so a signed-out browser gets what `auth` answers (the login redirect).
    if !on_web && guards.iter().any(|g| g.stateless()) {
        return unauthenticated_bearer();
    }
    super::require_auth(req, next).await
}

/// The answer of a bearer endpoint without a valid credential: 401 `{"error":"Unauthenticated."}` with
/// `WWW-Authenticate: Bearer` and `Cache-Control: no-store`, what the `auth:` family answers on API routes. Guard
/// crates answer with it so every bearer refusal looks the same.
pub fn unauthenticated_bearer() -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({ "error": "Unauthenticated." })),
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        http::header::WWW_AUTHENTICATE,
        http::HeaderValue::from_static("Bearer"),
    );
    headers.insert(
        http::header::CACHE_CONTROL,
        http::HeaderValue::from_static("no-store"),
    );
    response
}

/// The request's [`Principal`], as a handler argument, on any route: the one an `auth:` middleware or the web
/// stack (a signed-in session) stored, else, on API routes, the first a stateless guard finds. Without one the
/// request answers 401; `Option<Authenticated>` gives `None` instead.
///
/// ```
/// use smeltery_core::auth::Authenticated;
///
/// async fn greet(who: Option<Authenticated>) -> String {
///     match who {
///         Some(who) => format!("hello, user {}", who.user_id),
///         None => "hello, guest".to_owned(),
///     }
/// }
/// # let _ = greet;
/// ```
#[derive(Clone, Debug)]
pub struct Authenticated(Principal);

impl Authenticated {
    /// The principal.
    pub fn into_inner(self) -> Principal {
        self.0
    }
}

impl std::ops::Deref for Authenticated {
    type Target = Principal;

    fn deref(&self) -> &Principal {
        &self.0
    }
}

async fn find_principal(parts: &mut Parts, app: &App) -> Result<Option<Principal>> {
    if let Some(principal) = parts.extensions.get::<Principal>() {
        return Ok(Some(principal.clone()));
    }
    if parts.extensions.get::<Session>().is_some() {
        return Ok(None);
    }
    let found = authenticate(app, parts, GuardSet::Stateless).await?;
    if let Some(principal) = &found {
        parts.extensions.insert(principal.clone());
    }
    Ok(found)
}

impl axum::extract::FromRequestParts<App> for Authenticated {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        find_principal(parts, app)
            .await?
            .map(Self)
            .ok_or_else(Error::unauthorized)
    }
}

impl axum::extract::OptionalFromRequestParts<App> for Authenticated {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut Parts,
        app: &App,
    ) -> std::result::Result<Option<Self>, Self::Rejection> {
        Ok(find_principal(parts, app).await?.map(Self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abilities_are_exact_or_star_and_sessions_hold_every_ability() {
        let session = Principal::new(1, WEB_GUARD, Credential::session("ab"));
        assert!(session.can("anything"));
        assert_eq!(session.key(), "web:session:ab");
        let token = Principal::new(1, "t", Credential::token(9, ["orders:read"]));
        assert!(token.can("orders:read"));
        assert!(!token.can("orders:write"));
        assert!(!token.can("orders"));
        assert!(!token.can("orders:read "));
        assert_eq!(token.key(), "t:token:9");
        // Two guards' tokens with one id have different keys.
        assert_ne!(
            Principal::new(1, "a", Credential::token(9, ["*"])).key(),
            Principal::new(1, "b", Credential::token(9, ["*"])).key()
        );
        assert!(Principal::new(1, "t", Credential::token(9, ["*"])).can("x"));
        assert!(!Principal::new(1, "t", Credential::token::<String>(9, [])).can("x"));
        assert_eq!(token.credential.kind(), CredentialKind::Tokens);
        let debug = format!("{token:?}");
        assert!(
            debug.contains("Tokens") && !debug.contains("orders"),
            "{debug}"
        );
    }

    #[test]
    fn guard_names_are_plain() {
        for good in ["web", "hallmark", "api_v2", "a-b"] {
            assert!(valid_guard_name(good), "{good}");
        }
        for bad in ["", "Web", "a,b", "a:b", "a b", "ä"] {
            assert!(!valid_guard_name(bad), "{bad}");
        }
    }
}
