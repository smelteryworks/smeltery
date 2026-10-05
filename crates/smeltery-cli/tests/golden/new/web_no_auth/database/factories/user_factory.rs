//! Fake `User` records for tests and seeders.

use smeltery::db::factory::{Factory, Fake};
use smeltery::db::prelude::*;

use crate::app::models::user;

/// The argon2id hash of `password`: every factory user logs in with that password, without hashing per row.
const PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$ZEUDxrzcLbvVNEBt+Cy1cw$gHSlbWqZaIZWntXjxWoscU7+7xgSqFXP7aS89zJPv94";

/// Builds users with a fake name, a unique e-mail address and the password `password`.
pub struct UserFactory;

impl Factory for UserFactory {
    type Entity = user::Entity;

    fn definition(&self, fake: &mut Fake) -> user::ActiveModel {
        user::ActiveModel {
            name: Set(fake.name()),
            email: Set(fake.unique_email()),
            password: Set(PASSWORD_HASH.to_owned()),
            ..Default::default()
        }
    }
}
