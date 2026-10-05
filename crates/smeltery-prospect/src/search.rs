//! [`Search`]: the search builder.

use std::any::TypeId;
use std::sync::Arc;

use sea_orm::Select;
use sea_orm::prelude::DateTimeUtc;
use smeltery_core::Result;
use smeltery_core::db::{Page, PageQuery, Record};

use crate::error::ProspectError;
use crate::hit::Hit;
use crate::settings::Driver;
use crate::spec::{Kind, Resolved};
use crate::text::SearchText;
use crate::{Prospect, Searchable};

/// The most values `where_in` / `where_not_in` take.
pub const MAX_IN_VALUES: usize = 100;

/// The most columns `highlight` takes.
pub const MAX_HIGHLIGHTS: usize = 4;

/// A sort direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Smallest first.
    Asc,
    /// Largest first.
    Desc,
}

/// A filter value: an integer, a bool, a string or a date-time, of the type of its column.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum FilterValue {
    /// For integer columns.
    Int(i64),
    /// For bool columns.
    Bool(bool),
    /// For string columns.
    Str(String),
    /// For date-time columns.
    DateTime(DateTimeUtc),
}

macro_rules! from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for FilterValue {
            fn from(value: $t) -> Self {
                Self::Int(i64::from(value))
            }
        }
    )*};
}
from_int!(i8, i16, i32, i64, u8, u16, u32);

impl From<bool> for FilterValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<&str> for FilterValue {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<String> for FilterValue {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<&String> for FilterValue {
    fn from(value: &String) -> Self {
        Self::Str(value.clone())
    }
}

impl From<DateTimeUtc> for FilterValue {
    fn from(value: DateTimeUtc) -> Self {
        Self::DateTime(value)
    }
}

impl FilterValue {
    fn fits(&self, kind: Kind) -> bool {
        matches!(
            (self, kind),
            (Self::Int(_), Kind::Integer)
                | (Self::Bool(_), Kind::Bool)
                | (Self::Str(_), Kind::String)
                | (Self::DateTime(_), Kind::DateTime)
        )
    }

    /// Whether an engine filter can carry this value: strings of `[A-Za-z0-9_.:@-]`, 1 to 128 characters (an engine's
    /// filter language has no documented escaping); every other kind is typed.
    pub(crate) fn engine_safe(&self) -> bool {
        match self {
            Self::Str(s) => {
                (1..=128).contains(&s.len())
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.:@-".contains(&b))
            }
            _ => true,
        }
    }
}

/// What a filter asks for.
#[derive(Clone, Debug)]
pub(crate) enum FilterOp {
    Eq(FilterValue),
    In(Vec<FilterValue>),
    NotIn(Vec<FilterValue>),
    Between(FilterValue, FilterValue),
}

impl FilterOp {
    pub(crate) fn values(&self) -> Vec<&FilterValue> {
        match self {
            Self::Eq(v) => vec![v],
            Self::In(vs) | Self::NotIn(vs) => vs.iter().collect(),
            Self::Between(a, b) => vec![a, b],
        }
    }
}

/// One condition on a declared filter column.
#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub(crate) column: String,
    pub(crate) op: FilterOp,
}

#[derive(Clone, Debug)]
enum Scope {
    Unset,
    Within(FilterValue),
    Across,
}

/// A refinement of the SQL select (database driver only).
pub(crate) type Refine<E> = Box<dyn FnOnce(Select<E>) -> Select<E> + Send + Sync>;

/// What a driver runs: everything the builder collected, checked.
pub(crate) struct Plan<E: sea_orm::EntityTrait> {
    pub(crate) text: SearchText,
    /// The filters, the scope's included.
    pub(crate) filters: Vec<Filter>,
    pub(crate) order: Option<(String, Direction)>,
    pub(crate) highlight: Vec<String>,
    pub(crate) refine: Option<Refine<E>>,
}

type EntityOf<M> = <M as Record>::Entity;

/// A search of the model `M`, from [`Searchable::search`]. Every method checks its input against the model's
/// [`IndexSpec`](crate::IndexSpec); a mistake (an undeclared column, a value of the wrong type, a bound passed) is
/// returned by the call that runs the search, before any query.
///
/// ```no_run
/// # mod post {
/// # use smeltery::db::prelude::*;
/// # #[sea_orm::model]
/// # #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
/// # #[sea_orm(table_name = "posts")]
/// # pub struct Model {
/// #     #[sea_orm(primary_key)]
/// #     pub id: i64,
/// #     pub title: String,
/// #     pub body: String,
/// #     pub user_id: i64,
/// #     pub team_id: i64,
/// # }
/// # impl ActiveModelBehavior for ActiveModel {}
/// # impl smeltery::prospect::Searchable for Model {
/// #     fn index(i: &mut smeltery::prospect::IndexSpec) {
/// #         i.text("title"); i.text("body"); i.filter("user_id"); i.scoped_by("team_id");
/// #     }
/// # }
/// # }
/// # use post::Model as Post;
/// use smeltery::db::{Page, PageQuery};
/// use smeltery::prospect::{Hit, Prospect, Searchable};
///
/// async fn search(prospect: &Prospect, q: &str, team: i64, page: PageQuery) -> smeltery::Result<Page<Hit<Post>>> {
///     Post::search(prospect, q)
///         .within(team)
///         .where_eq("user_id", 7)
///         .highlight(["title", "body"])
///         .paginate(page)
///         .await
/// }
/// # fn main() {}
/// ```
#[must_use = "a search does nothing until `paginate`, `get`, `keys` or `count` runs it"]
pub struct Search<M: Searchable> {
    prospect: Prospect,
    spec: Option<Arc<Resolved>>,
    text: SearchText,
    filters: Vec<Filter>,
    scope: Scope,
    order: Option<(String, Direction)>,
    highlight: Vec<String>,
    refine: Option<Refine<EntityOf<M>>>,
    error: Option<ProspectError>,
}

impl<M: Searchable> std::fmt::Debug for Search<M> {
    // Never the search text: it may be personal data.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Search")
            .field("table", &self.spec.as_ref().map(|s| s.table))
            .field("terms", &self.text.terms().len())
            .field("filters", &self.filters.len())
            .finish_non_exhaustive()
    }
}

impl<M: Searchable> Search<M> {
    pub(crate) fn new(prospect: &Prospect, text: &str) -> Self {
        let (spec, error) = match prospect.spec_of::<M>() {
            Ok(spec) => (Some(spec), None),
            Err(e) => (None, Some(e)),
        };
        Self {
            text: SearchText::parse(text, prospect.settings().query_length()),
            prospect: prospect.clone(),
            spec,
            filters: Vec::new(),
            scope: Scope::Unset,
            order: None,
            highlight: Vec::new(),
            refine: None,
            error,
        }
    }

    fn fail(&mut self, error: ProspectError) {
        self.error.get_or_insert(error);
    }

    /// Check `column` is a filter column and every value fits it.
    fn filter(mut self, column: &str, op: FilterOp) -> Self {
        let Some(spec) = self.spec.clone() else {
            return self;
        };
        let Some(kind) = spec.filter_kind(column) else {
            self.fail(ProspectError::Undeclared {
                table: spec.table,
                column: column.to_owned(),
                what: "filter",
            });
            return self;
        };
        if let FilterOp::In(vs) | FilterOp::NotIn(vs) = &op
            && vs.len() > MAX_IN_VALUES
        {
            self.fail(ProspectError::Limit(format!(
                "`where_in` / `where_not_in` take at most {MAX_IN_VALUES} values ({} given)",
                vs.len()
            )));
            return self;
        }
        if op.values().iter().any(|v| !v.fits(kind)) {
            self.fail(ProspectError::FilterType {
                table: spec.table,
                column: column.to_owned(),
                expected: kind.name(),
            });
            return self;
        }
        self.filters.push(Filter {
            column: column.to_owned(),
            op,
        });
        self
    }

    /// Only records of this scope (the model's `scoped_by` column equals `value`).
    pub fn within(mut self, value: impl Into<FilterValue>) -> Self {
        let Some(spec) = self.spec.clone() else {
            return self;
        };
        let Some(column) = spec.scope.clone() else {
            self.fail(ProspectError::Undeclared {
                table: spec.table,
                column: "(none)".to_owned(),
                what: "scope",
            });
            return self;
        };
        let value = value.into();
        let before = self.filters.len();
        self = self.filter(&column, FilterOp::Eq(value.clone()));
        if self.filters.len() > before {
            // The scope is kept apart from the filters: it is checked to be there.
            self.filters.pop();
            self.scope = Scope::Within(value);
        }
        self
    }

    /// Search every scope: an explicit choice, for a model with `scoped_by` (admin pages, console commands).
    pub fn across_scopes(mut self) -> Self {
        self.scope = Scope::Across;
        self
    }

    /// Only records whose filter column `column` equals `value`.
    pub fn where_eq(self, column: &str, value: impl Into<FilterValue>) -> Self {
        self.filter(column, FilterOp::Eq(value.into()))
    }

    /// Only records whose filter column `column` is one of `values` (at most 100; none matches nothing).
    pub fn where_in<V: Into<FilterValue>>(
        self,
        column: &str,
        values: impl IntoIterator<Item = V>,
    ) -> Self {
        let values = values.into_iter().map(Into::into).collect();
        self.filter(column, FilterOp::In(values))
    }

    /// Only records whose filter column `column` is none of `values` (at most 100).
    pub fn where_not_in<V: Into<FilterValue>>(
        self,
        column: &str,
        values: impl IntoIterator<Item = V>,
    ) -> Self {
        let values = values.into_iter().map(Into::into).collect();
        self.filter(column, FilterOp::NotIn(values))
    }

    /// Only records whose filter column `column` is between `low` and `high`, both included.
    pub fn where_between(
        self,
        column: &str,
        low: impl Into<FilterValue>,
        high: impl Into<FilterValue>,
    ) -> Self {
        self.filter(column, FilterOp::Between(low.into(), high.into()))
    }

    /// Order by the sort column `column` (ties by key, newest first) instead of by relevance.
    pub fn order_by(mut self, column: &str, direction: Direction) -> Self {
        let Some(spec) = self.spec.clone() else {
            return self;
        };
        if spec.sort_kind(column).is_none() {
            self.fail(ProspectError::Undeclared {
                table: spec.table,
                column: column.to_owned(),
                what: "sort",
            });
            return self;
        }
        self.order = Some((column.to_owned(), direction));
        self
    }

    /// Order by relevance, then key, newest first (the default; without search terms: key, newest first).
    pub fn order_by_relevance(mut self) -> Self {
        self.order = None;
        self
    }

    /// Highlight these text columns (at most 4) in each hit's `highlights`.
    pub fn highlight<I, S>(mut self, columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let Some(spec) = self.spec.clone() else {
            return self;
        };
        for column in columns {
            let column = column.as_ref();
            if !spec.is_text(column) {
                self.fail(ProspectError::Undeclared {
                    table: spec.table,
                    column: column.to_owned(),
                    what: "text",
                });
                return self;
            }
            if !self.highlight.iter().any(|c| c == column) {
                self.highlight.push(column.to_owned());
            }
        }
        if self.highlight.len() > MAX_HIGHLIGHTS {
            self.fail(ProspectError::Limit(format!(
                "`highlight` takes at most {MAX_HIGHLIGHTS} columns"
            )));
        }
        self
    }

    /// Refine the SQL select (a SeaORM `Select`) with conditions of your own: visibility rules that are not one
    /// column's value ("published or mine"). The database driver runs them in the same statement as the search, so
    /// pages and totals stay exact; another driver answers [`ProspectError::Unsupported`].
    pub fn query(
        mut self,
        refine: impl FnOnce(Select<EntityOf<M>>) -> Select<EntityOf<M>> + Send + Sync + 'static,
    ) -> Self {
        self.refine = Some(Box::new(refine));
        self
    }

    /// Checks done at run time: a builder error, the scope, the driver's own rules.
    fn plan(self) -> Result<(Prospect, Arc<Resolved>, Plan<EntityOf<M>>)> {
        if let Some(e) = self.error {
            return Err(e.into());
        }
        let spec = self.spec.ok_or(ProspectError::NotRegistered)?;
        let mut filters = self.filters;
        match (&spec.scope, self.scope) {
            (Some(column), Scope::Unset) => {
                return Err(ProspectError::ScopeMissing {
                    table: spec.table,
                    column: column.clone(),
                }
                .into());
            }
            (Some(column), Scope::Within(value)) => filters.push(Filter {
                column: column.clone(),
                op: FilterOp::Eq(value),
            }),
            _ => {}
        }
        if self.prospect.driver() != Driver::Database {
            if self.refine.is_some() {
                return Err(ProspectError::Unsupported(
                    "`query(…)` works only with the database driver (PROSPECT_DRIVER=database)"
                        .to_owned(),
                )
                .into());
            }
            for filter in &filters {
                if filter.op.values().iter().any(|v| !v.engine_safe()) {
                    return Err(ProspectError::FilterValue {
                        table: spec.table,
                        column: filter.column.clone(),
                    }
                    .into());
                }
            }
        }
        tracing::debug!(
            table = spec.table,
            terms = self.text.terms().len(),
            filters = filters.len(),
            "prospect: search"
        );
        Ok((
            self.prospect,
            spec,
            Plan {
                text: self.text,
                filters,
                order: self.order,
                highlight: self.highlight,
                refine: self.refine,
            },
        ))
    }

    async fn run(
        self,
        offset: u64,
        limit: u64,
        with_total: bool,
    ) -> Result<(Vec<Hit<M>>, Option<u64>)> {
        let (prospect, spec, plan) = self.plan()?;
        let db = prospect.db()?;
        match prospect.driver() {
            Driver::Database => {
                prospect.ensure_index(TypeId::of::<M>(), &spec, &db).await?;
                crate::driver::database::search::<M>(
                    &prospect, &db, &spec, plan, offset, limit, with_total,
                )
                .await
            }
            Driver::Memory => {
                let (hits, total) =
                    crate::driver::memory::search::<M>(&prospect, &db, &spec, &plan, offset, limit)
                        .await?;
                Ok((hits, Some(total)))
            }
        }
    }

    /// One page of hits and the total (two statements on the database driver). `per_page` is at most
    /// `PROSPECT_MAX_PER_PAGE`; a page past the last one has no hits.
    ///
    /// # Errors
    /// A mistake in the search (see [`Search`]), a missing search index, or a query that fails.
    pub async fn paginate(self, page: PageQuery) -> Result<Page<Hit<M>>> {
        let per_page = page
            .per_page()
            .min(self.prospect.settings().per_page_limit());
        let offset = (page.page() - 1).saturating_mul(per_page);
        let number = page.page();
        let (hits, total) = self.run(offset, per_page, true).await?;
        Ok(Page::new(hits, number, per_page, total.unwrap_or(0)))
    }

    /// The first `limit` hits (1 to `PROSPECT_MAX_PER_PAGE`), without a total (one statement).
    ///
    /// # Errors
    /// See [`paginate`](Self::paginate).
    pub async fn get(self, limit: u64) -> Result<Vec<Hit<M>>> {
        let limit = limit.clamp(1, self.prospect.settings().per_page_limit());
        Ok(self.run(0, limit, false).await?.0)
    }

    /// The primary keys of the first `limit` hits.
    ///
    /// # Errors
    /// See [`paginate`](Self::paginate).
    pub async fn keys(self, limit: u64) -> Result<Vec<i64>> {
        let spec = self.spec.clone();
        let hits = self.get(limit).await?;
        let Some(spec) = spec else {
            return Ok(Vec::new());
        };
        Ok(hits
            .iter()
            .map(|h| crate::spec::document(&h.model, &spec).0)
            .collect())
    }

    /// How many records match (one statement on the database driver).
    ///
    /// # Errors
    /// See [`paginate`](Self::paginate).
    pub async fn count(self) -> Result<u64> {
        let (prospect, spec, plan) = self.plan()?;
        let db = prospect.db()?;
        match prospect.driver() {
            Driver::Database => {
                prospect.ensure_index(TypeId::of::<M>(), &spec, &db).await?;
                crate::driver::database::count::<M>(&prospect, &db, &spec, plan).await
            }
            Driver::Memory => {
                let (_, total) =
                    crate::driver::memory::search::<M>(&prospect, &db, &spec, &plan, 0, 0).await?;
                Ok(total)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_filter_values_cannot_inject() {
        for good in ["abc", "a.b:c@d-e_f", "42", &"x".repeat(128)] {
            assert!(FilterValue::from(good).engine_safe(), "{good}");
        }
        for bad in [
            "",
            "a b",
            "a'b",
            "a\"b",
            "a`b",
            "a=b",
            "(x)",
            "a\nb",
            "ä",
            &"x".repeat(129),
            "1 OR 1=1",
        ] {
            assert!(!FilterValue::from(bad).engine_safe(), "{bad}");
        }
        assert!(FilterValue::from(7_i64).engine_safe());
        assert!(FilterValue::from(true).fits(Kind::Bool));
        assert!(!FilterValue::from("7").fits(Kind::Integer));
    }
}
