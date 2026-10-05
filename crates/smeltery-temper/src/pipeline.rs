//! The login pipeline: what happens once a user is known to be who they say (a correct password, or a social
//! login): the app's own steps, then the sign-in, the `Login` event and the answer.

use std::future::Future;
use std::sync::Arc;

use smeltery_core::auth::{
    Auth, AuthUser, Authenticatable, LoginCompletion, LoginDecision, LoginPolicy,
};
use smeltery_core::http::HeaderMap;
use smeltery_core::session::Session;
use smeltery_core::{App, BoxFuture, Error, Response, Result};

use crate::events::{TemperEvent, fire};
use crate::{Shared, TemperCtx};

/// What a login-pipeline step decides ([`Temper::login_pipeline`](crate::Temper::login_pipeline)).
#[derive(Debug)]
#[non_exhaustive]
pub enum PipelineStep {
    /// Go on with the next step (and in the end sign in).
    Continue,
    /// Stop: answer with this response, nobody is signed in.
    Respond(Response),
}

pub(crate) type Hook<U> =
    Arc<dyn Fn(TemperCtx, U) -> BoxFuture<'static, Result<PipelineStep>> + Send + Sync>;

pub(crate) fn hook<U, F, Fut>(step: F) -> Hook<U>
where
    U: 'static,
    F: Fn(TemperCtx, U) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<PipelineStep>> + Send + 'static,
{
    Arc::new(move |ctx, user| Box::pin(step(ctx, user)))
}

/// A typed login policy ([`Temper::login_policy`](crate::Temper::login_policy)) as core's [`LoginPolicy`].
pub(crate) struct Policy<U>(
    pub(crate) Arc<dyn Fn(App, U) -> BoxFuture<'static, Result<LoginDecision>> + Send + Sync>,
);

pub(crate) fn policy<U, F, Fut>(check: F) -> Policy<U>
where
    U: 'static,
    F: Fn(App, U) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<LoginDecision>> + Send + 'static,
{
    Policy(Arc::new(move |app, user| Box::pin(check(app, user))))
}

impl<U: Authenticatable> LoginPolicy for Policy<U> {
    fn check<'a>(
        &'a self,
        app: &'a App,
        user: &'a AuthUser,
    ) -> BoxFuture<'a, Result<LoginDecision>> {
        Box::pin(async move {
            let user = user.downcast::<U>().ok_or_else(|| {
                Error::internal("the user given to a login policy is not Temper's user model")
            })?;
            (self.0)(app.clone(), user).await
        })
    }
}

/// Run the login pipeline for `user`, whose credentials are already checked: core's login policies
/// ([`App::check_login`], the rules token endpoints meet too; a refusal is returned as the error), the app's steps
/// ([`Temper::login_pipeline`](crate::Temper::login_pipeline)) in order, then
/// [`Auth::login`](smeltery_core::auth::Auth::login) (a new session id and CSRF token), the
/// [`TemperEvent::Login`] event and the login answer ([`TemperResponses::login`](crate::TemperResponses::login)).
/// Temper's `POST /login` runs it after the password check; a social login runs it after the other service's
/// callback (through core's `app.login_completion()`, which `.temper(…)` registers).
///
/// The steps gate new sign-ins only: a session that is already signed in and a sign-in restored from a remember-me
/// cookie never meet them. To lock an account out, also end its credentials (`auth::password_changed`, which ends
/// every session and remember-me cookie) or check the user in a middleware.
///
/// # Errors
/// Temper is not installed for `U` (`.temper(…)`), a login policy refuses the user, a step fails, or signing in
/// fails.
pub async fn login_pipeline<U: Authenticatable>(
    ctx: &TemperCtx,
    user: &U,
    remember: bool,
) -> Result<Response> {
    let shared = ctx.app().service::<Arc<Shared<U>>>().ok_or_else(|| {
        Error::internal("Temper is not installed for this user model: call `.temper(…)`")
    })?;
    run(
        &shared,
        ctx.clone().with_home(shared.home.as_deref()),
        user,
        remember,
    )
    .await
}

pub(crate) async fn run<U: Authenticatable>(
    shared: &Shared<U>,
    ctx: TemperCtx,
    user: &U,
    remember: bool,
) -> Result<Response> {
    // Core's session-free rules first: the same ones a token endpoint asks, before any step or second factor.
    ctx.app().check_login(&AuthUser::of(user)).await?;
    for step in &shared.hooks {
        if let PipelineStep::Respond(response) = step(ctx.clone(), user.clone()).await? {
            return Ok(response);
        }
    }
    // A user with a second factor gets a pending login (a new session id) and the challenge, not a sign-in.
    let needs_second_step = match &shared.second_step {
        Some(required) => required(ctx.app().clone(), user.auth_id()).await?,
        None => false,
    };
    if needs_second_step {
        let user_id = user.auth_id();
        crate::two_factor::start_pending(ctx.app(), ctx.session(), user_id, remember).await?;
        let response = shared.responses.two_factor_challenge(&ctx)?;
        fire(
            ctx.app(),
            &shared.listeners,
            TemperEvent::TwoFactorChallenged { user_id },
        )
        .await;
        return Ok(response);
    }
    // Every step has passed: only now is anyone signed in.
    sign_in(&ctx, user.auth_id(), remember).await?;
    let response = shared.responses.login(&ctx)?;
    fire(
        ctx.app(),
        &shared.listeners,
        TemperEvent::Login {
            user_id: user.auth_id(),
            two_factor: false,
            remember,
        },
    )
    .await;
    Ok(response)
}

/// Temper's pipeline as core's [`LoginCompletion`]: a crate that does not depend on Temper (social login) signs
/// users in with `app.login_completion()` and meets the same steps.
pub(crate) struct Completion<U>(pub(crate) Arc<Shared<U>>);

impl<U: Authenticatable> LoginCompletion for Completion<U> {
    fn complete<'a>(
        &'a self,
        app: &'a App,
        auth: &'a Auth,
        session: &'a Session,
        headers: &'a HeaderMap,
        user: AuthUser,
        remember: bool,
    ) -> BoxFuture<'a, Result<Response>> {
        Box::pin(async move {
            let user = user.downcast::<U>().ok_or_else(|| {
                Error::internal("the user given to the login pipeline is not Temper's user model")
            })?;
            let ctx = TemperCtx::new(app.clone(), auth.clone(), session.clone(), headers.clone())
                .with_home(self.0.home.as_deref());
            run(&self.0, ctx, &user, remember).await
        })
    }
}

/// Sign in user `id` through core (a new session id and CSRF secret, the keys of the last sign-in gone). Temper is
/// the app's login completion, so it signs in with `Auth::login_user` (core refuses `Auth::login` while a completion
/// is registered: that call would skip these steps).
pub(crate) async fn sign_in(ctx: &TemperCtx, id: i64, remember: bool) -> Result<()> {
    let user = ctx
        .app()
        .find_user(id)
        .await?
        .ok_or_else(|| Error::internal("the user signing in is gone"))?;
    ctx.auth().login_user(&user, remember).await
}
