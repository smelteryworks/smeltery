//! Create the `events` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `events`.
pub struct CreateEventsTable;

impl Migration for CreateEventsTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120000_create_events_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("events", |t| {
                t.id();
                t.string("name");
                t.text("notes").nullable();
                t.integer("seats");
                t.big_integer("views").nullable();
                t.boolean("open");
                t.double("price").nullable();
                t.date("day");
                t.datetime("starts_at").nullable();
                t.json("meta");
                t.uuid("code");
                t.foreign_id("user_id").constrained("users");
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("events").await
    }
}
