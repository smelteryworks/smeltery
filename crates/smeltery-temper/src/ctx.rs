//! The request context Temper hands to actions, answers, views and login-pipeline steps.

use smeltery_core::auth::Auth;
use smeltery_core::db::Db;
use smeltery_core::http::{Back, FromRequestParts, HeaderMap, request::Parts};
use smeltery_core::session::Session;
use smeltery_core::validation::{Input, ValidationErrors};
use smeltery_core::{App, Error, Result};

/// One request to a Temper route: the app, the signed-in state ([`Auth`]), the [`Session`] and how the client
/// wants its answers. Actions ([`CreatesNewUsers`](crate::CreatesNewUsers) …), answers
/// ([`TemperResponses`](crate::TemperResponses)) and login-pipeline steps get it.
///
/// It is also a handler argument on web routes (`ctx: TemperCtx`), for app code that calls
/// [`login_pipeline`](crate::login_pipeline) or an action itself.
#[derive(Clone)]
pub struct TemperCtx {
    app: App,
    auth: Auth,
    session: Session,
    headers: HeaderMap,
    back: String,
    home: Option<String>,
}

impl std::fmt::Debug for TemperCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the headers (cookies) or the session's values.
        f.debug_struct("TemperCtx")
            .field("user", &self.auth.id())
            .field("wants_json", &self.wants_json())
            .finish_non_exhaustive()
    }
}

impl TemperCtx {
    /// A context from its parts (for code outside a Temper route, such as a social-login callback). The previous
    /// page ([`back`](Self::back)) is the `Referer` when its host is the `Host` header, else `/`.
    pub fn new(app: App, auth: Auth, session: Session, headers: HeaderMap) -> Self {
        let back = Back::from_headers(&headers).url().to_owned();
        Self {
            app,
            auth,
            session,
            headers,
            back,
            home: None,
        }
    }

    /// The same context with `home` as [`home`](Self::home).
    pub(crate) fn with_home(mut self, home: Option<&str>) -> Self {
        self.home = home.map(str::to_owned);
        self
    }

    /// The app.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The app's database.
    ///
    /// # Errors
    /// The app has no database (`DATABASE_URL` is empty).
    pub fn db(&self) -> Result<Db> {
        self.app.db()
    }

    /// The signed-in state of the request.
    pub fn auth(&self) -> &Auth {
        &self.auth
    }

    /// The session.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The client address after `TRUSTED_PROXIES` (see [`Auth::ip`]).
    pub fn ip(&self) -> &str {
        self.auth.ip()
    }

    /// The request headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Whether the client asked for JSON (`Accept: application/json` or a `+json` type): such a client gets JSON
    /// answers instead of redirects. An Inertia visit is not a JSON client (it gets the browser answers).
    pub fn wants_json(&self) -> bool {
        smeltery_core::http::wants_json(&self.headers)
    }

    /// Whether the request is an Inertia visit (`X-Inertia: true`).
    pub fn is_inertia(&self) -> bool {
        smeltery_core::http::is_inertia(&self.headers)
    }

    /// Where a signed-in user goes: `Temper::home` when it is set, else `AUTH_HOME`.
    pub fn home(&self) -> &str {
        self.home
            .as_deref()
            .unwrap_or(&self.app.settings().auth_home)
    }

    /// The previous page: the `Referer` when it is on this site, else `/`.
    pub fn back(&self) -> &str {
        &self.back
    }

    /// The path of the route named `name`, else `fallback` (a route left out with
    /// [`without_route`](crate::Temper::without_route) or by `.views(false)`).
    pub fn route_or(&self, name: &str, fallback: &str) -> String {
        self.app
            .url(name, &[])
            .unwrap_or_else(|_| fallback.to_owned())
    }
}

impl FromRequestParts<App> for TemperCtx {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        let auth = Auth::from_request_parts(parts, app).await?;
        let session = Session::from_request_parts(parts, app).await?;
        let back = Back::from_request_parts(parts, app)
            .await
            .map_or_else(|_| "/".to_owned(), |b| b.url().to_owned());
        Ok(Self {
            app: app.clone(),
            auth,
            session,
            headers: parts.headers.clone(),
            back,
            home: None,
        })
    }
}

/// The context of a view: the request ([`TemperCtx`]) and, on the reset-password page, the token and address from
/// the link.
#[derive(Clone, Debug)]
pub struct ViewCtx {
    ctx: TemperCtx,
    token: Option<String>,
    email: Option<String>,
}

impl ViewCtx {
    pub(crate) fn new(ctx: TemperCtx) -> Self {
        Self {
            ctx,
            token: None,
            email: None,
        }
    }

    /// The token is kept only when it has the shape of a reset token (see [`is_reset_token`]): a page that puts it
    /// into a form's `action` can then never be steered to another path (`abc%2F..%2Flogin`).
    pub(crate) fn with_link(mut self, token: String, email: Option<String>) -> Self {
        self.token = is_reset_token(&token).then_some(token);
        self.email = email;
        self
    }

    /// The app.
    pub fn app(&self) -> &App {
        self.ctx.app()
    }

    /// The signed-in state of the request.
    pub fn auth(&self) -> &Auth {
        self.ctx.auth()
    }

    /// The session.
    pub fn session(&self) -> &Session {
        self.ctx.session()
    }

    /// The whole request context.
    pub fn ctx(&self) -> &TemperCtx {
        &self.ctx
    }

    /// The reset token from the link (the reset-password page; empty on the other pages). Only a value with the
    /// shape of a reset token (64 ASCII letters and digits) gets here; it is still untrusted (render it escaped, as
    /// Mold's `{{ }}` and React / Vue do).
    pub fn token(&self) -> String {
        self.token.clone().unwrap_or_default()
    }

    /// The address from the link's `?email=` (the reset-password page; empty when the link has none). It is taken
    /// from the URL as sent, so it is untrusted: render it escaped (Mold's `{{ }}` and React / Vue do), never into
    /// raw HTML.
    pub fn email(&self) -> String {
        self.email.clone().unwrap_or_default()
    }

    /// The `status` message the previous request flashed (a reset link was sent, …).
    pub fn status(&self) -> Option<String> {
        self.ctx.session().get::<String>("status")
    }
}

/// Whether `token` has the shape of core's reset tokens: 64 ASCII letters and digits (nothing that could change a
/// path when it is put into a URL).
pub(crate) fn is_reset_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Flash one validation message and the old input (core's `Session::flash_errors`: fields the app never flashes
/// stay out), for an answer that redirects somewhere other than back (the failed login goes to the login page even
/// without a `Referer`).
pub(crate) fn flash_invalid(ctx: &TemperCtx, field: &str, message: &str, old: &[(&str, &str)]) {
    let mut errors = ValidationErrors::new();
    errors.add(field, message);
    let old: Input = old
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    ctx.session().flash_errors(ctx.app(), &errors, &old);
}
