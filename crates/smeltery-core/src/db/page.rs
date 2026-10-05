//! Pagination: [`Page`] (one page of results and the total) and [`PageQuery`] (`?page=` and `?per_page=` from the
//! request, bounded).

use sea_orm::{
    EntityTrait, FromQueryResult, Iterable, PaginatorTrait, PrimaryKeyToColumn, QueryOrder, Select,
};
use serde::Serialize;

use super::Db;
use crate::app::App;
use crate::error::{Error, Result};

/// The highest page number [`PageQuery`] accepts; a larger `?page=` (up to 64 digits) is read as this one, a longer
/// value is ignored (page 1).
pub const MAX_PAGE: u64 = 10_000;

/// The page size [`PageQuery`] uses when the request names none.
pub const DEFAULT_PER_PAGE: u64 = 15;

/// The largest page size [`PageQuery`] accepts unless the handler raises it with [`PageQuery::max`].
pub const DEFAULT_MAX_PER_PAGE: u64 = 100;

/// One page of results: the items, where the page is, and how many there are in all.
///
/// It serializes with these field names (a JSON object for Alloy props, a value for Mold templates):
///
/// ```json
/// {"items": [...], "page": 2, "per_page": 15, "total": 47, "last_page": 4}
/// ```
///
/// `last_page` is at least 1, also when there are no items. A page past the last one has no items (its `page` is
/// the one asked for).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Page<T> {
    /// The items of this page, at most `per_page`.
    pub items: Vec<T>,
    /// This page's number, from 1.
    pub page: u64,
    /// The page size.
    pub per_page: u64,
    /// How many items there are on all pages together.
    pub total: u64,
    /// The number of the last page (at least 1).
    pub last_page: u64,
}

impl<T> Page<T> {
    /// A page of `items` (page `page` of `per_page` items each, `total` items in all). `last_page` is computed;
    /// `page` and `per_page` are taken as at least 1.
    pub fn new(items: Vec<T>, page: u64, per_page: u64, total: u64) -> Self {
        let per_page = per_page.max(1);
        Self {
            items,
            page: page.max(1),
            per_page,
            total,
            last_page: total.div_ceil(per_page).max(1),
        }
    }

    /// The same page with every item changed by `f`.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            page: self.page,
            per_page: self.per_page,
            total: self.total,
            last_page: self.last_page,
        }
    }

    /// Whether this page has no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether a page comes after this one.
    pub fn has_next(&self) -> bool {
        self.page < self.last_page
    }

    /// Whether a page comes before this one.
    pub fn has_previous(&self) -> bool {
        self.page > 1
    }

    /// The items of this page.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }
}

impl<T> IntoIterator for Page<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Page<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

/// Which page a request asks for: `?page=` (default 1) and `?per_page=` (default 15), as a handler argument.
///
/// It never rejects a request: a missing, empty or unreadable value means the default, `page` is kept within
/// `1..=10_000` and `per_page` within `1..=100` (raise the upper bound with [`PageQuery::max`]).
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
/// # }
/// # impl ActiveModelBehavior for ActiveModel {}
/// # }
/// # use post::Model as Post;
/// use smeltery_core::db::prelude::*;
/// use smeltery_core::db::{Page, PageQuery};
///
/// async fn index(db: Db, page: PageQuery) -> smeltery_core::Result<axum::Json<Page<Post>>> {
///     Ok(axum::Json(Post::paginate(&db, page).await?))
/// }
/// # fn main() {}
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageQuery {
    page: u64,
    per_page: Option<u64>,
    default_per_page: u64,
    max: u64,
}

impl Default for PageQuery {
    fn default() -> Self {
        Self {
            page: 1,
            per_page: None,
            default_per_page: DEFAULT_PER_PAGE,
            max: DEFAULT_MAX_PER_PAGE,
        }
    }
}

impl PageQuery {
    /// Page `page` with `per_page` items, bounded like a request's values (`page` within `1..=10_000`, `per_page`
    /// within `1..=100`).
    pub fn new(page: u64, per_page: u64) -> Self {
        Self {
            page: page.clamp(1, MAX_PAGE),
            per_page: Some(per_page),
            ..Self::default()
        }
    }

    /// Read `page` and `per_page` from a URL query string (`page=2&per_page=30`), bounded; anything else in it is
    /// ignored.
    pub fn from_query(query: &str) -> Self {
        let mut out = Self::default();
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "page" => {
                    if let Some(page) = number(&value) {
                        out.page = page.clamp(1, MAX_PAGE);
                    }
                }
                "per_page" => {
                    if let Some(per_page) = number(&value) {
                        out.per_page = Some(per_page);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Allow pages of up to `max` items (at least 1) instead of 100.
    #[must_use]
    pub fn max(mut self, max: u64) -> Self {
        self.max = max.max(1);
        self
    }

    /// Use `per_page` items when the request names no page size (default 15).
    #[must_use]
    pub fn default_per_page(mut self, per_page: u64) -> Self {
        self.default_per_page = per_page.max(1);
        self
    }

    /// The page number, from 1.
    pub fn page(&self) -> u64 {
        self.page
    }

    /// The page size, within `1..=max`.
    pub fn per_page(&self) -> u64 {
        self.per_page
            .unwrap_or(self.default_per_page)
            .clamp(1, self.max)
    }

    /// How many items come before this page (`(page - 1) × per_page`).
    pub fn offset(&self) -> u64 {
        (self.page - 1).saturating_mul(self.per_page())
    }
}

/// A run of at most 64 ASCII digits as a number (more than a `u64` holds reads as `u64::MAX`, then clamped by the
/// caller); anything else, a longer run included, as nothing.
fn number(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || value.len() > 64 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(value.parse::<u64>().unwrap_or(u64::MAX))
}

impl axum::extract::FromRequestParts<App> for PageQuery {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(Self::from_query(parts.uri.query().unwrap_or("")))
    }
}

/// One page of a SeaORM `select` (two statements: the page and the count). Give the select an order
/// (`.order_by_desc(post::Column::Id)`), so pages do not overlap.
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
/// #     pub published: bool,
/// # }
/// # impl ActiveModelBehavior for ActiveModel {}
/// # }
/// # use post::Model as Post;
/// use smeltery_core::db::prelude::*;
/// use smeltery_core::db::{Page, PageQuery, paginate};
///
/// async fn published(db: &Db, page: PageQuery) -> smeltery_core::Result<Page<Post>> {
///     let select = Post::query()
///         .filter(post::Column::Published.eq(true))
///         .order_by_desc(post::Column::Id);
///     paginate(db, select, page).await
/// }
/// # fn main() {}
/// ```
///
/// # Errors
/// A query fails.
pub async fn paginate<E>(db: &Db, select: Select<E>, page: PageQuery) -> Result<Page<E::Model>>
where
    E: EntityTrait,
    E::Model: FromQueryResult + Sized + Send + Sync,
{
    let per_page = page.per_page();
    let paginator = select.paginate(db.conn(), per_page);
    let total = paginator.num_items().await?;
    let items = if page.offset() >= total {
        Vec::new()
    } else {
        paginator.fetch_page(page.page() - 1).await?
    };
    Ok(Page::new(items, page.page(), per_page, total))
}

/// Every row of `E`, by primary key ascending: the select of [`Record::paginate`](super::Record::paginate).
pub(crate) fn by_key<E: EntityTrait>() -> Select<E> {
    let mut select = E::find();
    for key in E::PrimaryKey::iter() {
        select = select.order_by_asc(key.into_column());
    }
    select
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_and_per_page_are_clamped_and_never_rejected() {
        let q = PageQuery::from_query("");
        assert_eq!((q.page(), q.per_page(), q.offset()), (1, 15, 0));
        let q = PageQuery::from_query("page=3&per_page=20");
        assert_eq!((q.page(), q.per_page(), q.offset()), (3, 20, 40));
        let q = PageQuery::from_query("page=0&per_page=0");
        assert_eq!((q.page(), q.per_page()), (1, 1));
        let q = PageQuery::from_query("page=99999999&per_page=5000");
        assert_eq!((q.page(), q.per_page()), (MAX_PAGE, 100));
        let q = PageQuery::from_query("page=999999999999999999999999999&per_page=-4");
        assert_eq!((q.page(), q.per_page()), (MAX_PAGE, 15));
        for garbage in [
            "page=abc",
            "page=-1",
            "page=1.5",
            "page=%00",
            "page=",
            "page=%2B2",
            "page=0x10",
        ] {
            assert_eq!(PageQuery::from_query(garbage).page(), 1, "{garbage}");
        }
        assert_eq!(
            PageQuery::from_query("per_page=500").max(250).per_page(),
            250
        );
        assert_eq!(
            PageQuery::from_query("per_page=500").max(1000).per_page(),
            500
        );
        assert_eq!(
            PageQuery::from_query("").default_per_page(30).per_page(),
            30
        );
        assert_eq!(PageQuery::from_query("").max(0).per_page(), 1);
        assert_eq!(PageQuery::new(0, 0).page(), 1);
        assert_eq!(PageQuery::new(MAX_PAGE, 100).offset(), (MAX_PAGE - 1) * 100);
        // A long value is not parsed at all.
        let long = format!("page={}", "9".repeat(10_000));
        assert_eq!(PageQuery::from_query(&long).page(), 1);
    }

    #[test]
    fn pages_count_their_last_page_and_serialize_flat() {
        let page = Page::new(vec![1, 2], 2, 2, 5);
        assert_eq!(page.last_page, 3);
        assert!(page.has_next() && page.has_previous());
        let empty: Page<u8> = Page::new(Vec::new(), 1, 15, 0);
        assert_eq!(empty.last_page, 1);
        assert!(!empty.has_next() && !empty.has_previous() && empty.is_empty());
        let json = serde_json::to_value(page.map(|n| n * 10)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"items": [10, 20], "page": 2, "per_page": 2, "total": 5, "last_page": 3})
        );
    }
}
