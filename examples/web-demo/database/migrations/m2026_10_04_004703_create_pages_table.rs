//! Create the `pages` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `pages`.
pub struct CreatePagesTable;

impl Migration for CreatePagesTable {
    fn name(&self) -> &'static str {
        "2026_10_04_004703_create_pages_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("pages", |t| {
                t.id();
                t.string("url").unique();
                t.string("title");
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("pages").await
    }
}
