//! The `Event` model (table `events`).

use smeltery::db::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub name: String,
    pub notes: Option<String>,
    pub seats: i32,
    pub views: Option<i64>,
    pub open: bool,
    pub price: Option<f64>,
    pub day: Date,
    pub starts_at: Option<DateTimeUtc>,
    pub meta: Json,
    pub code: Uuid,
    pub user_id: i64,
    pub created_at: Option<DateTimeUtc>,
    pub updated_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
