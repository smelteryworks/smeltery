//! Add the search index of the `articles` table (Prospect).

use smeltery::Result;
use smeltery::db::migration::{Migration, Schema};
use smeltery::prospect::migration::{SearchIndex, Weight};

/// Creates the search index of `articles`.
pub struct AddSearchIndexToArticlesTable;

impl Migration for AddSearchIndexToArticlesTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120001_add_search_index_to_articles_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        // The columns and weights of `impl Searchable`; the rows already in the table are indexed too.
        SearchIndex::on("articles")
            .text("title", Weight::A)
            .create(schema)
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        SearchIndex::on("articles").drop(schema).await
    }
}
