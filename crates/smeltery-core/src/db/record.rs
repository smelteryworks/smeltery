//! [`Record`]: the everyday model calls, and [`Found`]: route model binding.

use std::future::Future;
use std::str::FromStr;

use sea_orm::sea_query::ArrayType;
use sea_orm::{
    ActiveModelBehavior, ActiveModelTrait, EntityTrait, FromQueryResult, IdenStatic,
    IntoActiveModel, Iterable, ModelTrait, PaginatorTrait, PrimaryKeyTrait, Select, Value,
};

use super::events::{ModelChange, notify};
use super::{Db, Page, PageQuery};
use crate::app::App;
use crate::error::{Error, Result};

/// The SeaORM `ActiveModel` of a model: what [`Record::create`] takes and
/// [`Record::update`]'s closure edits.
pub type ActiveModelOf<M> = <<M as Record>::Entity as EntityTrait>::ActiveModel;

/// The primary key type of a model (`i64` for `t.id()`).
pub type PrimaryKeyOf<M> =
    <<<M as Record>::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType;

/// The everyday model calls, implemented for every SeaORM entity `Model`.
///
/// A model file (`app/models/post.rs`) is a SeaORM entity; `app/models/mod.rs` names its
/// `Model` after the table (`pub use post::Model as Post;`), so the calls read:
///
/// ```no_run
/// # mod post {
/// # use smeltery_core::db::prelude::*;
/// # #[sea_orm::model]
/// # #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
/// # #[sea_orm(table_name = "posts")]
/// # pub struct Model {
/// #     #[sea_orm(primary_key)]
/// #     pub id: i64,
/// #     pub title: String,
/// #     pub draft: bool,
/// # }
/// # impl ActiveModelBehavior for ActiveModel {}
/// # }
/// # use post::Model as Post;
/// # use smeltery_core::db::prelude::*;
/// # async fn demo(db: Db) -> smeltery_core::Result<()> {
/// let posts = Post::all(&db).await?;
/// let post = Post::find_or_404(&db, 7).await?;
/// let post = Post::create(&db, post::ActiveModel { title: Set("Hi".into()), ..Default::default() }).await?;
/// let post = post.update(&db, |m| { m.title = Set("Hello".into()); }).await?;
/// post.delete(&db).await?;
/// let drafts = Post::query().filter(post::Column::Draft.eq(true)).all(db.conn()).await?;
/// # Ok(())
/// # }
/// # fn main() {}
/// ```
///
/// When the model has `created_at` / `updated_at` columns, [`create`](Record::create) sets
/// both (unless the `ActiveModel` sets them) and [`update`](Record::update) sets
/// `updated_at`, to the current UTC time. The columns may be `DateTimeUtc`,
/// `DateTimeWithTimeZone` or `DateTime` (naive, UTC), optional or not.
pub trait Record: Sized + Send + Sync + 'static {
    /// The SeaORM entity of this model.
    type Entity: EntityTrait<Model = Self>;

    /// Every row.
    ///
    /// # Errors
    /// The query fails.
    fn all(db: &Db) -> impl Future<Output = Result<Vec<Self>>> + Send;

    /// The row with this primary key, if any.
    ///
    /// # Errors
    /// The query fails.
    fn find(db: &Db, id: PrimaryKeyOf<Self>) -> impl Future<Output = Result<Option<Self>>> + Send;

    /// The row with this primary key, or a 404 [`Error`].
    ///
    /// # Errors
    /// No such row (404), or the query fails.
    fn find_or_404(db: &Db, id: PrimaryKeyOf<Self>) -> impl Future<Output = Result<Self>> + Send;

    /// A SeaORM `SELECT` over the table, to filter, order and run with SeaORM's API
    /// (`.filter(..)`, `.order_by_asc(..)`, `.all(db.conn())`).
    fn query() -> Select<Self::Entity>;

    /// The number of rows.
    ///
    /// # Errors
    /// The query fails.
    fn count(db: &Db) -> impl Future<Output = Result<u64>> + Send;

    /// Insert a row and return it, with `created_at` / `updated_at` filled in.
    ///
    /// # Errors
    /// The insert fails (e.g. a constraint).
    fn create(db: &Db, values: ActiveModelOf<Self>) -> impl Future<Output = Result<Self>> + Send;

    /// Change this row: `edit` sets fields on its `ActiveModel` (`m.title = Set(..)`); the
    /// changed fields and `updated_at` are saved, and the fresh row is returned.
    ///
    /// # Errors
    /// The update fails.
    fn update<F>(&self, db: &Db, edit: F) -> impl Future<Output = Result<Self>> + Send
    where
        F: FnOnce(&mut ActiveModelOf<Self>) + Send;

    /// Delete this row.
    ///
    /// # Errors
    /// The delete fails.
    fn delete(&self, db: &Db) -> impl Future<Output = Result<()>> + Send;

    /// One page of every row, by primary key ascending (two statements: the page and the count). For a filtered
    /// or differently ordered page, pass a select to [`db::paginate`](crate::db::paginate).
    ///
    /// # Errors
    /// A query fails.
    fn paginate(db: &Db, page: PageQuery) -> impl Future<Output = Result<Page<Self>>> + Send;
}

impl<M, E, A> Record for M
where
    M: ModelTrait<Entity = E> + FromQueryResult + IntoActiveModel<A> + Sync + 'static,
    E: EntityTrait<Model = M, ActiveModel = A>,
    A: ActiveModelTrait<Entity = E> + ActiveModelBehavior + Send + Sync + 'static,
{
    type Entity = E;

    async fn all(db: &Db) -> Result<Vec<Self>> {
        Ok(E::find().all(db.conn()).await?)
    }

    async fn find(db: &Db, id: PrimaryKeyOf<Self>) -> Result<Option<Self>> {
        Ok(E::find_by_id(id).one(db.conn()).await?)
    }

    async fn find_or_404(db: &Db, id: PrimaryKeyOf<Self>) -> Result<Self> {
        E::find_by_id(id)
            .one(db.conn())
            .await?
            .ok_or_else(Error::not_found)
    }

    fn query() -> Select<E> {
        E::find()
    }

    async fn count(db: &Db) -> Result<u64> {
        Ok(E::find().count(db.conn()).await?)
    }

    async fn create(db: &Db, mut values: A) -> Result<Self> {
        let now = chrono_now();
        stamp::<M, A>(&mut values, "created_at", now, true)?;
        stamp::<M, A>(&mut values, "updated_at", now, true)?;
        let row = values.insert(db.conn()).await?;
        if let Some(listeners) = db.listeners() {
            notify(db, listeners, ModelChange::Created, table::<E>(), &row).await;
        }
        Ok(row)
    }

    async fn update<F>(&self, db: &Db, edit: F) -> Result<Self>
    where
        F: FnOnce(&mut A) + Send,
    {
        let mut values: A = self.clone().into_active_model();
        edit(&mut values);
        stamp::<M, A>(&mut values, "updated_at", chrono_now(), false)?;
        if !values.is_changed() {
            return Ok(self.clone());
        }
        let row = values.update(db.conn()).await?;
        if let Some(listeners) = db.listeners() {
            notify(db, listeners, ModelChange::Updated, table::<E>(), &row).await;
        }
        Ok(row)
    }

    async fn delete(&self, db: &Db) -> Result<()> {
        let values: A = self.clone().into_active_model();
        let deleted = values.delete(db.conn()).await?;
        // SeaORM answers `Ok` with 0 rows when the row was already gone: nothing was deleted, nobody is told.
        if deleted.rows_affected == 0 {
            return Ok(());
        }
        if let Some(listeners) = db.listeners() {
            notify(db, listeners, ModelChange::Deleted, table::<E>(), self).await;
        }
        Ok(())
    }

    async fn paginate(db: &Db, page: PageQuery) -> Result<Page<Self>> {
        super::paginate(db, super::page::by_key::<E>(), page).await
    }
}

/// The table of the entity `E`.
fn table<E: EntityTrait>() -> &'static str {
    sea_orm::EntityName::table_name(&E::default())
}

fn chrono_now() -> sea_orm::prelude::DateTimeUtc {
    sea_orm::prelude::ChronoUtc::now()
}

/// Set the timestamp column `name` (when the model has it) to `now`, in the column's own
/// Rust type. With `only_if_unset`, a value the caller set is kept.
fn stamp<M, A>(
    values: &mut A,
    name: &str,
    now: sea_orm::prelude::DateTimeUtc,
    only_if_unset: bool,
) -> Result<()>
where
    M: ModelTrait,
    A: ActiveModelTrait<Entity = M::Entity>,
{
    let Some(column) = <<M::Entity as EntityTrait>::Column as Iterable>::iter()
        .find(|c| IdenStatic::as_str(c) == name)
    else {
        return Ok(());
    };
    if only_if_unset && !values.is_not_set(column) {
        return Ok(());
    }
    // Some other type (a string, a unix timestamp …): leave it to the app.
    let Some(value) = timestamp_value::<M::Entity>(column, now) else {
        return Ok(());
    };
    values.try_set(column, value)?;
    Ok(())
}

/// `now` in the Rust type of the model's timestamp `column` (`DateTimeUtc`,
/// `DateTimeWithTimeZone` or `DateTime`); `None` for any other type.
pub(crate) fn timestamp_value<E: EntityTrait>(
    column: E::Column,
    now: sea_orm::prelude::DateTimeUtc,
) -> Option<Value> {
    match <E::Model as ModelTrait>::get_value_type(column) {
        ArrayType::ChronoDateTimeUtc => Some(Value::from(now)),
        ArrayType::ChronoDateTimeWithTimeZone => Some(Value::from(now.fixed_offset())),
        ArrayType::ChronoDateTime => Some(Value::from(now.naive_utc())),
        _ => None,
    }
}

/// Route model binding: the model whose primary key is the route's last path parameter.
///
/// ```
/// # mod post {
/// # use smeltery_core::db::prelude::*;
/// # #[sea_orm::model]
/// # #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
/// # #[sea_orm(table_name = "posts")]
/// # pub struct Model {
/// #     #[sea_orm(primary_key)]
/// #     pub id: i64,
/// #     pub title: String,
/// #     pub draft: bool,
/// # }
/// # impl ActiveModelBehavior for ActiveModel {}
/// # }
/// # use post::Model as Post;
/// # use smeltery_core::Result;
/// # use smeltery_core::db::Found;
/// // r.get("/posts/{post}", show)
/// async fn show(Found(post): Found<Post>) -> Result<String> {
///     Ok(post.title)
/// }
/// # fn main() {}
/// ```
///
/// A row that does not exist, or a parameter that does not parse as the key, answers 404.
#[derive(Clone, Debug)]
pub struct Found<M>(pub M);

impl<M> std::ops::Deref for Found<M> {
    type Target = M;

    fn deref(&self) -> &M {
        &self.0
    }
}

impl<M> axum::extract::FromRequestParts<App> for Found<M>
where
    M: Record,
    PrimaryKeyOf<M>: FromStr,
{
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        let params = axum::extract::RawPathParams::from_request_parts(parts, app)
            .await
            .map_err(|_| Error::not_found())?;
        let raw = params
            .iter()
            .last()
            .map(|(_, value)| value.to_owned())
            .ok_or_else(|| Error::internal("Found<T> needs a route with a path parameter"))?;
        let id = raw
            .parse::<PrimaryKeyOf<M>>()
            .map_err(|_| Error::not_found())?;
        let db = app.db()?;
        M::find_or_404(&db, id).await.map(Found)
    }
}
