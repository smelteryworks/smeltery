//! Create the `comments` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `comments`.
pub struct CreateCommentsTable;

impl Migration for CreateCommentsTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120000_create_comments_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("comments", |t| {
                t.id();
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("comments").await
    }
}
