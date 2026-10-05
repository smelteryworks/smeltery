//! Temper, the authentication routes: login and logout, registration, password reset, e-mail verification, password
//! confirmation, profile and password updates and two-factor authentication. The forms and what they do are in
//! `app/actions/temper/`, the pages in `resources/views/auth/`; `smeltery route:list` lists the routes.

use smeltery::temper::{Temper, TemperViews, TwoFactor, ViewCtx};

use crate::app::actions::temper::{
    CreateNewUser, ResetUserPassword, UpdateUserPassword, UpdateUserProfileInformation,
};
use crate::app::models::User;

/// The features of this app; leaving one out removes its routes.
pub fn temper() -> Temper<User> {
    Temper::new()
        .registration(CreateNewUser)
        .reset_passwords(ResetUserPassword)
        .email_verification()
        .update_profile_information(UpdateUserProfileInformation)
        .update_passwords(UpdateUserPassword)
        .two_factor(TwoFactor::new())
        .views(views())
}

/// The pages of Temper's `GET` routes.
pub fn views() -> TemperViews {
    TemperViews::new()
        .login(|_| LoginPage {})
        .register(|_| RegisterPage {})
        .forgot_password(|_| ForgotPasswordPage {})
        // The token and address come from the link as sent: untrusted, and the view escapes them.
        .reset_password(|ctx: ViewCtx| ResetPasswordPage {
            token: ctx.token(),
            email: ctx.email(),
        })
        .verify_email(|_| VerifyEmailPage {})
        .confirm_password(|_| ConfirmPasswordPage {})
        .two_factor_challenge(|_| TwoFactorChallengePage {})
}

/// `resources/views/auth/login.mold.html`.
#[derive(smeltery::Mold)]
#[mold("auth/login")]
pub struct LoginPage {}

/// `resources/views/auth/register.mold.html`.
#[derive(smeltery::Mold)]
#[mold("auth/register")]
pub struct RegisterPage {}

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

/// `resources/views/auth/verify-email.mold.html`.
#[derive(smeltery::Mold)]
#[mold("auth/verify-email")]
pub struct VerifyEmailPage {}

/// `resources/views/auth/confirm-password.mold.html`.
#[derive(smeltery::Mold)]
#[mold("auth/confirm-password")]
pub struct ConfirmPasswordPage {}

/// `resources/views/auth/two-factor-challenge.mold.html`.
#[derive(smeltery::Mold)]
#[mold("auth/two-factor-challenge")]
pub struct TwoFactorChallengePage {}
