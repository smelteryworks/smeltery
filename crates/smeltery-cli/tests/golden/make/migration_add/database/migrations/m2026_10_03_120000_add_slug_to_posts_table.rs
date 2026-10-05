//! Add columns to the `posts` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Adds columns to `posts`.
pub struct AddSlugToPostsTable;

impl Migration for AddSlugToPostsTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120000_add_slug_to_posts_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .table("posts", |t| {
                // The new columns of `posts`.
                t.string("slug").nullable();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        // Drops the column `up` adds; add a `DROP COLUMN` here for every column added there.
        let sql = "ALTER TABLE posts DROP COLUMN slug";
        schema.raw(sql).await
    }
}
