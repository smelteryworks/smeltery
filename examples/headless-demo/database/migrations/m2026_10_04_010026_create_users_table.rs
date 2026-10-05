//! Create the `users` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `users`.
pub struct CreateUsersTable;

impl Migration for CreateUsersTable {
    fn name(&self) -> &'static str {
        "2026_10_04_010026_create_users_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("users", |t| {
                t.id();
                t.string("name");
                t.string("email").unique();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("users").await
    }
}
