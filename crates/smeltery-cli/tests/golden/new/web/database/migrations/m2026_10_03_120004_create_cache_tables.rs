//! Create the `cache` and `cache_locks` tables, used when `CACHE_STORE=database`.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `cache` and `cache_locks`.
pub struct CreateCacheTables;

impl Migration for CreateCacheTables {
    fn name(&self) -> &'static str {
        "2026_10_03_120004_create_cache_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        smeltery::cache::migrations::up(schema).await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        smeltery::cache::migrations::down(schema).await
    }
}
