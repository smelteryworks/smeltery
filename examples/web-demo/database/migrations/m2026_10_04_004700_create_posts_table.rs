//! Create the `posts` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `posts`.
pub struct CreatePostsTable;

impl Migration for CreatePostsTable {
    fn name(&self) -> &'static str {
        "2026_10_04_004700_create_posts_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("posts", |t| {
                t.id();
                t.string("title");
                t.text("body");
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("posts").await
    }
}
