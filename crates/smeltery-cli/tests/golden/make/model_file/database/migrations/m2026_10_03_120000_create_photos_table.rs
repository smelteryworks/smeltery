//! Create the `photos` table.

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `photos`.
pub struct CreatePhotosTable;

impl Migration for CreatePhotosTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120000_create_photos_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("photos", |t| {
                t.id();
                t.string("title");
                t.string("image");
                t.string("scan").nullable();
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("photos").await
    }
}
