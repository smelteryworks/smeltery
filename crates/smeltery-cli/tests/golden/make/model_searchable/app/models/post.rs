//! The `Post` model (table `posts`).

use smeltery::db::prelude::*;
use smeltery::prospect::{IndexSpec, Searchable, Weight};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "posts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub title: String,
    pub body: Option<String>,
    pub user_id: Option<i64>,
    pub created_at: Option<DateTimeUtc>,
    pub updated_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

/// Full-text search (Prospect, registered in `app/providers/search.rs`). The search index comes from a migration
/// whose `SearchIndex` names the same columns and weights: change both together (`SearchIndex::rebuild`).
impl Searchable for Model {
    fn index(i: &mut IndexSpec) {
        i.text("title").weight(Weight::A);
        i.text("body");
        i.filter("user_id");
    }
}
