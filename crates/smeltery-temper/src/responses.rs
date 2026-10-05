//! The answers of Temper's routes, one method per outcome. An app changes one by implementing
//! [`TemperResponses`] and overriding that method ([`Temper::responses`](crate::Temper::responses)); the others
//! keep their defaults.

use smeltery_core::http::{IntoResponse, Json, Redirect, StatusCode};
use smeltery_core::{Error, Response, Result};

use crate::TemperCtx;
use crate::ctx::flash_invalid;
use crate::messages;

fn redirect(to: &str) -> Response {
    Redirect::to(to).into_response()
}

fn json(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

/// JSON that holds a secret: never stored by a cache.
fn private_json(body: serde_json::Value) -> Response {
    let mut response = json(StatusCode::OK, body);
    response.headers_mut().insert(
        smeltery_core::http::header::CACHE_CONTROL,
        smeltery_core::http::HeaderValue::from_static("no-store"),
    );
    response
}

fn empty(status: StatusCode) -> Response {
    status.into_response()
}

/// The answer of each outcome. Every method has a default: browsers and Inertia visits get a 303 redirect (with a
/// flashed `status` or `error` message where the table says so), JSON clients ([`TemperCtx::wants_json`]) a status
/// code and a small JSON body.
///
/// | Outcome | Browser / Inertia | JSON client |
/// |---|---|---|
/// | `login` | the remembered page, else home | 200 `{"two_factor": false}` |
/// | `failed_login` | the login page with the message on `email` | 422 on `email` |
/// | `register` | home | 201 |
/// | `logout` | the route `home`, else `/` | 204 |
/// | `reset_link_sent` | `status` + the forgot-password page | 200 `{"message": …}` |
/// | `password_reset` | `status` + the login page | 200 `{"message": …}` |
/// | `password_reset_failed` | `error` + the forgot-password page | 422 on `email` |
/// | `profile_updated`, `password_updated` | `status` + back | 200 |
/// | `password_confirmed` | the remembered page, else home | 201 |
/// | `email_verified` | `status` (first time) + home | 204 |
/// | `verification_link_sent` | `status` (when sent) + back | 202 |
/// | `two_factor_challenge` | the challenge page | 200 `{"two_factor": true}` |
/// | `two_factor_login` | the remembered page, else home | 204 |
/// | `two_factor_failed` | the challenge page with the message on `code` / `recovery_code` | 422 on that field |
/// | `two_factor_expired` | `error` + the login page | 422 on `code` |
/// | `two_factor_enabled`, `recovery_codes_generated` | `status` + back | 200 `{"recovery_codes": […]}` |
/// | `two_factor_already_enabled` | `error` + back | 409 `{"message": …}` |
/// | `two_factor_confirmed`, `two_factor_disabled` | `status` + back | 200 |
///
/// ```
/// use smeltery::temper::{TemperCtx, TemperResponses};
/// use smeltery::http::{IntoResponse, Redirect};
/// use smeltery::{Response, Result};
///
/// /// Signed-out users land on `/goodbye`.
/// struct Responses;
///
/// impl TemperResponses for Responses {
///     fn logout(&self, ctx: &TemperCtx) -> Result<Response> {
///         if ctx.wants_json() {
///             return smeltery::temper::DefaultResponses.logout(ctx);
///         }
///         Ok(Redirect::to("/goodbye").into_response())
///     }
/// }
/// # let _ = Responses;
/// ```
pub trait TemperResponses: Send + Sync + 'static {
    /// A user signed in.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn login(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(json(
                StatusCode::OK,
                serde_json::json!({ "two_factor": false }),
            ));
        }
        Ok(ctx.auth().intended(ctx.home()).into_response())
    }

    /// The address or password was wrong. `email` is the address that was typed (normalized), for the old input.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn failed_login(&self, ctx: &TemperCtx, email: &str) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(Error::validation("email", messages::FAILED_LOGIN).into_response());
        }
        flash_invalid(ctx, "email", messages::FAILED_LOGIN, &[("email", email)]);
        Ok(redirect(&ctx.route_or("login", "/login")))
    }

    /// A user registered and is signed in.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn register(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::CREATED));
        }
        Ok(redirect(ctx.home()))
    }

    /// A user signed out.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn logout(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::NO_CONTENT));
        }
        Ok(redirect(&ctx.route_or("home", "/")))
    }

    /// A reset link was asked for (the same answer whether or not the address has an account).
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn reset_link_sent(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(json(
                StatusCode::OK,
                serde_json::json!({ "message": messages::RESET_LINK_SENT }),
            ));
        }
        ctx.session().flash("status", messages::RESET_LINK_SENT);
        Ok(redirect(&ctx.route_or("password.request", ctx.back())))
    }

    /// A password was reset.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn password_reset(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(json(
                StatusCode::OK,
                serde_json::json!({ "message": messages::PASSWORD_RESET }),
            ));
        }
        ctx.session().flash("status", messages::PASSWORD_RESET);
        Ok(redirect(&ctx.route_or("login", "/login")))
    }

    /// A reset with a wrong, used or expired token (or an unknown address).
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn password_reset_failed(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(Error::validation("email", messages::RESET_FAILED).into_response());
        }
        ctx.session().flash("error", messages::RESET_FAILED);
        Ok(redirect(&ctx.route_or("password.request", ctx.back())))
    }

    /// The signed-in user's profile was updated.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn profile_updated(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::OK));
        }
        ctx.session().flash("status", messages::PROFILE_UPDATED);
        Ok(redirect(ctx.back()))
    }

    /// The signed-in user's password was updated.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn password_updated(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::OK));
        }
        ctx.session().flash("status", messages::PASSWORD_UPDATED);
        Ok(redirect(ctx.back()))
    }

    /// The signed-in user confirmed their password.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn password_confirmed(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::CREATED));
        }
        Ok(ctx.auth().intended(ctx.home()).into_response())
    }

    /// A verification link was opened; `newly` is `false` when the address was verified before.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn email_verified(&self, ctx: &TemperCtx, newly: bool) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::NO_CONTENT));
        }
        if newly {
            ctx.session().flash("status", messages::EMAIL_VERIFIED);
        }
        Ok(redirect(ctx.home()))
    }

    /// "Send the link again"; `sent` is `false` when nothing was sent (verified already, or no verification).
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn verification_link_sent(&self, ctx: &TemperCtx, sent: bool) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::ACCEPTED));
        }
        if sent {
            ctx.session()
                .flash("status", messages::VERIFICATION_LINK_SENT);
        }
        Ok(redirect(ctx.back()))
    }

    /// The password was right and a second factor is needed: the challenge.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn two_factor_challenge(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(json(
                StatusCode::OK,
                serde_json::json!({ "two_factor": true }),
            ));
        }
        Ok(redirect(
            &ctx.route_or("two-factor.login", "/two-factor-challenge"),
        ))
    }

    /// The second factor was right and the user is signed in.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn two_factor_login(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::NO_CONTENT));
        }
        Ok(ctx.auth().intended(ctx.home()).into_response())
    }

    /// A wrong code; `field` is `code` or `recovery_code`.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn two_factor_failed(&self, ctx: &TemperCtx, field: &str) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(Error::validation(field, messages::TWO_FACTOR_FAILED).into_response());
        }
        flash_invalid(ctx, field, messages::TWO_FACTOR_FAILED, &[]);
        Ok(redirect(
            &ctx.route_or("two-factor.login", "/two-factor-challenge"),
        ))
    }

    /// No valid pending login (none, expired, too many wrong codes, or the password changed): the password again.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn two_factor_expired(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(Error::validation("code", messages::TWO_FACTOR_EXPIRED).into_response());
        }
        ctx.session().flash("error", messages::TWO_FACTOR_EXPIRED);
        Ok(redirect(&ctx.route_or("login", "/login")))
    }

    /// Two-factor authentication was enabled; `codes` are the new recovery codes (also flashed for the next page).
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn two_factor_enabled(&self, ctx: &TemperCtx, codes: &[String]) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(private_json(serde_json::json!({ "recovery_codes": codes })));
        }
        ctx.session().flash("status", messages::TWO_FACTOR_ENABLED);
        Ok(redirect(ctx.back()))
    }

    /// Enabling was refused: a confirmed enrolment exists.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn two_factor_already_enabled(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(json(
                StatusCode::CONFLICT,
                serde_json::json!({ "message": messages::TWO_FACTOR_ALREADY_ENABLED }),
            ));
        }
        ctx.session()
            .flash("error", messages::TWO_FACTOR_ALREADY_ENABLED);
        Ok(redirect(ctx.back()))
    }

    /// The enrolment was confirmed with a first code.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn two_factor_confirmed(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::OK));
        }
        ctx.session()
            .flash("status", messages::TWO_FACTOR_CONFIRMED);
        Ok(redirect(ctx.back()))
    }

    /// Two-factor authentication was turned off.
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn two_factor_disabled(&self, ctx: &TemperCtx) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(empty(StatusCode::OK));
        }
        ctx.session().flash("status", messages::TWO_FACTOR_DISABLED);
        Ok(redirect(ctx.back()))
    }

    /// New recovery codes replaced the old ones (`codes`, also flashed for the next page).
    ///
    /// # Errors
    /// The override's own errors (the default never fails).
    fn recovery_codes_generated(&self, ctx: &TemperCtx, codes: &[String]) -> Result<Response> {
        if ctx.wants_json() {
            return Ok(private_json(serde_json::json!({ "recovery_codes": codes })));
        }
        ctx.session()
            .flash("status", messages::RECOVERY_CODES_GENERATED);
        Ok(redirect(ctx.back()))
    }
}

/// The default answers (every method of [`TemperResponses`] as it comes), for an override that answers some
/// requests itself and leaves the rest to the default.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultResponses;

impl TemperResponses for DefaultResponses {}
