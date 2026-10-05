//! The two-factor columns, written by the user's id through the model's own entity (never a literal table), each
//! change that two requests could race on as one conditional `UPDATE`.

use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, Iterable, PrimaryKeyToColumn, QueryFilter, Value,
};
use smeltery_core::auth::model_column;
use smeltery_core::db::Record;
use smeltery_core::{App, Error, Result};

type Column<U> = <<U as Record>::Entity as EntityTrait>::Column;

fn key<U: Record>() -> Result<Column<U>> {
    Ok(<<U::Entity as EntityTrait>::PrimaryKey as Iterable>::iter()
        .next()
        .ok_or_else(|| Error::internal("the user model has no primary key"))?
        .into_column())
}

/// `UPDATE … SET <sets> WHERE id = :id AND <condition>`: the number of rows changed.
async fn update<U: Record>(
    app: &App,
    id: i64,
    sets: Vec<(&str, Value)>,
    condition: Condition,
) -> Result<u64> {
    let mut update = <U::Entity as EntityTrait>::update_many();
    for (name, value) in sets {
        update = update.col_expr(model_column::<U>(name)?, Expr::value(value));
    }
    let result = update
        .filter(key::<U>()?.eq(id))
        .filter(condition)
        .exec(app.db()?.conn())
        .await?;
    Ok(result.rows_affected)
}

fn at(unix: i64) -> Result<Value> {
    let at = sea_orm::prelude::DateTimeUtc::from_timestamp(unix, 0)
        .ok_or_else(|| Error::internal("the clock is out of range"))?;
    Ok(Value::from(Some(at)))
}

/// A new secret and recovery codes; confirmed at `confirmed_at` (or not confirmed yet); no step used.
pub(crate) async fn enable<U: Record>(
    app: &App,
    id: i64,
    secret: String,
    codes: String,
    confirmed_at: Option<i64>,
) -> Result<()> {
    let confirmed = match confirmed_at {
        Some(unix) => at(unix)?,
        None => Value::from(None::<sea_orm::prelude::DateTimeUtc>),
    };
    update::<U>(
        app,
        id,
        vec![
            ("two_factor_secret", Value::from(Some(secret))),
            ("two_factor_recovery_codes", Value::from(Some(codes))),
            ("two_factor_confirmed_at", confirmed),
            ("two_factor_last_step", Value::from(None::<i64>)),
        ],
        Condition::all(),
    )
    .await?;
    Ok(())
}

/// Mark the enrolment confirmed (only while it is not).
pub(crate) async fn confirm<U: Record>(app: &App, id: i64, now: i64) -> Result<bool> {
    let column = model_column::<U>("two_factor_confirmed_at")?;
    Ok(update::<U>(
        app,
        id,
        vec![("two_factor_confirmed_at", at(now)?)],
        Condition::all().add(column.is_null()),
    )
    .await?
        > 0)
}

/// Clear the four columns.
pub(crate) async fn disable<U: Record>(app: &App, id: i64) -> Result<()> {
    update::<U>(
        app,
        id,
        vec![
            ("two_factor_secret", Value::from(None::<String>)),
            ("two_factor_recovery_codes", Value::from(None::<String>)),
            (
                "two_factor_confirmed_at",
                Value::from(None::<sea_orm::prelude::DateTimeUtc>),
            ),
            ("two_factor_last_step", Value::from(None::<i64>)),
        ],
        Condition::all(),
    )
    .await?;
    Ok(())
}

/// Accept time step `step` for user `id` only when it is later than the last accepted one: one conditional
/// `UPDATE`, so of two requests with one code exactly one wins.
pub(crate) async fn accept_step<U: Record>(app: &App, id: i64, step: i64) -> Result<bool> {
    let column = model_column::<U>("two_factor_last_step")?;
    Ok(update::<U>(
        app,
        id,
        vec![("two_factor_last_step", Value::from(Some(step)))],
        Condition::any().add(column.is_null()).add(column.lt(step)),
    )
    .await?
        > 0)
}

/// Replace the stored recovery codes `old` with `new`, only while they are still `old` (compare-and-set): of two
/// requests with one code exactly one wins.
pub(crate) async fn swap_codes<U: Record>(
    app: &App,
    id: i64,
    old: &str,
    new: String,
) -> Result<bool> {
    let column = model_column::<U>("two_factor_recovery_codes")?;
    Ok(update::<U>(
        app,
        id,
        vec![("two_factor_recovery_codes", Value::from(Some(new)))],
        Condition::all().add(column.eq(old)),
    )
    .await?
        > 0)
}

/// Replace every recovery code.
pub(crate) async fn set_codes<U: Record>(app: &App, id: i64, codes: String) -> Result<()> {
    update::<U>(
        app,
        id,
        vec![("two_factor_recovery_codes", Value::from(Some(codes)))],
        Condition::all(),
    )
    .await?;
    Ok(())
}
