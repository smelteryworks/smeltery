//! Profile: the form of `PUT /user/profile-information` (`/settings/profile`) and the columns it writes. When the
//! address changes and e-mail verification is on, Temper marks it unverified and mails a link to the new one.

use serde::Deserialize;
use smeltery::db::prelude::*;
use smeltery::temper::{EmailChanged, TemperCtx, UpdatesUserProfileInformation};
use smeltery::{Error, Result, Validate};

use crate::app::models::{User, user};

/// The profile form.
#[derive(Debug, Deserialize, Validate)]
pub struct ProfileForm {
    #[validate(required, max = 255)]
    pub name: String,
    #[validate(required, email, max = 255)]
    #[serde(deserialize_with = "smeltery::auth::deserialize_email")]
    pub email: String,
}

/// Writes the name and the e-mail address.
pub struct UpdateUserProfileInformation;

impl UpdatesUserProfileInformation<User> for UpdateUserProfileInformation {
    type Input = ProfileForm;

    async fn update(
        &self,
        ctx: &TemperCtx,
        user: &User,
        input: ProfileForm,
    ) -> Result<EmailChanged> {
        let db = ctx.db()?;
        let changed = input.email != user.email;
        // Another account's address is refused like the registration form's `unique` rule.
        if changed
            && User::query()
                .filter(user::Column::Email.eq(input.email.as_str()))
                .filter(user::Column::Id.ne(user.id))
                .count(db.conn())
                .await?
                > 0
        {
            return Err(Error::validation(
                "email",
                "The email has already been taken.",
            ));
        }
        user.update(&db, |m| {
            m.name = Set(input.name);
            m.email = Set(input.email);
        })
        .await?;
        Ok(if changed {
            EmailChanged::Yes
        } else {
            EmailChanged::No
        })
    }
}
