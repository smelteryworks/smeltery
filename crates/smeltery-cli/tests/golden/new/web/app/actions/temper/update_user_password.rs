//! Password change: the form of `PUT /user/password` (`/settings/password`). Temper checks `current_password`, stores
//! the new password and ends the user's other sessions; this device stays signed in.

use serde::Deserialize;
use smeltery::Validate;
use smeltery::temper::{UpdatePasswordInput, UpdatesUserPasswords};

use super::password_rules::password_form;
use crate::app::models::User;

password_form! {
    /// The password change form.
    #[derive(Debug, Deserialize, Validate)]
    pub struct PasswordForm {
        #[validate(required)]
        pub current_password: String,
    }
}

impl UpdatePasswordInput for PasswordForm {
    fn current_password(&self) -> &str {
        &self.current_password
    }

    fn password(&self) -> &str {
        &self.password
    }
}

/// The password change (Temper stores the password itself).
pub struct UpdateUserPassword;

impl UpdatesUserPasswords<User> for UpdateUserPassword {
    type Input = PasswordForm;
}
