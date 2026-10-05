//! Create the `posts` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};
use smeltery::prospect::migration::{SearchIndex, Weight};

/// Creates `posts`.
pub struct CreatePostsTable;

impl Migration for CreatePostsTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120000_create_posts_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("posts", |t| {
                t.id();
                t.string("title");
                t.text("body").nullable();
                t.foreign_id("user_id").constrained("users").nullable();
                t.timestamps();
            })
            .await?;
        // The full-text search index (Prospect): the columns and weights of `impl Searchable`.
        SearchIndex::on("posts")
            .text("title", Weight::A)
            .text("body", Weight::B)
            .create(schema)
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        SearchIndex::on("posts").drop(schema).await?;
        schema.drop_if_exists("posts").await
    }
}
