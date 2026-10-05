//! Create the `password_reset_tokens` table: one pending reset per account.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `password_reset_tokens`.
pub struct CreatePasswordResetTokensTable;

impl Migration for CreatePasswordResetTokensTable {
    fn name(&self) -> &'static str {
        "2026_10_04_010027_create_password_reset_tokens_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("password_reset_tokens", |t| {
                t.foreign_id("user_id")
                    .unique()
                    .constrained("users")
                    .cascade_on_delete();
                t.string("token");
                t.datetime("created_at").nullable();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("password_reset_tokens").await
    }
}
