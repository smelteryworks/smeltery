//! Create the `metrics` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `metrics`.
pub struct CreateMetricsTable;

impl Migration for CreateMetricsTable {
    fn name(&self) -> &'static str {
        "2026_10_04_004702_create_metrics_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("metrics", |t| {
                t.id();
                t.string("name").unique();
                t.big_integer("value").default(0);
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("metrics").await
    }
}
