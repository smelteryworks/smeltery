//! SQLite: an FTS5 external-content table `<table>_search` joined on its rowid (the integer key).

use sea_orm::sea_query::{Alias, Expr, ExprTrait, JoinType, SelectStatement};
use smeltery_core::Result;
use smeltery_core::db::Db;

use super::index_name;
use crate::spec::Resolved;
use crate::text::SearchText;

/// Columns longer than this (characters) get an FTS5 `snippet` (32 tokens) instead of the whole text.
pub(crate) const SNIPPET_ABOVE: usize = 2_000;

/// Join the FTS5 table and require the terms (the FTS5 string is a bound value).
pub(super) fn text_condition(stmt: &mut SelectStatement, spec: &Resolved, text: &SearchText) {
    let index = index_name(spec.table);
    stmt.join(
        JoinType::InnerJoin,
        Alias::new(index.as_str()),
        Expr::col((Alias::new(index.as_str()), Alias::new("rowid")))
            .equals((Alias::new(spec.table), Alias::new(spec.key.as_str()))),
    );
    stmt.and_where(Expr::cust_with_values(
        format!("\"{index}\" MATCH ?"),
        [text.fts5()],
    ));
}

/// `bm25` with the column weights (lower is better).
pub(super) fn score(spec: &Resolved) -> Expr {
    let weights: Vec<String> = spec
        .texts
        .iter()
        .map(|(_, w)| format!("{:.1}", w.bm25()))
        .collect();
    Expr::cust(format!(
        "bm25(\"{}\", {})",
        index_name(spec.table),
        weights.join(", ")
    ))
}

/// FTS5 `highlight` (or `snippet` for long values) of `column`, with U+E000 / U+E001 as markers.
pub(super) fn highlight(spec: &Resolved, column: &str) -> Option<Expr> {
    let at = spec.texts.iter().position(|(c, _)| c == column)?;
    let index = index_name(spec.table);
    Some(Expr::cust(format!(
        "CASE WHEN length(\"{table}\".\"{column}\") > {SNIPPET_ABOVE} \
         THEN snippet(\"{index}\", {at}, char(57344), char(57345), '…', 32) \
         ELSE highlight(\"{index}\", {at}, char(57344), char(57345)) END",
        table = spec.table
    )))
}

/// The FTS5 table exists with the spec's text columns in order, and its three triggers exist.
pub(super) async fn check(db: &Db, spec: &Resolved) -> Result<Option<String>> {
    let index = index_name(spec.table);
    let rows = db
        .query_with(
            "SELECT name AS name FROM pragma_table_info(?)",
            [index.clone().into()],
        )
        .await?;
    let columns: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "name").ok())
        .collect();
    let want: Vec<&str> = spec.texts.iter().map(|(c, _)| c.as_str()).collect();
    if columns.is_empty() {
        return Ok(Some(format!("there is no table `{index}`")));
    }
    if columns != want {
        return Ok(Some(format!(
            "`{index}` has the columns {}, the spec has {}",
            columns.join(", "),
            want.join(", ")
        )));
    }
    // The FTS5 rowid must be the model's key over the model's table, or a join returns other rows.
    let created = db
        .query_with(
            "SELECT sql AS sql FROM sqlite_master WHERE type = 'table' AND name = ?",
            [index.clone().into()],
        )
        .await?;
    let sql: String = created
        .first()
        .and_then(|r| r.try_get::<String>("", "sql").ok())
        .unwrap_or_default();
    let compact: String = sql.chars().filter(|c| !c.is_whitespace()).collect();
    let (table, key) = (spec.table, spec.key.as_str());
    if !compact.contains(&format!("content='{table}'"))
        || !compact.contains(&format!("content_rowid='{key}'"))
    {
        return Ok(Some(format!(
            "`{index}` is not an index over `{table}` keyed by `{key}` (content='{table}', content_rowid='{key}')"
        )));
    }
    let triggers = db
        .query_with(
            "SELECT name AS name FROM sqlite_master WHERE type = 'trigger' AND name IN (?, ?, ?)",
            [
                format!("{index}_ai").into(),
                format!("{index}_ad").into(),
                format!("{index}_au").into(),
            ],
        )
        .await?;
    if triggers.len() != 3 {
        return Ok(Some(format!(
            "the triggers `{index}_ai`, `{index}_ad` and `{index}_au` that keep it current are not all there"
        )));
    }
    Ok(None)
}
