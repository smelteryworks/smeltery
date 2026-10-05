//! Password reset: ask for a link (`/forgot-password`), then set a new password (`/reset-password/{token}`).
//!
//! The reset link is mailed as the framework's `ResetPassword` mail (with `MAIL_MAILER=log` it is written to the log).

use smeltery::auth::passwords;
use smeltery::http::{Path, Query, Redirect};
use smeltery::session::Session;
use smeltery::validation::Valid;
use smeltery::{App, Result, Validate};

/// `resources/views/auth/forgot-password.mold.html`.
#[derive(smeltery::Mold)]
#[mold("auth/forgot-password")]
pub struct ForgotPasswordPage {}

/// `resources/views/auth/reset-password.mold.html`.
#[derive(smeltery::Mold)]
#[mold("auth/reset-password")]
pub struct ResetPasswordPage {
    /// The token from the link.
    pub token: String,
    /// The e-mail address from the link.
    pub email: String,
}

/// The forgot-password form.
#[derive(Debug, serde::Deserialize, Validate)]
pub struct EmailForm {
    #[validate(required, email)]
    pub email: String,
}

/// The query string of a reset link.
#[derive(Debug, serde::Deserialize)]
pub struct ResetQuery {
    /// The account's e-mail address.
    pub email: Option<String>,
}

/// The reset-password form.
#[derive(Debug, serde::Deserialize, Validate)]
pub struct ResetForm {
    #[validate(required, email)]
    pub email: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: Option<String>,
}

/// Shows the forgot-password form.
pub async fn request() -> ForgotPasswordPage {
    ForgotPasswordPage {}
}

/// Creates a reset link for the address. The answer is the same whether or not an account exists.
pub async fn email(app: App, session: Session, Valid(form): Valid<EmailForm>) -> Result<Redirect> {
    passwords::send_reset_link(&app, &form.email).await?;
    session.flash(
        "status",
        "If that e-mail address has an account, a password reset link has been sent to it.",
    );
    Ok(Redirect::to(&app.url("password.request", &[])?))
}

/// Shows the reset-password form for the token in the link.
pub async fn edit(Path(token): Path<String>, Query(query): Query<ResetQuery>) -> ResetPasswordPage {
    ResetPasswordPage {
        token,
        email: query.email.unwrap_or_default(),
    }
}

/// Sets the new password when the token is valid.
pub async fn update(
    app: App,
    session: Session,
    Path(token): Path<String>,
    Valid(form): Valid<ResetForm>,
) -> Result<Redirect> {
    if passwords::reset(&app, &form.email, &token, &form.password)
        .await?
        .is_some()
    {
        session.flash(
            "status",
            "Your password has been reset. Log in with the new password.",
        );
        return Ok(Redirect::to(&app.url("login", &[])?));
    }
    session.flash(
        "error",
        "This password reset link is invalid or has expired.",
    );
    Ok(Redirect::to(&app.url("password.request", &[])?))
}
