//! The actions: the app's own code for the parts of a flow that differ between apps (which fields a user has, the
//! password rules). Each takes a typed input that the app's `#[derive(Validate)]` rules check before the action runs.

use std::future::Future;

use serde::de::DeserializeOwned;
use smeltery_core::validation::Validate;
use smeltery_core::{Error, Result};

use crate::TemperCtx;

/// Creates users: `Temper::registration(action)`.
///
/// After [`create`](Self::create) returns the user, Temper signs them in, sends the verification link (with
/// `.email_verification()` and core's `.verify_email::<User>()`) and fires
/// [`TemperEvent::Registered`](crate::TemperEvent::Registered).
///
/// ```
/// # mod user {
/// #     use smeltery::db::prelude::*;
/// #     #[sea_orm::model]
/// #     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
/// #     #[sea_orm(table_name = "users")]
/// #     pub struct Model {
/// #         #[sea_orm(primary_key)]
/// #         pub id: i64,
/// #         pub name: String,
/// #         pub email: String,
/// #         pub password: String,
/// #         pub remember_token: Option<String>,
/// #     }
/// #     impl ActiveModelBehavior for ActiveModel {}
/// #     impl smeltery::auth::Authenticatable for Model {
/// #         fn auth_id(&self) -> i64 { self.id }
/// #         fn password_hash(&self) -> &str { &self.password }
/// #         fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
/// #     }
/// # }
/// # use user::Model as User;
/// use smeltery::auth::hash_password;
/// use smeltery::db::prelude::*;
/// use smeltery::temper::{CreatesNewUsers, TemperCtx};
/// use smeltery::{Result, Validate};
///
/// #[derive(Deserialize, Validate)]
/// pub struct RegisterForm {
///     #[validate(required, max = 255)]
///     pub name: String,
///     #[validate(required, email, max = 255, unique(table = "users", column = "email"))]
///     #[serde(deserialize_with = "smeltery::auth::deserialize_email")]
///     pub email: String,
///     #[validate(required, min = 8, confirmed)]
///     pub password: String,
///     pub password_confirmation: Option<String>,
/// }
///
/// pub struct CreateNewUser;
///
/// impl CreatesNewUsers<User> for CreateNewUser {
///     type Input = RegisterForm;
///
///     async fn create(&self, ctx: &TemperCtx, input: RegisterForm) -> Result<User> {
///         User::create(
///             &ctx.db()?,
///             user::ActiveModel {
///                 name: Set(input.name),
///                 email: Set(input.email),
///                 password: Set(hash_password(&input.password).await?),
///                 ..Default::default()
///             },
///         )
///         .await
///     }
/// }
/// # fn main() {}
/// ```
pub trait CreatesNewUsers<U>: Send + Sync + 'static {
    /// The registration form (validated before [`create`](Self::create) runs; a failure answers 422 to JSON
    /// clients and redirects back with the messages otherwise).
    type Input: DeserializeOwned + Validate + Send + 'static;

    /// Create and return the user.
    fn create(&self, ctx: &TemperCtx, input: Self::Input)
    -> impl Future<Output = Result<U>> + Send;

    /// Create a user who signs in through another service (social login): no password is given, and the
    /// address counts as verified only when [`SocialUser::email_verified`] says the service verified it. The
    /// default refuses (an app without social login never calls it).
    ///
    /// # Errors
    /// The default always fails: implement it to create users this way.
    fn create_social(
        &self,
        _ctx: &TemperCtx,
        _user: SocialUser,
    ) -> impl Future<Output = Result<U>> + Send {
        async {
            Err(Error::internal(
                "this app's `CreatesNewUsers` action does not create users without a password: implement \
                 `create_social`",
            ))
        }
    }
}

/// A user from another service (social login), for [`CreatesNewUsers::create_social`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SocialUser {
    /// The service (`github`, `google`, …).
    pub provider: String,
    /// The user's id at the service.
    pub provider_id: String,
    /// The address the service gave (trimmed, in lower case).
    pub email: String,
    /// Whether the service verified the address.
    pub email_verified: bool,
    /// The display name the service gave, if any.
    pub name: Option<String>,
}

impl SocialUser {
    /// A user of `provider` with id `provider_id` and address `email` (trimmed and lower-cased here), not
    /// verified and without a name.
    pub fn new(
        provider: impl Into<String>,
        provider_id: impl Into<String>,
        email: impl AsRef<str>,
    ) -> Self {
        Self {
            provider: provider.into(),
            provider_id: provider_id.into(),
            email: email.as_ref().trim().to_lowercase(),
            email_verified: false,
            name: None,
        }
    }

    /// Mark the address as verified by the service.
    #[must_use]
    pub fn verified(mut self, verified: bool) -> Self {
        self.email_verified = verified;
        self
    }

    /// Set the display name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// A reset-password form: the address and the new password.
pub trait PasswordInput {
    /// The address the reset link was sent to.
    fn email(&self) -> &str;
    /// The new password.
    fn password(&self) -> &str;
}

/// Resets passwords: `Temper::reset_passwords(action)`.
///
/// Temper checks the token and stores the new password through core
/// ([`passwords::reset`](smeltery_core::auth::passwords::reset): every session and remember-me cookie of the user
/// ends) before [`reset`](Self::reset) runs; the action holds the password rules (its `Input`'s validation) and any
/// app-specific side effect.
pub trait ResetsUserPasswords<U>: Send + Sync + 'static {
    /// The reset form: `email`, `password` and the app's rules (`#[validate(required, min = 8, confirmed)]`).
    type Input: PasswordInput + DeserializeOwned + Validate + Send + Sync + 'static;

    /// Runs after the password was reset. The default does nothing.
    fn reset(
        &self,
        _ctx: &TemperCtx,
        _user: &U,
        _input: &Self::Input,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }
}

/// A password-update form: the current password and the new one.
pub trait UpdatePasswordInput {
    /// The current password.
    fn current_password(&self) -> &str;
    /// The new password.
    fn password(&self) -> &str;
}

/// Updates the signed-in user's password: `Temper::update_passwords(action)`.
///
/// Temper checks `current_password` (through core's password confirmation, five tries a minute per user) and
/// stores the new password with [`Auth::set_password`](smeltery_core::auth::Auth::set_password) (this device stays
/// signed in, every other session and remember-me cookie ends) before [`update`](Self::update) runs.
pub trait UpdatesUserPasswords<U>: Send + Sync + 'static {
    /// The form: `current_password`, `password` and the app's rules.
    type Input: UpdatePasswordInput + DeserializeOwned + Validate + Send + Sync + 'static;

    /// Runs after the password was stored. The default does nothing.
    fn update(
        &self,
        _ctx: &TemperCtx,
        _user: &U,
        _input: &Self::Input,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }
}

/// Whether a profile update changed the e-mail address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmailChanged {
    /// The address changed: with `.email_verification()` Temper marks it unverified and sends a new link.
    Yes,
    /// The address stayed.
    No,
}

/// Updates the signed-in user's profile: `Temper::update_profile_information(action)`.
pub trait UpdatesUserProfileInformation<U>: Send + Sync + 'static {
    /// The profile form (the app's fields and rules).
    type Input: DeserializeOwned + Validate + Send + 'static;

    /// Write the profile of `user`; return [`EmailChanged::Yes`] when the address changed.
    fn update(
        &self,
        ctx: &TemperCtx,
        user: &U,
        input: Self::Input,
    ) -> impl Future<Output = Result<EmailChanged>> + Send;
}
