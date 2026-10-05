//! Add columns to the `posts` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Adds columns to `posts`.
pub struct AddImageToPostsTable;

impl Migration for AddImageToPostsTable {
    fn name(&self) -> &'static str {
        "2026_10_04_004701_add_image_to_posts_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .table("posts", |t| {
                // The new columns of `posts`.
                t.string("image").nullable();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.raw("ALTER TABLE posts DROP COLUMN image").await
    }
}
