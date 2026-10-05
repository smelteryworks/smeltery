//! The `User` model (table `users`): the accounts that log in.

use smeltery::db::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub name: String,
    #[sea_orm(unique)]
    pub email: String,
    /// The argon2id hash of the password (`smeltery::auth::hash_password`), never the password itself.
    #[serde(skip_serializing)]
    pub password: String,
    #[serde(skip_serializing)]
    pub remember_token: Option<String>,
    pub created_at: Option<DateTimeUtc>,
    pub updated_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

impl smeltery::auth::Authenticatable for Model {
    fn auth_id(&self) -> i64 {
        self.id
    }

    fn password_hash(&self) -> &str {
        &self.password
    }

    fn remember_token(&self) -> Option<&str> {
        self.remember_token.as_deref()
    }
}
