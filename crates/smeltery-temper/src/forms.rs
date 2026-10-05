//! The forms Temper reads itself (the app's actions bring their own).

use serde::Deserialize;
use smeltery_macros::Validate;

/// `POST /login`.
#[derive(Debug, Deserialize, Validate)]
#[validate(crate = "smeltery_core::validation")]
pub(crate) struct LoginForm {
    #[validate(required, email)]
    #[serde(deserialize_with = "smeltery_core::auth::deserialize_email")]
    pub(crate) email: String,
    #[validate(required)]
    pub(crate) password: String,
    /// The "remember me" checkbox.
    #[serde(default)]
    pub(crate) remember: bool,
}

/// `POST /forgot-password`.
#[derive(Debug, Deserialize, Validate)]
#[validate(crate = "smeltery_core::validation")]
pub(crate) struct EmailForm {
    #[validate(required, email)]
    #[serde(deserialize_with = "smeltery_core::auth::deserialize_email")]
    pub(crate) email: String,
}

/// `POST /user/confirm-password`.
#[derive(Debug, Deserialize, Validate)]
#[validate(crate = "smeltery_core::validation")]
pub(crate) struct ConfirmForm {
    #[validate(required)]
    pub(crate) password: String,
}

/// The query string of a reset link.
#[derive(Debug, Deserialize)]
pub(crate) struct ResetQuery {
    pub(crate) email: Option<String>,
}

/// `POST /two-factor-challenge`: one of the two.
#[derive(Debug, Deserialize, Validate)]
#[validate(crate = "smeltery_core::validation")]
pub(crate) struct ChallengeForm {
    pub(crate) code: Option<String>,
    pub(crate) recovery_code: Option<String>,
}

/// `POST /user/confirmed-two-factor-authentication`.
#[derive(Debug, Deserialize, Validate)]
#[validate(crate = "smeltery_core::validation")]
pub(crate) struct CodeForm {
    #[validate(required)]
    pub(crate) code: String,
}
