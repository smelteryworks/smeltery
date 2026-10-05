//! Password reset: the form of `POST /reset-password/{token}`. Temper checks the token, stores the new password and
//! ends every session of the user; `reset` runs after that for anything the app adds.

use serde::Deserialize;
use smeltery::Validate;
use smeltery::temper::{PasswordInput, ResetsUserPasswords};

use super::password_rules::password_form;
use crate::app::models::User;

password_form! {
    /// The reset form.
    #[derive(Debug, Deserialize, Validate)]
    pub struct ResetForm {
        #[validate(required, email)]
        #[serde(deserialize_with = "smeltery::auth::deserialize_email")]
        pub email: String,
    }
}

impl PasswordInput for ResetForm {
    fn email(&self) -> &str {
        &self.email
    }

    fn password(&self) -> &str {
        &self.password
    }
}

/// The password reset (Temper does the reset itself).
pub struct ResetUserPassword;

impl ResetsUserPasswords<User> for ResetUserPassword {
    type Input = ResetForm;
}
