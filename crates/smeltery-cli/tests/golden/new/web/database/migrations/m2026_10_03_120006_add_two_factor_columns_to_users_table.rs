//! Add the two-factor authentication columns to `users` (Temper's `TwoFactorAuthenticatable`).

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Adds the secret (encrypted), the recovery codes (hashed), the confirmation time and the last accepted time step.
pub struct AddTwoFactorColumnsToUsersTable;

impl Migration for AddTwoFactorColumnsToUsersTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120006_add_two_factor_columns_to_users_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .table("users", |t| {
                t.text("two_factor_secret").nullable();
                t.text("two_factor_recovery_codes").nullable();
                t.datetime("two_factor_confirmed_at").nullable();
                t.big_integer("two_factor_last_step").nullable();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema
            .table("users", |t| {
                t.drop_column("two_factor_secret");
                t.drop_column("two_factor_recovery_codes");
                t.drop_column("two_factor_confirmed_at");
                t.drop_column("two_factor_last_step");
            })
            .await
    }
}
