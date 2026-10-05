//! MySQL / MariaDB: a `FULLTEXT` index over the text columns, searched in boolean mode with `+term` / `+term*`
//! built from the parsed terms and bound.

use sea_orm::sea_query::{Expr, SelectStatement};
use smeltery_core::Result;
use smeltery_core::db::Db;

use super::index_name;
use crate::spec::Resolved;

/// `MATCH(cols) AGAINST (? IN BOOLEAN MODE)`: `MATCH` names exactly the indexed columns, in index order.
fn matcher(spec: &Resolved, query: &str) -> Expr {
    let columns: Vec<String> = spec
        .texts
        .iter()
        .map(|(c, _)| format!("`{}`.`{c}`", spec.table))
        .collect();
    Expr::cust_with_values(
        format!("MATCH({}) AGAINST (? IN BOOLEAN MODE)", columns.join(", ")),
        [query.to_owned()],
    )
}

pub(super) fn text_condition(stmt: &mut SelectStatement, spec: &Resolved, query: &str) {
    stmt.and_where(matcher(spec, query));
}

/// The relevance of the same `MATCH` (higher is better).
pub(super) fn score(spec: &Resolved, query: &str) -> Expr {
    matcher(spec, query)
}

/// A `FULLTEXT` index `<table>_search` over the spec's text columns, in order.
pub(super) async fn check(db: &Db, spec: &Resolved) -> Result<Option<String>> {
    let index = index_name(spec.table);
    let rows = db
        .query_with(
            "SELECT CAST(column_name AS CHAR) AS name FROM information_schema.statistics \
             WHERE table_schema = DATABASE() AND table_name = ? AND index_name = ? AND index_type = 'FULLTEXT' \
             ORDER BY seq_in_index",
            [spec.table.into(), index.clone().into()],
        )
        .await?;
    let columns: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "name").ok())
        .collect();
    let want: Vec<&str> = spec.texts.iter().map(|(c, _)| c.as_str()).collect();
    if columns.is_empty() {
        return Ok(Some(format!(
            "there is no FULLTEXT index `{index}` on `{}`",
            spec.table
        )));
    }
    if columns != want {
        return Ok(Some(format!(
            "`{index}` covers {}, the spec has {}",
            columns.join(", "),
            want.join(", ")
        )));
    }
    Ok(None)
}
