//! The `database` driver: each backend's own full-text search, kept current by the database itself (FTS5 triggers,
//! a generated `tsvector` column, `FULLTEXT` indexes). One statement for the page and one for the total; the search
//! text is always a bound value, identifiers come from the checked spec.

mod mysql;
mod postgres;
mod sqlite;

use sea_orm::sea_query::{Alias, Expr, ExprTrait, Order, Query, SelectStatement};
use sea_orm::{
    ConnectionTrait, DbBackend, EntityTrait, FromQueryResult, ModelTrait, QueryTrait, Value,
};
use smeltery_core::db::{Backend, Db, Record};
use smeltery_core::{Error, Result};

use crate::highlight::{Highlight, Highlights};
use crate::hit::Hit;
use crate::search::{Direction, Filter, FilterOp, FilterValue, Plan};
use crate::spec::{DocValue, Resolved, column};
use crate::{Prospect, Searchable};

/// The score column of a search statement.
pub(crate) const SCORE: &str = "_prospect_score";

/// The PostgreSQL column that holds the `tsvector`.
pub(crate) const PG_COLUMN: &str = "search_vector";

/// The FTS5 table (SQLite) or the `FULLTEXT` index (MySQL) of a table.
pub(crate) fn index_name(table: &str) -> String {
    format!("{table}_search")
}

/// The GIN index of the `tsvector` column (PostgreSQL).
pub(crate) fn pg_index_name(table: &str) -> String {
    format!("{table}_search_index")
}

/// The column a highlight of `column` is read from.
pub(crate) fn highlight_alias(column: &str) -> String {
    format!("_prospect_h_{column}")
}

/// SeaORM's backend for a [`Backend`].
pub(crate) fn sea_backend(backend: Backend) -> Result<DbBackend> {
    match backend {
        Backend::Sqlite => Ok(DbBackend::Sqlite),
        Backend::Postgres => Ok(DbBackend::Postgres),
        Backend::MySql => Ok(DbBackend::MySql),
        _ => Err(Error::internal(
            "prospect: this database backend has no full-text search driver",
        )),
    }
}

/// An identifier quoted for `backend` (names are checked `[A-Za-z_][A-Za-z0-9_]*` before they get here).
pub(crate) fn quote(backend: Backend, name: &str) -> String {
    match backend {
        Backend::MySql => format!("`{name}`"),
        _ => format!("\"{name}\""),
    }
}

type EntityOf<M> = <M as Record>::Entity;

/// A filter value as a SQL value of `column`'s own Rust type (date-times in the column's flavour).
fn sql_value<M: Searchable>(column_name: &str, value: &FilterValue) -> Value {
    match value {
        FilterValue::Int(n) => Value::BigInt(Some(*n)),
        FilterValue::Bool(b) => Value::Bool(Some(*b)),
        FilterValue::Str(s) => Value::String(Some(s.clone())),
        FilterValue::DateTime(t) => {
            use sea_orm::sea_query::ArrayType;
            let kind =
                column::<EntityOf<M>>(column_name).map(|c| <M as ModelTrait>::get_value_type(c));
            match kind {
                Some(ArrayType::ChronoDateTime) => Value::ChronoDateTime(Some(t.naive_utc())),
                Some(ArrayType::ChronoDateTimeWithTimeZone) => {
                    Value::ChronoDateTimeWithTimeZone(Some(t.fixed_offset()))
                }
                _ => Value::ChronoDateTimeUtc(Some(*t)),
            }
        }
    }
}

/// The SQL condition of one filter.
pub(crate) fn condition<M: Searchable>(table: &str, filter: &Filter) -> Expr {
    let col = Expr::col((Alias::new(table), Alias::new(filter.column.as_str())));
    let value = |v: &FilterValue| sql_value::<M>(&filter.column, v);
    match &filter.op {
        FilterOp::Eq(v) => col.eq(value(v)),
        FilterOp::In(vs) => col.is_in(vs.iter().map(value)),
        FilterOp::NotIn(vs) => col.is_not_in(vs.iter().map(value)),
        FilterOp::Between(a, b) => col.between(value(a), value(b)),
    }
}

/// The statements of one search.
#[derive(Debug)]
pub(crate) struct Built {
    pub(crate) count: SelectStatement,
    pub(crate) page: SelectStatement,
    /// Whether the page has a score column (there are terms).
    pub(crate) scored: bool,
    /// The score is lower-is-better (FTS5 `bm25`) and is negated.
    pub(crate) negate: bool,
    /// Columns whose highlights come marked from the database.
    pub(crate) marked: Vec<String>,
    /// Columns highlighted in Rust (MySQL).
    pub(crate) in_rust: Vec<String>,
}

/// Build the statements of a search.
pub(crate) fn build<M: Searchable>(
    backend: Backend,
    spec: &Resolved,
    plan: Plan<EntityOf<M>>,
    mysql_min_token: usize,
    offset: u64,
    limit: u64,
) -> Built {
    let table = spec.table;
    let mut select = EntityOf::<M>::find();
    if let Some(refine) = plan.refine {
        select = refine(select);
    }
    let mut stmt = select.into_query();
    if let Some(flag) = &spec.only_when {
        stmt.and_where(Expr::col((Alias::new(table), Alias::new(flag.as_str()))).eq(true));
    }
    for filter in &plan.filters {
        stmt.and_where(condition::<M>(table, filter));
    }
    // MySQL drops the terms InnoDB does not index; none left is a search without a text condition.
    let mysql_query = if backend == Backend::MySql {
        plan.text.mysql_boolean(mysql_min_token)
    } else {
        None
    };
    let terms = !plan.text.is_empty() && (backend != Backend::MySql || mysql_query.is_some());
    let mut built = Built {
        count: SelectStatement::new(),
        page: SelectStatement::new(),
        scored: terms,
        negate: false,
        marked: Vec::new(),
        in_rust: Vec::new(),
    };
    // The text condition; then the score and the highlights of the page.
    let mut page_exprs: Vec<(Expr, String)> = Vec::new();
    if terms {
        match backend {
            Backend::Sqlite => {
                sqlite::text_condition(&mut stmt, spec, &plan.text);
                page_exprs.push((sqlite::score(spec), SCORE.to_owned()));
                for column in &plan.highlight {
                    if let Some(expr) = sqlite::highlight(spec, column) {
                        page_exprs.push((expr, highlight_alias(column)));
                        built.marked.push(column.clone());
                    }
                }
                built.negate = true;
            }
            Backend::Postgres => {
                postgres::text_condition(&mut stmt, spec, &plan.text);
                page_exprs.push((postgres::score(spec, &plan.text), SCORE.to_owned()));
                built.marked.clone_from(&plan.highlight);
            }
            _ => {
                let query = mysql_query.unwrap_or_default();
                mysql::text_condition(&mut stmt, spec, &query);
                page_exprs.push((mysql::score(spec, &query), SCORE.to_owned()));
                built.in_rust.clone_from(&plan.highlight);
            }
        }
    }
    built.count = Query::select()
        .expr_as(Expr::cust("COUNT(*)"), Alias::new("n"))
        .from_subquery(stmt.clone(), Alias::new("_prospect_count"))
        .to_owned();

    let mut page = stmt;
    for (expr, alias) in page_exprs {
        page.expr_as(expr, Alias::new(alias));
    }
    order(&mut page, backend, spec, plan.order.as_ref(), terms, None);
    page.limit(limit).offset(offset);
    if backend == Backend::Postgres && terms && !built.marked.is_empty() {
        // `ts_headline` re-reads each document, so it runs only over the page's rows: an outer select over the page.
        page = postgres::with_headlines(page, spec, &plan.text, &built.marked);
        order(
            &mut page,
            backend,
            spec,
            plan.order.as_ref(),
            terms,
            Some("_p"),
        );
    }
    built.page = page;
    built
}

/// ORDER BY: the sort column, or relevance when there are terms, then the key, newest first. `outer` names the
/// subquery alias the columns are read through.
fn order(
    stmt: &mut SelectStatement,
    backend: Backend,
    spec: &Resolved,
    by: Option<&(String, Direction)>,
    terms: bool,
    outer: Option<&str>,
) {
    let table = outer.unwrap_or(spec.table);
    let col = |name: &str| Expr::col((Alias::new(table), Alias::new(name)));
    match by {
        Some((name, direction)) => {
            let order = match direction {
                Direction::Asc => Order::Asc,
                Direction::Desc => Order::Desc,
            };
            stmt.order_by_expr(col(name), order);
        }
        None if terms => {
            // FTS5's bm25 is lower-is-better; ts_rank_cd and MATCH … AGAINST are higher-is-better.
            let order = if backend == Backend::Sqlite {
                Order::Asc
            } else {
                Order::Desc
            };
            let score = match outer {
                Some(alias) => Expr::col((Alias::new(alias), Alias::new(SCORE))),
                None => Expr::col(Alias::new(SCORE)),
            };
            stmt.order_by_expr(score, order);
        }
        None => {}
    }
    stmt.order_by_expr(col(&spec.key), Order::Desc);
}

/// Run a search: the hits of the page and, with `with_total`, the total.
pub(crate) async fn search<M: Searchable>(
    prospect: &Prospect,
    db: &Db,
    spec: &Resolved,
    plan: Plan<EntityOf<M>>,
    offset: u64,
    limit: u64,
    with_total: bool,
) -> Result<(Vec<Hit<M>>, Option<u64>)> {
    let backend = db.backend();
    let min_token = mysql_min_token(prospect, db).await;
    let text = plan.text.clone();
    let built = build::<M>(backend, spec, plan, min_token, offset, limit);
    let sea = sea_backend(backend)?;
    let total = if with_total {
        Some(count_of(db, sea, &built.count).await?)
    } else {
        None
    };
    if limit == 0 || total.is_some_and(|t| offset >= t) {
        return Ok((Vec::new(), total));
    }
    let rows = db.conn().query_all_raw(sea.build(&built.page)).await?;
    let mut hits = Vec::with_capacity(rows.len());
    for row in rows {
        let model = <M as FromQueryResult>::from_query_result(&row, "")?;
        let score = if built.scored {
            row.try_get::<f64>("", SCORE)
                .ok()
                .map(|s| if built.negate { -s } else { s })
        } else {
            None
        };
        let mut highlights = Highlights::default();
        for column in &built.marked {
            // The database copies the value's own characters into its output: a value that holds U+E000 / U+E001
            // could pass a stored pair off as a match. Such a value is highlighted in Rust, which strips them.
            if let Some(value) = stored_markers::<M>(&model, column) {
                highlights.insert(column, Highlight::of_text(&value, &text));
                continue;
            }
            if let Ok(Some(marked)) = row.try_get::<Option<String>>("", &highlight_alias(column)) {
                highlights.insert(column, Highlight::from_marked(&marked));
            }
        }
        for column in &built.in_rust {
            if let Some(c) = column_of::<M>(column)
                && let DocValue::Str(value) = DocValue::of(ModelTrait::get(&model, c))
            {
                highlights.insert(column, Highlight::of_text(&value, &text));
            }
        }
        hits.push(Hit::new(model, score, highlights));
    }
    Ok((hits, total))
}

fn column_of<M: Searchable>(name: &str) -> Option<<EntityOf<M> as EntityTrait>::Column> {
    column::<EntityOf<M>>(name)
}

/// The value of `model`'s text column `column` when it holds a highlight marker character (U+E000 / U+E001).
fn stored_markers<M: Searchable>(model: &M, column: &str) -> Option<String> {
    let c = column_of::<M>(column)?;
    match DocValue::of(ModelTrait::get(model, c)) {
        DocValue::Str(value)
            if value.contains([crate::highlight::START, crate::highlight::END]) =>
        {
            Some(value)
        }
        _ => None,
    }
}

/// Count the matches of a search (one statement).
pub(crate) async fn count<M: Searchable>(
    prospect: &Prospect,
    db: &Db,
    spec: &Resolved,
    plan: Plan<EntityOf<M>>,
) -> Result<u64> {
    let backend = db.backend();
    let min_token = mysql_min_token(prospect, db).await;
    let built = build::<M>(backend, spec, plan, min_token, 0, 0);
    count_of(db, sea_backend(backend)?, &built.count).await
}

async fn count_of(db: &Db, sea: DbBackend, stmt: &SelectStatement) -> Result<u64> {
    let row = db
        .conn()
        .query_one_raw(sea.build(stmt))
        .await?
        .ok_or_else(|| Error::internal("prospect: the count returned no row"))?;
    let n = row.try_get::<i64>("", "n")?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// MySQL's `innodb_ft_min_token_size` (read once per process; 3, the server default, when it cannot be read).
async fn mysql_min_token(prospect: &Prospect, db: &Db) -> usize {
    if db.backend() != Backend::MySql {
        return 0;
    }
    *prospect
        .inner
        .mysql_min_token
        .get_or_init(|| async {
            let read = db
                .query_with("SELECT CAST(@@innodb_ft_min_token_size AS SIGNED) AS n", [])
                .await
                .ok()
                .and_then(|rows| rows.first().and_then(|r| r.try_get::<i64>("", "n").ok()));
            read.and_then(|n| usize::try_from(n).ok()).unwrap_or(3)
        })
        .await
}

/// `None` when the database has `spec`'s index as the spec wants it, else what is wrong.
pub(crate) async fn check_index(
    prospect: &Prospect,
    db: &Db,
    spec: &Resolved,
) -> Result<Option<String>> {
    match db.backend() {
        Backend::Sqlite => sqlite::check(db, spec).await,
        Backend::Postgres => postgres::check(db, spec).await,
        _ => {
            // The token size is read here, once per process (searches reuse it).
            mysql_min_token(prospect, db).await;
            mysql::check(db, spec).await
        }
    }
}

/// The rows of the model's table.
pub(crate) async fn row_count(db: &Db, spec: &Resolved) -> Result<u64> {
    let stmt = Query::select()
        .expr_as(Expr::cust("COUNT(*)"), Alias::new("n"))
        .from(Alias::new(spec.table))
        .to_owned();
    count_of(db, sea_backend(db.backend())?, &stmt).await
}

/// Rebuild the index from the table (SQLite's FTS5 `'rebuild'`; PostgreSQL and MySQL keep theirs current: 0).
pub(crate) async fn rebuild(db: &Db, spec: &Resolved) -> Result<u64> {
    if db.backend() != Backend::Sqlite {
        return Ok(0);
    }
    let index = quote(Backend::Sqlite, &index_name(spec.table));
    db.execute(&format!("INSERT INTO {index}({index}) VALUES('rebuild')"))
        .await?;
    row_count(db, spec).await
}

#[cfg(test)]
mod tests;
