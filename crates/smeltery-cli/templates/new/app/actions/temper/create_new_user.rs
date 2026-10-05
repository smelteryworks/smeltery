//! Registration: the form of `POST /register` and the account it creates. Temper then signs the user in, mails the
//! verification link (when e-mail verification is on) and opens the dashboard.

use serde::Deserialize;
use smeltery::auth::hash_password;
use smeltery::db::prelude::*;
use smeltery::temper::{CreatesNewUsers, TemperCtx};
use smeltery::{Result, Validate};

use super::password_rules::password_form;
use crate::app::models::{User, user};

password_form! {
    /// The registration form.
    #[derive(Debug, Deserialize, Validate)]
    pub struct RegisterForm {
        #[validate(required, max = 255)]
        pub name: String,
        /// Trimmed and lower-cased before the rules run, so `Ada@Example.com ` and `ada@example.com` are one account.
        #[validate(required, email, max = 255, unique(table = "users", column = "email"))]
        #[serde(deserialize_with = "smeltery::auth::deserialize_email")]
        pub email: String,
    }
}

/// Creates the account from the registration form.
pub struct CreateNewUser;

impl CreatesNewUsers<User> for CreateNewUser {
    type Input = RegisterForm;

    async fn create(&self, ctx: &TemperCtx, input: RegisterForm) -> Result<User> {
        User::create(
            &ctx.db()?,
            user::ActiveModel {
                name: Set(input.name),
                email: Set(input.email),
                password: Set(hash_password(&input.password).await?),
                ..Default::default()
            },
        )
        .await
    }
}
