//! Create Watchfire's tables: agents, runs, checkpoints, the job queue and dead letters.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates the `watchfire_*` tables.
pub struct CreateWatchfireTables;

impl Migration for CreateWatchfireTables {
    fn name(&self) -> &'static str {
        "2026_10_04_004657_create_watchfire_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        smeltery::watchfire::migrations::up(schema).await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        smeltery::watchfire::migrations::down(schema).await
    }
}
