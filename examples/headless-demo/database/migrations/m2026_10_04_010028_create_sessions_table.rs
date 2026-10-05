//! Create the `sessions` table, used when `SESSION_DRIVER=database`.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `sessions`.
pub struct CreateSessionsTable;

impl Migration for CreateSessionsTable {
    fn name(&self) -> &'static str {
        "2026_10_04_010028_create_sessions_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("sessions", |t| {
                t.string("id").unique();
                t.text("payload");
                t.big_integer("last_activity").index();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("sessions").await
    }
}
