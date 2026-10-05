//! The pages: one closure per page, returning anything that is a response (a `#[derive(Mold)]` struct, an Alloy
//! page, plain HTML). Temper never renders a page itself.

use std::sync::Arc;

use smeltery_core::Response;
use smeltery_core::http::IntoResponse;

use crate::ViewCtx;

pub(crate) type ViewFn = Arc<dyn Fn(ViewCtx) -> Response + Send + Sync>;

/// The app's pages for Temper's `GET` routes.
///
/// ```
/// use smeltery::temper::{TemperViews, ViewCtx};
/// use smeltery::http::Html;
///
/// fn views() -> TemperViews {
///     TemperViews::new()
///         .login(|_| Html("<form method=post action=/login>…</form>"))
///         .reset_password(|ctx: ViewCtx| Html(format!("reset with token {}", ctx.token().len())))
/// }
/// # let _ = views;
/// ```
#[derive(Clone, Default)]
pub struct TemperViews {
    pub(crate) login: Option<ViewFn>,
    pub(crate) register: Option<ViewFn>,
    pub(crate) forgot_password: Option<ViewFn>,
    pub(crate) reset_password: Option<ViewFn>,
    pub(crate) verify_email: Option<ViewFn>,
    pub(crate) confirm_password: Option<ViewFn>,
    pub(crate) two_factor_challenge: Option<ViewFn>,
}

impl std::fmt::Debug for TemperViews {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemperViews")
            .field("login", &self.login.is_some())
            .field("register", &self.register.is_some())
            .field("forgot_password", &self.forgot_password.is_some())
            .field("reset_password", &self.reset_password.is_some())
            .field("verify_email", &self.verify_email.is_some())
            .field("confirm_password", &self.confirm_password.is_some())
            .field("two_factor_challenge", &self.two_factor_challenge.is_some())
            .finish()
    }
}

fn erase<F, R>(view: F) -> ViewFn
where
    F: Fn(ViewCtx) -> R + Send + Sync + 'static,
    R: IntoResponse,
{
    Arc::new(move |ctx| view(ctx).into_response())
}

impl TemperViews {
    /// No pages yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The login page (`GET /login`, route `login`).
    #[must_use]
    pub fn login<F, R>(mut self, view: F) -> Self
    where
        F: Fn(ViewCtx) -> R + Send + Sync + 'static,
        R: IntoResponse,
    {
        self.login = Some(erase(view));
        self
    }

    /// The registration page (`GET /register`, route `register`).
    #[must_use]
    pub fn register<F, R>(mut self, view: F) -> Self
    where
        F: Fn(ViewCtx) -> R + Send + Sync + 'static,
        R: IntoResponse,
    {
        self.register = Some(erase(view));
        self
    }

    /// The "forgot your password?" page (`GET /forgot-password`, route `password.request`).
    #[must_use]
    pub fn forgot_password<F, R>(mut self, view: F) -> Self
    where
        F: Fn(ViewCtx) -> R + Send + Sync + 'static,
        R: IntoResponse,
    {
        self.forgot_password = Some(erase(view));
        self
    }

    /// The reset-password page (`GET /reset-password/{token}`, route `password.reset`); the context holds the
    /// token ([`ViewCtx::token`]) and the link's address ([`ViewCtx::email`]).
    #[must_use]
    pub fn reset_password<F, R>(mut self, view: F) -> Self
    where
        F: Fn(ViewCtx) -> R + Send + Sync + 'static,
        R: IntoResponse,
    {
        self.reset_password = Some(erase(view));
        self
    }

    /// The "check your inbox" page (`GET /email/verify`, route `verification.notice`). A user who needs no
    /// verification is sent to the home page instead.
    #[must_use]
    pub fn verify_email<F, R>(mut self, view: F) -> Self
    where
        F: Fn(ViewCtx) -> R + Send + Sync + 'static,
        R: IntoResponse,
    {
        self.verify_email = Some(erase(view));
        self
    }

    /// The password confirmation page (`GET /user/confirm-password`, route `password.confirm`).
    #[must_use]
    pub fn confirm_password<F, R>(mut self, view: F) -> Self
    where
        F: Fn(ViewCtx) -> R + Send + Sync + 'static,
        R: IntoResponse,
    {
        self.confirm_password = Some(erase(view));
        self
    }

    /// The two-factor challenge page (`GET /two-factor-challenge`, route `two-factor.login`): a form posting `code`
    /// (or `recovery_code`) to the same path. Shown only while a login waits for its second factor.
    #[must_use]
    pub fn two_factor_challenge<F, R>(mut self, view: F) -> Self
    where
        F: Fn(ViewCtx) -> R + Send + Sync + 'static,
        R: IntoResponse,
    {
        self.two_factor_challenge = Some(erase(view));
        self
    }
}

/// What [`Temper::views`](crate::Temper::views) takes: pages ([`TemperViews`]), or `false` for an app without
/// pages (a single-page or mobile client): then the `GET` page routes are not registered and every answer is JSON
/// to JSON clients.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ViewSetting {
    /// These pages.
    Pages(TemperViews),
    /// No pages.
    None,
}

impl From<TemperViews> for ViewSetting {
    fn from(views: TemperViews) -> Self {
        Self::Pages(views)
    }
}

impl From<bool> for ViewSetting {
    /// `false`: no pages; `true`: pages, none given yet (each page route then needs its view).
    fn from(pages: bool) -> Self {
        if pages {
            Self::Pages(TemperViews::new())
        } else {
            Self::None
        }
    }
}
