//! The `backfill_slugs` migration.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Changes the schema.
pub struct BackfillSlugs;

impl Migration for BackfillSlugs {
    fn name(&self) -> &'static str {
        "2026_10_03_120000_backfill_slugs"
    }

    async fn up(&self, _schema: &Schema) -> Result<()> {
        // Change the schema here, e.g. `_schema.create("table", |t| { … }).await`.
        Ok(())
    }

    async fn down(&self, _schema: &Schema) -> Result<()> {
        // Undo `up` here.
        Ok(())
    }
}
