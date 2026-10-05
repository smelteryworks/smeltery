//! Temper's routes and their handlers.

use std::sync::Arc;

use smeltery_core::auth::{Authenticatable, EmailVerificationRequest, normalize_email, passwords};
use smeltery_core::http::{IntoResponse, Json, Path, Query, Redirect, StatusCode};
use smeltery_core::routing::{Route, Router};
use smeltery_core::validation::{Valid, ValidationErrors};
use smeltery_core::{App, Error, Response, Result};

use crate::actions::{
    CreatesNewUsers, EmailChanged, PasswordInput, ResetsUserPasswords, UpdatePasswordInput,
    UpdatesUserPasswords, UpdatesUserProfileInformation,
};
use crate::events::{TemperEvent, fire};
use crate::forms::{ConfirmForm, EmailForm, LoginForm, ResetQuery};
use crate::views::ViewFn;
use crate::{Limits, Shared, TemperCtx, ViewCtx, messages, pipeline};

/// Every route name Temper registers, for `without_route`.
pub(crate) const ROUTE_NAMES: &[&str] = &[
    "login",
    "login.store",
    "logout",
    "register",
    "register.store",
    "password.request",
    "password.email",
    "password.reset",
    "password.update",
    "verification.notice",
    "verification.verify",
    "verification.send",
    "user-profile-information.update",
    "user-password.update",
    "password.confirm",
    "password.confirm.store",
    "password.confirmation",
];

/// Adds Temper's routes to a router: skips the left-out names and the pages of an app without views.
pub(crate) struct Registrar<'r, U> {
    pub(crate) router: &'r mut Router,
    pub(crate) shared: Arc<Shared<U>>,
    pub(crate) limits: Limits,
    pub(crate) without: Vec<String>,
    pub(crate) pages: bool,
}

impl<U> Registrar<'_, U> {
    pub(crate) fn on(&self, name: &str) -> bool {
        !self.without.iter().any(|w| w == name)
    }

    pub(crate) fn page_on(&self, name: &str) -> bool {
        self.pages && self.on(name)
    }

    pub(crate) fn login_limit(&self) -> String {
        format!("throttle:{}", self.limits.login)
    }

    pub(crate) fn form_limit(&self) -> String {
        format!("throttle:{}", self.limits.forms)
    }
}

/// Name the route and add its middleware, in order.
pub(crate) fn finish(route: &mut Route, name: &str, middleware: &[&str]) {
    route.name(name);
    for alias in middleware {
        route.middleware(*alias);
    }
}

fn model_mismatch() -> Error {
    Error::internal("the signed-in user is not Temper's user model")
}

/// The page's view, or a 500 when it is missing (the boot check makes that impossible for registered pages).
pub(crate) fn page(view: Option<&ViewFn>, ctx: ViewCtx) -> Result<Response> {
    let view = view.ok_or_else(|| Error::internal("a Temper page has no view"))?;
    Ok(view(ctx))
}

/// The HMAC (under an `APP_KEY`-derived key of the purpose `temper.email-hash`) of the normalized address: stable
/// within one app for correlating events, and not a plain hash a list of addresses could be matched against.
fn address_hash(app: &App, email: &str) -> Result<String> {
    app.sign("temper.email-hash", normalize_email(email).as_bytes())
}

/// Whether a verification link can be sent: Temper's verification routes, or the app's own `verification.verify`.
fn sends_links<U>(shared: &Shared<U>, app: &App) -> bool {
    shared.email_verification
        || app
            .url("verification.verify", &[("id", "0"), ("hash", "0")])
            .is_ok()
}

// ---- always: login, logout, password confirmation ------------------------------------------------------------

pub(crate) fn core<U: Authenticatable>(reg: &mut Registrar<'_, U>) {
    let login_limit = reg.login_limit();
    let form_limit = reg.form_limit();
    if reg.page_on("login") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.get("/login", move |ctx: TemperCtx| {
            let shared = Arc::clone(&shared);
            async move { page(shared.view(|v| v.login.as_ref()), ViewCtx::new(ctx)) }
        });
        finish(route, "login", &["guest"]);
    }
    if reg.on("login.store") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.post(
            "/login",
            move |ctx: TemperCtx, Valid(form): Valid<LoginForm>| {
                let shared = Arc::clone(&shared);
                async move { login(shared, ctx, form).await }
            },
        );
        finish(route, "login.store", &["guest", &login_limit]);
    }
    if reg.on("logout") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.post("/logout", move |ctx: TemperCtx| {
            let shared = Arc::clone(&shared);
            async move { logout(shared, ctx).await }
        });
        finish(route, "logout", &["auth"]);
    }
    if reg.page_on("password.confirm") {
        let shared = Arc::clone(&reg.shared);
        let route = reg
            .router
            .get("/user/confirm-password", move |ctx: TemperCtx| {
                let shared = Arc::clone(&shared);
                async move {
                    page(
                        shared.view(|v| v.confirm_password.as_ref()),
                        ViewCtx::new(ctx),
                    )
                }
            });
        finish(route, "password.confirm", &["auth"]);
    }
    if reg.on("password.confirm.store") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.post(
            "/user/confirm-password",
            move |ctx: TemperCtx, Valid(form): Valid<ConfirmForm>| {
                let shared = Arc::clone(&shared);
                async move { confirm_password(shared, ctx, form).await }
            },
        );
        finish(route, "password.confirm.store", &["auth", &form_limit]);
    }
    if reg.on("password.confirmation") {
        let route = reg.router.get(
            "/user/confirmed-password-status",
            |ctx: TemperCtx| async move {
                let timeout = ctx.app().settings().password_timeout;
                let confirmed = ctx.auth().password_confirmed_within(timeout);
                Json(serde_json::json!({ "confirmed": confirmed }))
            },
        );
        finish(route, "password.confirmation", &["auth"]);
    }
}

async fn login<U: Authenticatable>(
    shared: Arc<Shared<U>>,
    ctx: TemperCtx,
    form: LoginForm,
) -> Result<Response> {
    let ctx = shared.ctx(ctx);
    // Core counts the login budgets before the lookup and the password check.
    let found = match ctx.auth().validate(&form.email, &form.password).await {
        Ok(found) => found,
        Err(e) => {
            if e.status() == StatusCode::TOO_MANY_REQUESTS {
                let email_hash = address_hash(ctx.app(), &form.email)?;
                fire(
                    ctx.app(),
                    &shared.listeners,
                    TemperEvent::Lockout { email_hash },
                )
                .await;
            }
            return Err(e);
        }
    };
    let Some(user) = found else {
        // The event fires whatever the answer: an override that fails still leaves a record of the attempt.
        let response = shared.responses.failed_login(&ctx, &form.email);
        let email_hash = address_hash(ctx.app(), &form.email)?;
        fire(
            ctx.app(),
            &shared.listeners,
            TemperEvent::Failed { email_hash },
        )
        .await;
        return response;
    };
    let user = user.downcast::<U>().ok_or_else(model_mismatch)?;
    pipeline::run(&shared, ctx, &user, form.remember).await
}

async fn logout<U>(shared: Arc<Shared<U>>, ctx: TemperCtx) -> Result<Response> {
    let ctx = shared.ctx(ctx);
    let id = ctx.auth().id();
    ctx.auth().logout().await?;
    let response = shared.responses.logout(&ctx)?;
    if let Some(user_id) = id {
        fire(
            ctx.app(),
            &shared.listeners,
            TemperEvent::Logout { user_id },
        )
        .await;
    }
    Ok(response)
}

async fn confirm_password<U>(
    shared: Arc<Shared<U>>,
    ctx: TemperCtx,
    form: ConfirmForm,
) -> Result<Response> {
    let ctx = shared.ctx(ctx);
    // Core: five tries a minute per user, counted before the password check.
    if !ctx.auth().confirm_password(&form.password).await? {
        return Err(Error::validation("password", messages::WRONG_PASSWORD));
    }
    let response = shared.responses.password_confirmed(&ctx)?;
    if let Some(user_id) = ctx.auth().id() {
        fire(
            ctx.app(),
            &shared.listeners,
            TemperEvent::PasswordConfirmed { user_id },
        )
        .await;
    }
    Ok(response)
}

// ---- registration ---------------------------------------------------------------------------------------------

pub(crate) fn registration<U, A>(reg: &mut Registrar<'_, U>, action: Arc<A>)
where
    U: Authenticatable,
    A: CreatesNewUsers<U>,
{
    let form_limit = reg.form_limit();
    if reg.page_on("register") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.get("/register", move |ctx: TemperCtx| {
            let shared = Arc::clone(&shared);
            async move { page(shared.view(|v| v.register.as_ref()), ViewCtx::new(ctx)) }
        });
        finish(route, "register", &["guest"]);
    }
    if reg.on("register.store") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.post(
            "/register",
            move |ctx: TemperCtx, Valid(input): Valid<A::Input>| {
                let (shared, action) = (Arc::clone(&shared), Arc::clone(&action));
                async move {
                    let ctx = shared.ctx(ctx);
                    let user = action.create(&ctx, input).await?;
                    refuse_enrolled(&shared, &ctx, user.auth_id()).await?;
                    // The login policies decide the first sign-in too (an approval-gated sign-up): a refusal keeps
                    // the account, signs nobody in, still fires `Registered` and answers the refusal.
                    if let Err(refused) = ctx
                        .app()
                        .check_login(&smeltery_core::auth::AuthUser::of(&user))
                        .await
                    {
                        let user_id = user.auth_id();
                        fire(
                            ctx.app(),
                            &shared.listeners,
                            TemperEvent::Registered { user_id },
                        )
                        .await;
                        return Err(refused);
                    }
                    pipeline::sign_in(&ctx, user.auth_id(), false).await?;
                    if sends_links(&shared, ctx.app()) {
                        // Sends only when the app requires verification (`.verify_email::<User>()`).
                        ctx.auth().send_verification_email().await?;
                    }
                    let response = shared.responses.register(&ctx)?;
                    let user_id = user.auth_id();
                    fire(
                        ctx.app(),
                        &shared.listeners,
                        TemperEvent::Registered { user_id },
                    )
                    .await;
                    Ok::<_, Error>(response)
                }
            },
        );
        finish(route, "register.store", &["guest", &form_limit]);
    }
}

/// Registration signs in without a password check or a second factor, which is right only for a new account. An
/// account with a two-factor enrolment cannot be new: the `create` action returned an existing user (an "upsert by
/// email" mistake), so nobody is signed in and the request fails (500, logged).
async fn refuse_enrolled<U>(shared: &Shared<U>, ctx: &TemperCtx, user_id: i64) -> Result<()> {
    let Some(required) = &shared.second_step else {
        return Ok(());
    };
    if required(ctx.app().clone(), user_id).await? {
        tracing::error!(
            user_id,
            "registration refused: the `CreatesNewUsers` action returned a user with two-factor authentication, \
             so not a new account"
        );
        return Err(Error::internal(
            "the registration action returned an existing account (it has two-factor authentication)",
        ));
    }
    Ok(())
}

// ---- password resets ------------------------------------------------------------------------------------------

pub(crate) fn reset_passwords<U, A>(reg: &mut Registrar<'_, U>, action: Arc<A>)
where
    U: Authenticatable,
    A: ResetsUserPasswords<U>,
{
    let form_limit = reg.form_limit();
    if reg.page_on("password.request") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.get("/forgot-password", move |ctx: TemperCtx| {
            let shared = Arc::clone(&shared);
            async move {
                page(
                    shared.view(|v| v.forgot_password.as_ref()),
                    ViewCtx::new(ctx),
                )
            }
        });
        finish(route, "password.request", &["guest"]);
    }
    if reg.on("password.email") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.post(
            "/forgot-password",
            move |ctx: TemperCtx, Valid(form): Valid<EmailForm>| {
                let shared = Arc::clone(&shared);
                async move {
                    let ctx = shared.ctx(ctx);
                    // The same answer whether or not the address has an account (core sends in the background).
                    let issued = passwords::send_reset_link(ctx.app(), &form.email).await?;
                    let response = shared.responses.reset_link_sent(&ctx)?;
                    if let Some(user_id) = issued {
                        fire(
                            ctx.app(),
                            &shared.listeners,
                            TemperEvent::PasswordResetLinkSent { user_id },
                        )
                        .await;
                    }
                    Ok::<_, Error>(response)
                }
            },
        );
        finish(route, "password.email", &["guest", &form_limit]);
    }
    if reg.page_on("password.reset") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.get(
            "/reset-password/{token}",
            move |ctx: TemperCtx, Path(token): Path<String>, Query(query): Query<ResetQuery>| {
                let shared = Arc::clone(&shared);
                async move {
                    // A path segment that is no reset token (`abc%2F..%2Flogin`, decoded by the router) is an invalid
                    // link: the page would put it into its form's `action`.
                    if !crate::ctx::is_reset_token(&token) {
                        let ctx = shared.ctx(ctx);
                        return shared.responses.password_reset_failed(&ctx);
                    }
                    page(
                        shared.view(|v| v.reset_password.as_ref()),
                        ViewCtx::new(ctx).with_link(token, query.email),
                    )
                }
            },
        );
        finish(route, "password.reset", &["guest"]);
    }
    if reg.on("password.update") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.post(
            "/reset-password/{token}",
            move |ctx: TemperCtx, Path(token): Path<String>, Valid(input): Valid<A::Input>| {
                let (shared, action) = (Arc::clone(&shared), Arc::clone(&action));
                async move {
                    let ctx = shared.ctx(ctx);
                    // Core: the account by its stored address, the token used up once, the password changed by
                    // id, every session and remember-me cookie of the user ended.
                    let reset =
                        passwords::reset(ctx.app(), input.email(), &token, input.password())
                            .await?;
                    let Some(user_id) = reset else {
                        return shared.responses.password_reset_failed(&ctx);
                    };
                    if let Some(user) = ctx
                        .app()
                        .find_user(user_id)
                        .await?
                        .and_then(|u| u.downcast::<U>())
                    {
                        action.reset(&ctx, &user, &input).await?;
                    }
                    let response = shared.responses.password_reset(&ctx)?;
                    fire(
                        ctx.app(),
                        &shared.listeners,
                        TemperEvent::PasswordReset { user_id },
                    )
                    .await;
                    Ok(response)
                }
            },
        );
        finish(route, "password.update", &["guest", &form_limit]);
    }
}

// ---- e-mail verification --------------------------------------------------------------------------------------

pub(crate) fn email_verification<U: Authenticatable>(reg: &mut Registrar<'_, U>) {
    let form_limit = reg.form_limit();
    if reg.page_on("verification.notice") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.get("/email/verify", move |ctx: TemperCtx| {
            let shared = Arc::clone(&shared);
            async move {
                let ctx = shared.ctx(ctx);
                // Nothing to verify (verified, or the app does not require it): on to the home page.
                if ctx.auth().has_verified_email().await? {
                    return Ok(Redirect::to(ctx.home()).into_response());
                }
                page(shared.view(|v| v.verify_email.as_ref()), ViewCtx::new(ctx))
            }
        });
        finish(route, "verification.notice", &["auth"]);
    }
    if reg.on("verification.verify") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.get(
            "/email/verify/{id}/{hash}",
            move |ctx: TemperCtx, request: EmailVerificationRequest| {
                let shared = Arc::clone(&shared);
                async move {
                    let ctx = shared.ctx(ctx);
                    let newly = request.fulfill().await?;
                    let response = shared.responses.email_verified(&ctx, newly)?;
                    if newly {
                        let user_id = request.user_id();
                        fire(
                            ctx.app(),
                            &shared.listeners,
                            TemperEvent::Verified { user_id },
                        )
                        .await;
                    }
                    Ok::<_, Error>(response)
                }
            },
        );
        finish(route, "verification.verify", &["auth", &form_limit]);
    }
    if reg.on("verification.send") {
        let shared = Arc::clone(&reg.shared);
        // Core counts six sends a minute per user.
        let route = reg
            .router
            .post("/email/verification-notification", move |ctx: TemperCtx| {
                let shared = Arc::clone(&shared);
                async move {
                    let ctx = shared.ctx(ctx);
                    let sent = ctx.auth().resend_verification_email().await?;
                    shared.responses.verification_link_sent(&ctx, sent)
                }
            });
        finish(route, "verification.send", &["auth"]);
    }
}

// ---- profile and password updates ----------------------------------------------------------------------------

pub(crate) fn update_profile<U, A>(reg: &mut Registrar<'_, U>, action: Arc<A>)
where
    U: Authenticatable,
    A: UpdatesUserProfileInformation<U>,
{
    let form_limit = reg.form_limit();
    if !reg.on("user-profile-information.update") {
        return;
    }
    let shared = Arc::clone(&reg.shared);
    let route = reg.router.put(
        "/user/profile-information",
        move |ctx: TemperCtx, Valid(input): Valid<A::Input>| {
            let (shared, action) = (Arc::clone(&shared), Arc::clone(&action));
            async move {
                let ctx = shared.ctx(ctx);
                let user = ctx
                    .auth()
                    .user::<U>()
                    .await?
                    .ok_or_else(Error::unauthorized)?;
                let changed = action.update(&ctx, &user, input).await?;
                let email_changed = changed == EmailChanged::Yes;
                if email_changed {
                    // A reset link mailed to the old address must not reset the account any more.
                    if shared.reset_passwords
                        || ctx.app().url("password.reset", &[("token", "0")]).is_ok()
                    {
                        forget_reset_token(ctx.app(), user.auth_id()).await?;
                    }
                    // Core is the authority on whether the app verifies addresses: a new address is never verified.
                    if unverify(ctx.app(), user.auth_id()).await?
                        && sends_links(&shared, ctx.app())
                        && verification_mail_allowed(ctx.app(), user.auth_id()).await?
                    {
                        // Reads the row again: the link goes to the new address.
                        ctx.auth().send_verification_email().await?;
                    }
                }
                let response = shared.responses.profile_updated(&ctx)?;
                let user_id = user.auth_id();
                fire(
                    ctx.app(),
                    &shared.listeners,
                    TemperEvent::ProfileUpdated {
                        user_id,
                        email_changed,
                    },
                )
                .await;
                Ok::<_, Error>(response)
            }
        },
    );
    // `password.confirm`: the address decides where reset links go, so changing it needs the password again (within
    // `AUTH_PASSWORD_TIMEOUT`), like the two-factor management routes. A stolen session alone cannot move the
    // account to another mailbox and reset its password.
    finish(
        route,
        "user-profile-information.update",
        &["auth", "password.confirm", &form_limit],
    );
}

/// Verification links an address change may mail per user and hour: the address is typed by the user, so without a
/// budget of its own an account could mail links to any address (`throttle:` counts requests, not mails).
const VERIFICATION_MAILS_PER_HOUR: u32 = 3;

/// Whether one more verification link may go out after an address change of user `id` (counted; a cache failure
/// refuses). Over the budget the change is still saved; the user asks for the link again later (`verification.send`).
async fn verification_mail_allowed(app: &App, id: i64) -> Result<bool> {
    static MAILS: std::sync::LazyLock<smeltery_core::cache::RateLimiter> =
        std::sync::LazyLock::new(|| {
            smeltery_core::cache::RateLimiter::new(
                "temper.verification-mail",
                VERIFICATION_MAILS_PER_HOUR,
                std::time::Duration::from_secs(3600),
            )
        });
    let allowed = MAILS.hit(app, &format!("user:{id}")).await?.allowed();
    if !allowed {
        tracing::info!(
            user_id = id,
            "no verification link after the address change: the hourly budget is used up"
        );
    }
    Ok(allowed)
}

/// Delete user `id`'s pending reset token (core's `password_reset_tokens` row): it was mailed to the old address.
async fn forget_reset_token(app: &App, id: i64) -> Result<()> {
    use sea_orm::ConnectionTrait as _;
    use sea_orm::sea_query::{Alias, Expr, ExprTrait as _, Query};
    let db = app.db()?;
    let backend = db.conn().get_database_backend();
    let delete = Query::delete()
        .from_table(Alias::new(passwords::TOKENS_TABLE))
        .and_where(Expr::col(Alias::new("user_id")).eq(id))
        .to_owned();
    db.conn().execute_raw(backend.build(&delete)).await?;
    Ok(())
}

/// Empty `email_verified_at` of user `id` through core; `false` when the app does not verify addresses.
async fn unverify(app: &App, id: i64) -> Result<bool> {
    if !app.verifies_email() {
        return Ok(false);
    }
    smeltery_core::auth::mark_unverified(app, id).await?;
    Ok(true)
}

pub(crate) fn update_passwords<U, A>(reg: &mut Registrar<'_, U>, action: Arc<A>)
where
    U: Authenticatable,
    A: UpdatesUserPasswords<U>,
{
    let form_limit = reg.form_limit();
    if !reg.on("user-password.update") {
        return;
    }
    let shared = Arc::clone(&reg.shared);
    let route = reg.router.put(
        "/user/password",
        move |ctx: TemperCtx, Valid(input): Valid<A::Input>| {
            let (shared, action) = (Arc::clone(&shared), Arc::clone(&action));
            async move {
                let ctx = shared.ctx(ctx);
                let user = ctx
                    .auth()
                    .user::<U>()
                    .await?
                    .ok_or_else(Error::unauthorized)?;
                // The current password through core's confirmation: five tries a minute per user, counted before
                // the check (shared with the confirm-password form).
                match ctx.auth().confirm_password(input.current_password()).await {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(Error::validation(
                            "current_password",
                            messages::WRONG_CURRENT_PASSWORD,
                        ));
                    }
                    Err(e) => return Err(on_field(e, "password", "current_password")),
                }
                // This session stays signed in; every other session and remember-me cookie ends.
                ctx.auth().set_password(input.password()).await?;
                action.update(&ctx, &user, &input).await?;
                let response = shared.responses.password_updated(&ctx)?;
                let user_id = user.auth_id();
                fire(
                    ctx.app(),
                    &shared.listeners,
                    TemperEvent::PasswordUpdated { user_id },
                )
                .await;
                Ok::<_, Error>(response)
            }
        },
    );
    finish(route, "user-password.update", &["auth", &form_limit]);
}

/// `error` with its messages on `from` moved to `to` (core's confirmation budget names `password`).
fn on_field(error: Error, from: &str, to: &str) -> Error {
    match error {
        Error::Validation(mut invalid) => {
            let mut errors = ValidationErrors::new();
            for (field, messages) in invalid.errors.iter() {
                let field = if field == from { to } else { field };
                for message in messages {
                    errors.add(field, message.clone());
                }
            }
            invalid.errors = errors;
            Error::Validation(invalid)
        }
        other => other,
    }
}
