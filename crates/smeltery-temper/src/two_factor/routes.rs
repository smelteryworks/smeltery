//! The two-factor routes: the login challenge and enrolment management.

use std::sync::Arc;

use smeltery_core::http::{HeaderValue, IntoResponse, Json, StatusCode, header};
use smeltery_core::validation::{Invalid, Valid, ValidationErrors};
use smeltery_core::{Error, Response, Result};

use super::{
    Checked, CodeKind, RECOVERY_CODES_KEY, TwoFactor, TwoFactorAuthenticatable, now, pending, qr,
    store,
};
use crate::ctx::flash_invalid;
use crate::events::{TemperEvent, fire};
use crate::forms::{ChallengeForm, CodeForm};
use crate::routes::{Registrar, finish, page};
use crate::{Shared, TemperCtx, ViewCtx, messages};

/// Every route name of the two-factor feature.
pub(crate) const ROUTE_NAMES: &[&str] = &[
    "two-factor.login",
    "two-factor.login.store",
    "two-factor.enable",
    "two-factor.confirm",
    "two-factor.disable",
    "two-factor.qr-code",
    "two-factor.secret-key",
    "two-factor.recovery-codes",
    "two-factor.regenerate-recovery-codes",
];

/// 429 with `message` on `field` and `Retry-After` (JSON clients), or the message flashed and the challenge page.
fn too_many(ctx: &TemperCtx, field: &str, retry_after: u64, redirect_to: &str) -> Response {
    let message = messages::TWO_FACTOR_TOO_MANY.replace("{}", &retry_after.to_string());
    if ctx.wants_json() {
        let invalid = Invalid::too_many(field, message, retry_after);
        return Error::Validation(Box::new(invalid)).into_response();
    }
    flash_invalid(ctx, field, &message, &[]);
    smeltery_core::http::Redirect::to(redirect_to).into_response()
}

/// A login policy refused the user at the challenge (the pending login is gone): JSON clients get the refusal with
/// its message on `code` (the field of this form); browsers go to the login page with it on `email`, where that page
/// shows it (the challenge page would only send them on, losing the flash). A policy's own error stays an error.
fn policy_refusal(ctx: &TemperCtx, refused: Error) -> Response {
    let Error::Validation(mut invalid) = refused else {
        return refused.into_response();
    };
    let message = invalid.message.clone();
    if ctx.wants_json() {
        let mut errors = ValidationErrors::new();
        errors.add("code", message);
        invalid.errors = errors;
        return Error::Validation(invalid).into_response();
    }
    flash_invalid(ctx, "email", &message, &[]);
    smeltery_core::http::Redirect::to(&ctx.route_or("login", "/login")).into_response()
}

/// A JSON answer nobody caches (it holds a secret or its state).
fn private(body: serde_json::Value) -> Response {
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(crate) fn register<U: TwoFactorAuthenticatable>(
    reg: &mut Registrar<'_, U>,
    options: TwoFactor,
) {
    let login_limit = reg.login_limit();
    let form_limit = reg.form_limit();
    if reg.page_on("two-factor.login") {
        let shared = Arc::clone(&reg.shared);
        let route = reg
            .router
            .get("/two-factor-challenge", move |ctx: TemperCtx| {
                let shared = Arc::clone(&shared);
                async move {
                    let ctx = shared.ctx(ctx);
                    // Only while a login waits for its second factor.
                    if pending::<U>(ctx.app(), ctx.session(), &options)
                        .await?
                        .is_none()
                    {
                        return Ok(smeltery_core::http::Redirect::to(
                            &ctx.route_or("login", "/login"),
                        )
                        .into_response());
                    }
                    page(
                        shared.view(|v| v.two_factor_challenge.as_ref()),
                        ViewCtx::new(ctx),
                    )
                }
            });
        finish(route, "two-factor.login", &["guest"]);
    }
    if reg.on("two-factor.login.store") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.post(
            "/two-factor-challenge",
            move |ctx: TemperCtx, Valid(form): Valid<ChallengeForm>| {
                let shared = Arc::clone(&shared);
                async move { challenge::<U>(shared, options, ctx, form).await }
            },
        );
        finish(route, "two-factor.login.store", &["guest", &login_limit]);
    }

    // The management routes: signed in, and with `confirm_password` a recent password confirmation.
    let mut guard = vec!["auth"];
    if options.confirm_password {
        guard.push("password.confirm");
    }
    let mut writes = guard.clone();
    writes.push(&form_limit);

    if reg.on("two-factor.enable") {
        let shared = Arc::clone(&reg.shared);
        let route = reg
            .router
            .post("/user/two-factor-authentication", move |ctx: TemperCtx| {
                let shared = Arc::clone(&shared);
                async move { enable::<U>(shared, options, ctx).await }
            });
        finish(route, "two-factor.enable", &writes);
    }
    if reg.on("two-factor.confirm") {
        let shared = Arc::clone(&reg.shared);
        let route = reg.router.post(
            "/user/confirmed-two-factor-authentication",
            move |ctx: TemperCtx, Valid(form): Valid<CodeForm>| {
                let shared = Arc::clone(&shared);
                async move { confirm::<U>(shared, options, ctx, form).await }
            },
        );
        finish(route, "two-factor.confirm", &writes);
    }
    if reg.on("two-factor.disable") {
        let shared = Arc::clone(&reg.shared);
        let route = reg
            .router
            .delete("/user/two-factor-authentication", move |ctx: TemperCtx| {
                let shared = Arc::clone(&shared);
                async move {
                    let ctx = shared.ctx(ctx);
                    let user = signed_in::<U>(&ctx).await?;
                    if let Some(refused) = write_needs_recent_password(&ctx, &user) {
                        return Ok(refused);
                    }
                    let was_on = user.two_factor_secret().is_some();
                    store::disable::<U>(ctx.app(), user.auth_id()).await?;
                    // Remember-me cookies issued under the old setting stop signing in.
                    smeltery_core::auth::cycle_remember_token(ctx.app(), user.auth_id()).await?;
                    let response = shared.responses.two_factor_disabled(&ctx)?;
                    if was_on {
                        let user_id = user.auth_id();
                        fire(
                            ctx.app(),
                            &shared.listeners,
                            TemperEvent::TwoFactorDisabled { user_id },
                        )
                        .await;
                    }
                    Ok::<_, Error>(response)
                }
            });
        finish(route, "two-factor.disable", &writes);
    }
    if reg.on("two-factor.qr-code") {
        let route = reg
            .router
            .get("/user/two-factor-qr-code", |ctx: TemperCtx| async move {
                let user = signed_in::<U>(&ctx).await?;
                if let Some(refused) = needs_recent_password(&ctx, &user) {
                    return Ok(refused);
                }
                let secret = super::secret_text(ctx.app(), &user)
                    .await?
                    .ok_or_else(Error::not_found)?;
                let uri = qr::otpauth_uri(
                    &ctx.app().settings().name,
                    &user.two_factor_account(),
                    &secret,
                );
                let svg = qr::svg(&uri)?;
                let url = qr::data_uri(&svg);
                Ok::<_, Error>(private(serde_json::json!({ "svg": svg, "url": url })))
            });
        finish(route, "two-factor.qr-code", &guard);
    }
    if reg.on("two-factor.secret-key") {
        let route = reg
            .router
            .get("/user/two-factor-secret-key", |ctx: TemperCtx| async move {
                let user = signed_in::<U>(&ctx).await?;
                if let Some(refused) = needs_recent_password(&ctx, &user) {
                    return Ok(refused);
                }
                let secret = super::secret_text(ctx.app(), &user)
                    .await?
                    .ok_or_else(Error::not_found)?;
                Ok::<_, Error>(private(serde_json::json!({ "secretKey": secret })))
            });
        finish(route, "two-factor.secret-key", &guard);
    }
    if reg.on("two-factor.recovery-codes") {
        let route = reg.router.get(
            "/user/two-factor-recovery-codes",
            |ctx: TemperCtx| async move {
                let user = signed_in::<U>(&ctx).await?;
                if user.two_factor_secret().is_none() {
                    return Err(Error::not_found());
                }
                // The codes are shown once, when made; here only how many are left.
                Ok(private(
                    serde_json::json!({ "remaining": super::codes_left(&user) }),
                ))
            },
        );
        finish(route, "two-factor.recovery-codes", &guard);
    }
    if reg.on("two-factor.regenerate-recovery-codes") {
        let shared = Arc::clone(&reg.shared);
        let route = reg
            .router
            .post("/user/two-factor-recovery-codes", move |ctx: TemperCtx| {
                let shared = Arc::clone(&shared);
                async move {
                    let ctx = shared.ctx(ctx);
                    let user = signed_in::<U>(&ctx).await?;
                    if user.two_factor_secret().is_none() {
                        return Err(Error::not_found());
                    }
                    if let Some(refused) = write_needs_recent_password(&ctx, &user) {
                        return Ok(refused);
                    }
                    let (codes, stored) = super::new_codes(options.recovery_codes)?;
                    store::set_codes::<U>(ctx.app(), user.auth_id(), stored).await?;
                    // JSON clients get them in the answer only.
                    if !ctx.wants_json() {
                        ctx.session().flash(RECOVERY_CODES_KEY, &codes);
                    }
                    let response = shared.responses.recovery_codes_generated(&ctx, &codes)?;
                    let user_id = user.auth_id();
                    fire(
                        ctx.app(),
                        &shared.listeners,
                        TemperEvent::RecoveryCodesGenerated { user_id },
                    )
                    .await;
                    Ok(response)
                }
            });
        finish(route, "two-factor.regenerate-recovery-codes", &writes);
    }
}

/// The secret of a confirmed enrolment is shown only after a recent password confirmation, whatever
/// `confirm_password` says (a stolen session must not clone the authenticator): 423 otherwise.
fn needs_recent_password<U: TwoFactorAuthenticatable>(
    ctx: &TemperCtx,
    user: &U,
) -> Option<Response> {
    let timeout = ctx.app().settings().password_timeout;
    (user.two_factor_confirmed_at().is_some() && !ctx.auth().password_confirmed_within(timeout))
        .then(|| {
            (
                StatusCode::LOCKED,
                Json(serde_json::json!({ "message": "Password confirmation required." })),
            )
                .into_response()
        })
}

/// A write that touches a confirmed enrolment (turning it off, new recovery codes, enabling over it) needs a recent
/// password confirmation whatever `confirm_password` says: with the session alone, someone could otherwise take the
/// recovery codes or put their own authenticator in place of the owner's. JSON clients get 423; browsers and Inertia
/// visits go to the password confirmation page (as the `password.confirm` middleware sends them).
fn write_needs_recent_password<U: TwoFactorAuthenticatable>(
    ctx: &TemperCtx,
    user: &U,
) -> Option<Response> {
    let refused = needs_recent_password(ctx, user)?;
    if ctx.wants_json() {
        return Some(refused);
    }
    Some(
        smeltery_core::http::Redirect::to(
            &ctx.route_or("password.confirm", "/user/confirm-password"),
        )
        .into_response(),
    )
}

async fn signed_in<U: TwoFactorAuthenticatable>(ctx: &TemperCtx) -> Result<U> {
    ctx.auth()
        .user::<U>()
        .await?
        .ok_or_else(Error::unauthorized)
}

/// `POST /two-factor-challenge`.
async fn challenge<U: TwoFactorAuthenticatable>(
    shared: Arc<Shared<U>>,
    options: TwoFactor,
    ctx: TemperCtx,
    form: ChallengeForm,
) -> Result<Response> {
    let ctx = shared.ctx(ctx);
    let app = ctx.app().clone();
    let Some((mut waiting, user)) = pending::<U>(&app, ctx.session(), &options).await? else {
        return shared.responses.two_factor_expired(&ctx);
    };
    let (kind, code) = match (
        form.recovery_code.filter(|c| !c.trim().is_empty()),
        form.code,
    ) {
        (Some(recovery), _) => (CodeKind::Recovery, recovery),
        (None, Some(code)) if !code.trim().is_empty() => (CodeKind::Totp, code),
        _ => return Err(Error::validation("code", "The code field is required.")),
    };
    // The login policies again: the account may have been suspended since the password step. Asked before the
    // code, so a refused account spends no code.
    if let Err(refused) = app
        .check_login(&smeltery_core::auth::AuthUser::of(&user))
        .await
    {
        ctx.session().remove(super::PENDING_KEY);
        return Ok(policy_refusal(&ctx, refused));
    }
    let user_id = user.auth_id();
    let challenge_page = ctx.route_or("two-factor.login", "/two-factor-challenge");
    match super::check(&app, &options, &user, &code, kind).await? {
        Checked::Limited { retry_after } => {
            let response = too_many(&ctx, kind.field(), retry_after, &challenge_page);
            fire(
                &app,
                &shared.listeners,
                TemperEvent::TwoFactorLockout { user_id },
            )
            .await;
            Ok(response)
        }
        Checked::Wrong => {
            waiting.failures += 1;
            let response = if waiting.failures >= super::MAX_PENDING_FAILURES {
                // The password is needed again (and with it core's login budgets).
                ctx.session().remove(super::PENDING_KEY);
                shared.responses.two_factor_expired(&ctx)
            } else {
                ctx.session().insert(super::PENDING_KEY, &waiting);
                shared.responses.two_factor_failed(&ctx, kind.field())
            };
            fire(
                &app,
                &shared.listeners,
                TemperEvent::TwoFactorFailed { user_id },
            )
            .await;
            response
        }
        Checked::Accepted { recovery_left } => {
            ctx.session().remove(super::PENDING_KEY);
            // Core: a new session id and CSRF secret, the keys of the last sign-in gone.
            crate::pipeline::sign_in(&ctx, user_id, waiting.remember).await?;
            let response = shared.responses.two_factor_login(&ctx)?;
            let remember = waiting.remember;
            fire(
                &app,
                &shared.listeners,
                TemperEvent::Login {
                    user_id,
                    two_factor: true,
                    remember,
                },
            )
            .await;
            if let Some(left) = recovery_left {
                fire(
                    &app,
                    &shared.listeners,
                    TemperEvent::RecoveryCodeUsed { user_id, left },
                )
                .await;
            }
            Ok(response)
        }
    }
}

/// `POST /user/two-factor-authentication`.
async fn enable<U: TwoFactorAuthenticatable>(
    shared: Arc<Shared<U>>,
    options: TwoFactor,
    ctx: TemperCtx,
) -> Result<Response> {
    let ctx = shared.ctx(ctx);
    let user = signed_in::<U>(&ctx).await?;
    if let Some(refused) = write_needs_recent_password(&ctx, &user) {
        return Ok(refused);
    }
    // A confirmed enrolment is never replaced in place: turn it off first.
    if user.two_factor_secret().is_some() && user.two_factor_confirmed_at().is_some() {
        return shared.responses.two_factor_already_enabled(&ctx);
    }
    let id = user.auth_id();
    let secret = super::new_secret()?;
    let (codes, stored) = super::new_codes(options.recovery_codes)?;
    let confirmed_at = (!options.confirm).then(|| now(ctx.app()));
    store::enable::<U>(
        ctx.app(),
        id,
        super::seal(ctx.app(), id, &secret)?,
        stored,
        confirmed_at,
    )
    .await?;
    if confirmed_at.is_some() {
        smeltery_core::auth::cycle_remember_token(ctx.app(), id).await?;
    }
    // JSON clients get them in the answer only.
    if !ctx.wants_json() {
        ctx.session().flash(RECOVERY_CODES_KEY, &codes);
    }
    let response = shared.responses.two_factor_enabled(&ctx, &codes)?;
    fire(
        ctx.app(),
        &shared.listeners,
        TemperEvent::TwoFactorEnabled { user_id: id },
    )
    .await;
    Ok(response)
}

/// `POST /user/confirmed-two-factor-authentication`.
async fn confirm<U: TwoFactorAuthenticatable>(
    shared: Arc<Shared<U>>,
    options: TwoFactor,
    ctx: TemperCtx,
    form: CodeForm,
) -> Result<Response> {
    let ctx = shared.ctx(ctx);
    let user = signed_in::<U>(&ctx).await?;
    if user.two_factor_secret().is_none() {
        return Err(Error::not_found());
    }
    if user.two_factor_confirmed_at().is_some() {
        return shared.responses.two_factor_confirmed(&ctx);
    }
    let user_id = user.auth_id();
    match super::check(ctx.app(), &options, &user, &form.code, CodeKind::Totp).await? {
        Checked::Limited { retry_after } => {
            let back = ctx.back().to_owned();
            let response = too_many(&ctx, "code", retry_after, &back);
            fire(
                ctx.app(),
                &shared.listeners,
                TemperEvent::TwoFactorLockout { user_id },
            )
            .await;
            Ok(response)
        }
        Checked::Wrong => {
            fire(
                ctx.app(),
                &shared.listeners,
                TemperEvent::TwoFactorFailed { user_id },
            )
            .await;
            Err(Error::validation("code", messages::TWO_FACTOR_FAILED))
        }
        Checked::Accepted { .. } => {
            store::confirm::<U>(ctx.app(), user_id, now(ctx.app())).await?;
            // Remember-me cookies issued before the second factor stop signing in; this session stays.
            smeltery_core::auth::cycle_remember_token(ctx.app(), user_id).await?;
            let response = shared.responses.two_factor_confirmed(&ctx)?;
            fire(
                ctx.app(),
                &shared.listeners,
                TemperEvent::TwoFactorConfirmed { user_id },
            )
            .await;
            Ok(response)
        }
    }
}
