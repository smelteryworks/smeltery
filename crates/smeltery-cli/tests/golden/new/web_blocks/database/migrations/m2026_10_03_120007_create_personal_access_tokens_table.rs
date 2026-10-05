//! Create the `personal_access_tokens` table: the app's API tokens (Hallmark).

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};

/// Creates `personal_access_tokens`.
pub struct CreatePersonalAccessTokensTable;

impl Migration for CreatePersonalAccessTokensTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120007_create_personal_access_tokens_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        smeltery::hallmark::migrations::up(schema).await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        smeltery::hallmark::migrations::down(schema).await
    }
}
