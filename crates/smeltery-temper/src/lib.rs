#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod actions;
mod ctx;
mod error;
mod events;
mod forms;
pub mod messages;
mod pipeline;
mod responses;
mod routes;
pub mod testing;
pub mod two_factor;
mod views;

use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use smeltery_core::auth::{Authenticatable, MustVerifyEmail};
use smeltery_core::db::{PrimaryKeyOf, Record};
use smeltery_core::routing::Router;
use smeltery_core::{App, AppBuilder, Result};

pub use actions::{
    CreatesNewUsers, EmailChanged, PasswordInput, ResetsUserPasswords, SocialUser,
    UpdatePasswordInput, UpdatesUserPasswords, UpdatesUserProfileInformation,
};
pub use ctx::{TemperCtx, ViewCtx};
pub use error::TemperError;
pub use events::TemperEvent;
pub use pipeline::{PipelineStep, login_pipeline};
pub use responses::{DefaultResponses, TemperResponses};
pub use two_factor::{TwoFactor, TwoFactorAuthenticatable, TwoFactorSetup, TwoFactorStatus};
pub use views::{TemperViews, ViewSetting};

use events::Listener;
use pipeline::Hook;
use routes::Registrar;
use views::ViewFn;

/// Registers the routes of one feature.
type FeatureFn<U> = Box<dyn FnOnce(&mut Registrar<'_, U>) + Send>;

/// The throttles on Temper's form routes (`throttle:<max>,<minutes>` values; an invalid value stops the app at
/// boot, like any `throttle:` alias).
///
/// ```
/// use smeltery::temper::Limits;
///
/// let limits = Limits::new().login("10,1").forms("3,1");
/// # let _ = limits;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub(crate) login: String,
    pub(crate) forms: String,
}

impl Default for Limits {
    fn default() -> Self {
        Self::new()
    }
}

impl Limits {
    /// The defaults: `30,1` on `POST /login`, `6,1` on every other form route.
    pub fn new() -> Self {
        Self {
            login: "30,1".to_owned(),
            forms: "6,1".to_owned(),
        }
    }

    /// The throttle of `POST /login` (default `30,1`: thirty a minute per client).
    #[must_use]
    pub fn login(mut self, limit: impl Into<String>) -> Self {
        self.login = limit.into();
        self
    }

    /// The throttle of every other form route: registration, the reset forms, the verification link, password
    /// confirmation, profile and password updates (default `6,1`).
    #[must_use]
    pub fn forms(mut self, limit: impl Into<String>) -> Self {
        self.forms = limit.into();
        self
    }
}

/// What every handler of one app's Temper shares.
pub(crate) struct Shared<U> {
    pub(crate) home: Option<String>,
    pub(crate) views: Option<TemperViews>,
    pub(crate) responses: Arc<dyn TemperResponses>,
    pub(crate) listeners: Vec<Listener>,
    pub(crate) hooks: Vec<Hook<U>>,
    pub(crate) email_verification: bool,
    /// Whether Temper's password-reset routes are on (an address change then deletes the pending reset token).
    pub(crate) reset_passwords: bool,
    /// Whether a user must give a second factor (set by `two_factor`).
    pub(crate) second_step: Option<Required>,
}

/// Whether the user with this id must give a second factor, read from their row now.
type Required =
    Arc<dyn Fn(App, i64) -> smeltery_core::BoxFuture<'static, Result<bool>> + Send + Sync>;

/// What `Temper::two_factor` adds.
struct TwoFactorFeature<U> {
    options: TwoFactor,
    required: Required,
    routes: FeatureFn<U>,
    install: Box<dyn FnOnce(AppBuilder) -> AppBuilder + Send>,
}

impl<U> Shared<U> {
    /// The request context with the configured home page.
    pub(crate) fn ctx(&self, ctx: TemperCtx) -> TemperCtx {
        ctx.with_home(self.home.as_deref())
    }

    /// One page's view.
    pub(crate) fn view(&self, pick: impl Fn(&TemperViews) -> Option<&ViewFn>) -> Option<&ViewFn> {
        self.views.as_ref().and_then(pick)
    }
}

/// Temper's set-up for the user model `U`: which features are on, the app's actions, views and answers. Installed
/// with [`TemperExt::temper`].
///
/// ```
/// # mod user {
/// #     use smeltery::db::prelude::*;
/// #     #[sea_orm::model]
/// #     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
/// #     #[sea_orm(table_name = "users")]
/// #     pub struct Model {
/// #         #[sea_orm(primary_key)]
/// #         pub id: i64,
/// #         pub email: String,
/// #         pub email_verified_at: Option<DateTimeUtc>,
/// #         pub password: String,
/// #         pub remember_token: Option<String>,
/// #     }
/// #     impl ActiveModelBehavior for ActiveModel {}
/// #     impl smeltery::auth::Authenticatable for Model {
/// #         fn auth_id(&self) -> i64 { self.id }
/// #         fn password_hash(&self) -> &str { &self.password }
/// #         fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
/// #     }
/// #     impl smeltery::auth::MustVerifyEmail for Model {
/// #         fn email(&self) -> &str { &self.email }
/// #         fn email_verified_at(&self) -> Option<DateTimeUtc> { self.email_verified_at }
/// #     }
/// # }
/// # use user::Model as User;
/// use smeltery::http::Html;
/// use smeltery::temper::{Temper, TemperExt as _, TemperViews};
///
/// fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
///     app.temper(
///         Temper::<User>::new()
///             .email_verification()
///             .views(
///                 TemperViews::new()
///                     .login(|_| Html("login"))
///                     .confirm_password(|_| Html("confirm your password"))
///                     .verify_email(|_| Html("check your inbox")),
///             ),
///     )
/// }
/// # let _ = build;
/// ```
pub struct Temper<U> {
    registration: Option<FeatureFn<U>>,
    reset_passwords: Option<FeatureFn<U>>,
    email_verification: bool,
    profile: Option<FeatureFn<U>>,
    passwords: Option<FeatureFn<U>>,
    two_factor: Option<TwoFactorFeature<U>>,
    home: Option<String>,
    prefix: String,
    routes: bool,
    without: Vec<String>,
    views: ViewSetting,
    responses: Arc<dyn TemperResponses>,
    listeners: Vec<Listener>,
    hooks: Vec<Hook<U>>,
    policies: Vec<pipeline::Policy<U>>,
    limits: Limits,
    _user: PhantomData<fn() -> U>,
}

impl<U> std::fmt::Debug for Temper<U> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Temper")
            .field("registration", &self.registration.is_some())
            .field("reset_passwords", &self.reset_passwords.is_some())
            .field("email_verification", &self.email_verification)
            .field("update_profile_information", &self.profile.is_some())
            .field("update_passwords", &self.passwords.is_some())
            .field("two_factor", &self.two_factor.as_ref().map(|t| t.options))
            .field("home", &self.home)
            .field("prefix", &self.prefix)
            .field("routes", &self.routes)
            .field("without", &self.without)
            .field("views", &self.views)
            .field("listeners", &self.listeners.len())
            .field("login_pipeline", &self.hooks.len())
            .field("login_policies", &self.policies.len())
            .field("limits", &self.limits)
            .finish()
    }
}

impl<U: Authenticatable + Record> Default for Temper<U> {
    fn default() -> Self {
        Self::new()
    }
}

impl<U: Authenticatable + Record> Temper<U> {
    /// Login, logout and password confirmation; every other feature off; no views yet; the default answers.
    pub fn new() -> Self {
        Self {
            registration: None,
            reset_passwords: None,
            email_verification: false,
            profile: None,
            passwords: None,
            two_factor: None,
            home: None,
            prefix: String::new(),
            routes: true,
            without: Vec::new(),
            views: ViewSetting::Pages(TemperViews::new()),
            responses: Arc::new(DefaultResponses),
            listeners: Vec::new(),
            hooks: Vec::new(),
            policies: Vec::new(),
            limits: Limits::new(),
            _user: PhantomData,
        }
    }

    /// Registration: `GET /register` (`register`) and `POST /register` (`register.store`); `action` creates the
    /// user.
    #[must_use]
    pub fn registration<A: CreatesNewUsers<U>>(mut self, action: A) -> Self {
        let action = Arc::new(action);
        self.registration = Some(Box::new(move |reg| routes::registration(reg, action)));
        self
    }

    /// Password resets: `GET` / `POST /forgot-password` (`password.request`, `password.email`) and `GET` /
    /// `POST /reset-password/{token}` (`password.reset`, `password.update`).
    #[must_use]
    pub fn reset_passwords<A: ResetsUserPasswords<U>>(mut self, action: A) -> Self {
        let action = Arc::new(action);
        self.reset_passwords = Some(Box::new(move |reg| routes::reset_passwords(reg, action)));
        self
    }

    /// Profile updates: `PUT /user/profile-information` (`user-profile-information.update`), behind `password.confirm`
    /// (the address decides where reset links go). An address change deletes the user's pending reset link.
    #[must_use]
    pub fn update_profile_information<A: UpdatesUserProfileInformation<U>>(
        mut self,
        action: A,
    ) -> Self {
        let action = Arc::new(action);
        self.profile = Some(Box::new(move |reg| routes::update_profile(reg, action)));
        self
    }

    /// Password updates: `PUT /user/password` (`user-password.update`).
    #[must_use]
    pub fn update_passwords<A: UpdatesUserPasswords<U>>(mut self, action: A) -> Self {
        let action = Arc::new(action);
        self.passwords = Some(Box::new(move |reg| routes::update_passwords(reg, action)));
        self
    }

    /// Where signed-in users go after logging in, registering, confirming their password or verifying their
    /// address (default: `AUTH_HOME`).
    #[must_use]
    pub fn home(mut self, path: impl Into<String>) -> Self {
        self.home = Some(path.into());
        self
    }

    /// Put every Temper route under `prefix` (`/auth` → `/auth/login`); route names stay the same.
    #[must_use]
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// `false`: register no routes (the app declares its own and uses Temper's actions, answers and
    /// [`login_pipeline`]).
    #[must_use]
    pub fn routes(mut self, routes: bool) -> Self {
        self.routes = routes;
        self
    }

    /// Leave out the route named `name` (the app declares its own route there). An unknown name stops the app at
    /// boot.
    #[must_use]
    pub fn without_route(mut self, name: impl Into<String>) -> Self {
        self.without.push(name.into());
        self
    }

    /// The pages ([`TemperViews`]), or `false` for an app without pages. Without pages, clients should send
    /// `Accept: application/json`: a browser request gets the browser answers, which redirect to page routes that
    /// do not exist.
    #[must_use]
    pub fn views(mut self, views: impl Into<ViewSetting>) -> Self {
        self.views = views.into();
        self
    }

    /// Answer the outcomes the app's way ([`TemperResponses`]).
    #[must_use]
    pub fn responses(mut self, responses: impl TemperResponses) -> Self {
        self.responses = Arc::new(responses);
        self
    }

    /// Run `listener` for every [`TemperEvent`], after the answer is decided (it never changes or delays the
    /// answer; under `APP_ENV=testing` it runs before the answer returns). An error or a panic is logged at `warn`.
    #[must_use]
    pub fn listen<F, Fut>(mut self, listener: F) -> Self
    where
        F: Fn(App, TemperEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.listeners
            .push(Arc::new(move |app, event| Box::pin(listener(app, event))));
        self
    }

    /// The throttles of the form routes.
    #[must_use]
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Add a step to the login pipeline (run in order, after the password check or a social login, before the
    /// sign-in): `PipelineStep::Respond(response)` stops the login with that answer, an error fails it.
    ///
    /// Steps gate new sign-ins only: a session that is already signed in and a sign-in restored from a remember-me
    /// cookie never meet them. To lock an account out, also end its credentials (`auth::password_changed`) or check
    /// the user in a middleware.
    ///
    /// ```
    /// # mod user {
    /// #     use smeltery::db::prelude::*;
    /// #     #[sea_orm::model]
    /// #     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    /// #     #[sea_orm(table_name = "users")]
    /// #     pub struct Model {
    /// #         #[sea_orm(primary_key)]
    /// #         pub id: i64,
    /// #         pub banned: bool,
    /// #         pub password: String,
    /// #         pub remember_token: Option<String>,
    /// #     }
    /// #     impl ActiveModelBehavior for ActiveModel {}
    /// #     impl smeltery::auth::Authenticatable for Model {
    /// #         fn auth_id(&self) -> i64 { self.id }
    /// #         fn password_hash(&self) -> &str { &self.password }
    /// #         fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
    /// #     }
    /// # }
    /// # use user::Model as User;
    /// use smeltery::Error;
    /// use smeltery::temper::{PipelineStep, Temper};
    ///
    /// let temper = Temper::<User>::new().login_pipeline(|_ctx, user: User| async move {
    ///     if user.banned {
    ///         return Err(Error::validation("email", "This account is closed."));
    ///     }
    ///     Ok(PipelineStep::Continue)
    /// });
    /// # let _ = temper;
    /// ```
    #[must_use]
    pub fn login_pipeline<F, Fut>(mut self, step: F) -> Self
    where
        F: Fn(TemperCtx, U) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<PipelineStep>> + Send + 'static,
    {
        self.hooks.push(pipeline::hook(step));
        self
    }

    /// Add a session-free login rule, registered as core's [`LoginPolicy`](smeltery_core::auth::LoginPolicy):
    /// "may this user sign in now?". Unlike [`login_pipeline`](Self::login_pipeline) steps (web sign-ins only), it
    /// gates every way in: Temper's `POST /login` (before the steps and the second factor), the two-factor challenge
    /// (asked again before the code), registration (after `create`: a refusal keeps the account, fires `Registered`
    /// and signs nobody in), a social login through core's `LoginCompletion`, a remember-me cookie, core's
    /// `Auth::attempt`, and a token endpoint that asks `app.check_login(…)`. A refusal answers its status and message
    /// (JSON clients) or sends the browser back with the message on `email` (from the challenge: to the login page);
    /// nobody is signed in. Rules that must hold for API tokens too (a suspended account) belong here. A policy gates
    /// new sign-ins, token issuance and remember-me restores only; to cut what the user already holds, call
    /// `auth::end_credentials` (open sessions through the user's `credentials_epoch` column, the remember token, API
    /// tokens, sockets).
    ///
    /// ```
    /// # mod user {
    /// #     use smeltery::db::prelude::*;
    /// #     #[sea_orm::model]
    /// #     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    /// #     #[sea_orm(table_name = "users")]
    /// #     pub struct Model {
    /// #         #[sea_orm(primary_key)]
    /// #         pub id: i64,
    /// #         pub suspended: bool,
    /// #         pub password: String,
    /// #         pub remember_token: Option<String>,
    /// #     }
    /// #     impl ActiveModelBehavior for ActiveModel {}
    /// #     impl smeltery::auth::Authenticatable for Model {
    /// #         fn auth_id(&self) -> i64 { self.id }
    /// #         fn password_hash(&self) -> &str { &self.password }
    /// #         fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
    /// #     }
    /// # }
    /// # use user::Model as User;
    /// use smeltery::auth::LoginDecision;
    /// use smeltery::http::StatusCode;
    /// use smeltery::temper::Temper;
    ///
    /// let temper = Temper::<User>::new().login_policy(|_app, user: User| async move {
    ///     Ok(if user.suspended {
    ///         LoginDecision::refuse(StatusCode::FORBIDDEN, "This account is suspended.")
    ///     } else {
    ///         LoginDecision::Allow
    ///     })
    /// });
    /// # let _ = temper;
    /// ```
    #[must_use]
    pub fn login_policy<F, Fut>(mut self, check: F) -> Self
    where
        F: Fn(App, U) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<smeltery_core::auth::LoginDecision>> + Send + 'static,
    {
        self.policies.push(pipeline::policy(check));
        self
    }
}

impl<U: Authenticatable + Record + MustVerifyEmail> Temper<U> {
    /// E-mail verification: `GET /email/verify` (`verification.notice`), `GET /email/verify/{id}/{hash}`
    /// (`verification.verify`) and `POST /email/verification-notification` (`verification.send`), and the link
    /// after registration and after an address change. Requiring verified addresses stays core's
    /// `.verify_email::<User>()` (the `verified` middleware); without it every signed-in user counts as verified.
    #[must_use]
    pub fn email_verification(mut self) -> Self {
        self.email_verification = true;
        self
    }
}

impl<U: TwoFactorAuthenticatable> Temper<U> {
    /// Two-factor authentication: the challenge after the password (`GET` / `POST /two-factor-challenge`,
    /// `two-factor.login`, `two-factor.login.store`) and the management routes (enable, confirm, disable, the QR
    /// code, the secret key, the recovery codes; behind `password.confirm`; with `confirm_password(false)` only a first
    /// enrolment goes without a recent password confirmation). Also
    /// registers core's `SecondFactor` (other sign-in endpoints ask it), a credential listener that removes an
    /// enrolment when a reset verifies a previously unverified address, and the console command
    /// `temper:two-factor-disable <email> --force`.
    #[must_use]
    pub fn two_factor(mut self, options: TwoFactor) -> Self {
        self.two_factor = Some(TwoFactorFeature {
            options,
            required: Arc::new(move |app: App, id: i64| {
                Box::pin(async move {
                    // A fresh row: a user value read before two-factor was enabled must not skip it.
                    let user = two_factor::fresh::<U>(&app, id).await?.ok_or_else(|| {
                        smeltery_core::Error::internal("the user signing in is gone")
                    })?;
                    Ok(options.required(&user))
                })
            }),
            routes: Box::new(move |reg| two_factor::routes::register(reg, options)),
            install: Box::new(move |app| {
                app.second_factor(two_factor::Factor::<U> {
                    options,
                    _user: PhantomData,
                })
                .credential_listener(two_factor::ResetListener::<U>(PhantomData))
                .commands(|c| {
                    c.add(two_factor::DisableCommand::<U>::new());
                })
            }),
        });
        self
    }
}

/// Installs Temper on an app: `use smeltery::temper::TemperExt as _;` then `app.temper(Temper::new()…)`.
pub trait TemperExt: Sized {
    /// Register `U` as the user model (`.auth::<U>()`), Temper's routes (unless `routes(false)`), the login
    /// pipeline as core's `LoginCompletion` and `dont_flash(["code"])`. The app stops at boot with a [`TemperError`]
    /// when a page route has no view, `without_route` names an unknown route or the prefix is invalid; core stops
    /// it when `.auth::<…>()` is called with another model, before or after `.temper(…)`.
    fn temper<U>(self, temper: Temper<U>) -> Self
    where
        U: Record
            + Authenticatable
            + sea_orm::FromQueryResult
            + sea_orm::ModelTrait<Entity = <U as Record>::Entity>,
        PrimaryKeyOf<U>: From<i64>;
}

impl TemperExt for AppBuilder {
    fn temper<U>(self, temper: Temper<U>) -> Self
    where
        U: Record
            + Authenticatable
            + sea_orm::FromQueryResult
            + sea_orm::ModelTrait<Entity = <U as Record>::Entity>,
        PrimaryKeyOf<U>: From<i64>,
    {
        let Temper {
            registration,
            reset_passwords,
            email_verification,
            profile,
            passwords,
            two_factor,
            home,
            prefix,
            routes,
            without,
            views,
            responses,
            listeners,
            hooks,
            policies,
            limits,
            _user,
        } = temper;
        let views = match views {
            ViewSetting::Pages(views) => Some(views),
            ViewSetting::None => None,
        };
        let (two_factor_options, second_step, two_factor_routes, install) = match two_factor {
            Some(t) => (
                Some(t.options),
                Some(t.required),
                Some(t.routes),
                Some(t.install),
            ),
            None => (None, None, None, None),
        };
        let problem = setup_problem(
            &prefix,
            &without,
            routes.then_some(()).and(views.as_ref()),
            Features {
                two_factor: two_factor_options,
                registration: registration.is_some(),
                reset_passwords: reset_passwords.is_some(),
                email_verification,
            },
        );
        let pages = views.is_some();
        let has_resets = reset_passwords.is_some();
        let shared = Arc::new(Shared {
            home,
            views,
            responses,
            listeners,
            hooks,
            email_verification,
            reset_passwords: has_resets,
            second_step,
        });
        let mut app = self
            .auth::<U>()
            // A field named `code` (one-time codes) is never flashed back as old input.
            .dont_flash(&["code"])
            .service(Arc::clone(&shared))
            .login_completion(pipeline::Completion(Arc::clone(&shared)));
        for policy in policies {
            app = app.login_policy(policy);
        }
        if let Some(install) = install {
            app = install(app);
        }
        if routes {
            app = app.routes(move |r| {
                let add = move |router: &mut Router| {
                    let mut reg = Registrar {
                        router,
                        shared,
                        limits,
                        without,
                        pages,
                    };
                    routes::core(&mut reg);
                    for feature in [
                        registration,
                        reset_passwords,
                        profile,
                        passwords,
                        two_factor_routes,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        feature(&mut reg);
                    }
                    if email_verification {
                        routes::email_verification(&mut reg);
                    }
                };
                if prefix.is_empty() {
                    add(r);
                } else {
                    r.group(&prefix, add);
                }
            });
        }
        app.on_boot(move |app| async move {
            let _ = app;
            problem.map_or(Ok(()), |problem| Err(problem.into()))
        })
    }
}

/// The first set-up mistake, if any.
/// Which features are on (for the set-up checks).
struct Features {
    two_factor: Option<TwoFactor>,
    registration: bool,
    reset_passwords: bool,
    email_verification: bool,
}

/// `views`: the pages when routes are registered with pages.
fn setup_problem(
    prefix: &str,
    without: &[String],
    views: Option<&TemperViews>,
    features: Features,
) -> Option<TemperError> {
    let Features {
        two_factor,
        registration,
        reset_passwords,
        email_verification,
    } = features;
    if !prefix.is_empty()
        && (!prefix.starts_with('/') || prefix.ends_with('/') || prefix.contains(['{', '}']))
    {
        return Some(TemperError::InvalidPrefix(prefix.to_owned()));
    }
    if let Some(problem) = two_factor.and_then(|t| t.problem()) {
        return Some(TemperError::InvalidTwoFactor(problem));
    }
    let known = || {
        routes::ROUTE_NAMES
            .iter()
            .chain(two_factor::routes::ROUTE_NAMES)
    };
    if let Some(name) = without.iter().find(|name| !known().any(|k| k == name)) {
        return Some(TemperError::UnknownRoute {
            name: name.clone(),
            known: known().copied().collect::<Vec<_>>().join(", "),
        });
    }
    let views = views?;
    let pages: [(bool, &'static str, &'static str, bool); 7] = [
        (
            two_factor.is_some(),
            "two_factor_challenge",
            "two-factor.login",
            views.two_factor_challenge.is_some(),
        ),
        (true, "login", "login", views.login.is_some()),
        (
            true,
            "confirm_password",
            "password.confirm",
            views.confirm_password.is_some(),
        ),
        (
            registration,
            "register",
            "register",
            views.register.is_some(),
        ),
        (
            reset_passwords,
            "forgot_password",
            "password.request",
            views.forgot_password.is_some(),
        ),
        (
            reset_passwords,
            "reset_password",
            "password.reset",
            views.reset_password.is_some(),
        ),
        (
            email_verification,
            "verify_email",
            "verification.notice",
            views.verify_email.is_some(),
        ),
    ];
    pages
        .into_iter()
        .find(|(on, _, route, given)| *on && !given && !without.iter().any(|w| w == route))
        .map(|(_, page, route, _)| TemperError::MissingView { page, route })
}
