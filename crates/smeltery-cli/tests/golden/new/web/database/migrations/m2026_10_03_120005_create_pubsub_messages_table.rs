//! Create the `pubsub_messages` table, used when `PUBSUB_DRIVER` is `database` (or `auto` chooses it).

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `pubsub_messages`.
pub struct CreatePubsubMessagesTable;

impl Migration for CreatePubsubMessagesTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120005_create_pubsub_messages_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        smeltery::pubsub::migrations::up(schema).await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        smeltery::pubsub::migrations::down(schema).await
    }
}
