//! The `PostComment` model (table `post_comments`).

use smeltery::db::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "post_comments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub body: String,
    pub created_at: Option<DateTimeUtc>,
    pub updated_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
