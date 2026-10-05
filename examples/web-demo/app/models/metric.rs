//! The `Metric` model (table `metrics`).

use sea_orm::sea_query::ExprTrait as _;
use smeltery::db::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "metrics")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub name: String,
    pub value: i64,
    pub created_at: Option<DateTimeUtc>,
    pub updated_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// The value of the metric `name` (0 when it has no row yet).
    pub async fn current(db: &Db, name: &str) -> smeltery::Result<i64> {
        let row = Entity::find()
            .filter(Column::Name.eq(name))
            .one(db.conn())
            .await?;
        Ok(row.map_or(0, |m| m.value))
    }

    /// Adds one to the metric `name` (creating it at 1) and returns the new value. The increment is one
    /// `UPDATE … SET value = value + 1`, so two workers never lose a count.
    pub async fn increment(db: &Db, name: &str) -> smeltery::Result<i64> {
        let updated = Entity::update_many()
            .col_expr(Column::Value, Expr::col(Column::Value).add(1))
            .col_expr(Column::UpdatedAt, Expr::value(ChronoUtc::now()))
            .filter(Column::Name.eq(name))
            .exec(db.conn())
            .await?;
        if updated.rows_affected == 0 {
            Self::create(
                db,
                ActiveModel {
                    name: Set(name.to_owned()),
                    value: Set(1),
                    ..Default::default()
                },
            )
            .await?;
        }
        Self::current(db, name).await
    }
}
