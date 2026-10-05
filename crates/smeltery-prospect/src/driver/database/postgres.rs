//! PostgreSQL: a stored generated `tsvector` column (`search_vector`) with a GIN index; the `tsquery` text is built
//! from the parsed terms and bound.

use sea_orm::sea_query::{Alias, Asterisk, Expr, Query, SelectStatement};
use smeltery_core::Result;
use smeltery_core::db::Db;

use super::{PG_COLUMN, highlight_alias};
use crate::highlight::{END, START};
use crate::spec::Resolved;
use crate::text::SearchText;

/// `ts_headline` reads at most this many characters of a column.
pub(crate) const HEADLINE_CHARS: u32 = 20_000;

fn vector(spec: &Resolved) -> String {
    format!("\"{}\".\"{PG_COLUMN}\"", spec.table)
}

/// `search_vector @@ to_tsquery(config, $1)`.
pub(super) fn text_condition(stmt: &mut SelectStatement, spec: &Resolved, text: &SearchText) {
    stmt.and_where(Expr::cust_with_values(
        format!(
            "{} @@ to_tsquery('{}', $1)",
            vector(spec),
            spec.language.pg_config()
        ),
        [text.tsquery()],
    ));
}

/// `ts_rank_cd` normalized to 0..1 (option 32), as `float8`.
pub(super) fn score(spec: &Resolved, text: &SearchText) -> Expr {
    Expr::cust_with_values(
        format!(
            "ts_rank_cd({}, to_tsquery('{}', $1), 32)::float8",
            vector(spec),
            spec.language.pg_config()
        ),
        [text.tsquery()],
    )
}

/// The `ts_headline` options for a long value: the markers, two fragments of 10 to 30 words.
fn fragments() -> String {
    format!("StartSel={START}, StopSel={END}, MaxFragments=2, MaxWords=30, MinWords=10")
}

/// The `ts_headline` options for a value of at most `SNIPPET_ABOVE` characters: the whole value, every match marked.
fn whole() -> String {
    format!("StartSel={START}, StopSel={END}, HighlightAll=true")
}

/// An outer select over the page (`_p`) that adds `ts_headline` for `columns`, so it runs only on the page's rows.
pub(super) fn with_headlines(
    page: SelectStatement,
    spec: &Resolved,
    text: &SearchText,
    columns: &[String],
) -> SelectStatement {
    let mut outer = Query::select();
    outer.column((Alias::new("_p"), Asterisk));
    for column in columns {
        outer.expr_as(
            Expr::cust_with_exprs(
                format!(
                    "CASE WHEN length($1) > {above} \
                     THEN ts_headline('{config}', left($1, {HEADLINE_CHARS}), to_tsquery('{config}', $2), $3) \
                     ELSE ts_headline('{config}', $1, to_tsquery('{config}', $2), $4) END",
                    above = super::sqlite::SNIPPET_ABOVE,
                    config = spec.language.pg_config()
                ),
                [
                    Expr::col((Alias::new("_p"), Alias::new(column.as_str()))),
                    Expr::val(text.tsquery()),
                    Expr::val(fragments()),
                    Expr::val(whole()),
                ],
            ),
            Alias::new(highlight_alias(column)),
        );
    }
    outer.from_subquery(page, Alias::new("_p"));
    outer
}

/// One weighted part of the generated column: `setweight(to_tsvector('<config>', coalesce(<column>, '')), '<w>')`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Part {
    pub(crate) config: String,
    pub(crate) column: String,
    pub(crate) weight: char,
}

/// The parts of a `search_vector` generation expression as PostgreSQL stores it (normalized, e.g.
/// `(setweight(to_tsvector('simple'::regconfig, (COALESCE(title, ''::character varying))::text), 'A'::"char") || …)`),
/// in order. A part it cannot read ends the list, so a foreign expression never passes the check.
pub(crate) fn parts(expression: &str) -> Vec<Part> {
    let lower = expression.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(found) = lower.get(at..).and_then(|rest| rest.find("to_tsvector(")) {
        let start = at + found + "to_tsvector(".len();
        // The configuration: the first quoted literal.
        let Some(config) = quoted(&lower, start) else {
            break;
        };
        // The column: the identifier after `coalesce(` (with any `(`, space or `"` before it).
        let Some(c) = lower.get(start..).and_then(|rest| rest.find("coalesce(")) else {
            break;
        };
        let mut i = start + c + "coalesce(".len();
        while bytes
            .get(i)
            .is_some_and(|b| *b == b'(' || *b == b'"' || *b == b' ')
        {
            i += 1;
        }
        let column: String = expression
            .get(i..)
            .unwrap_or("")
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        // The weight: the next `'X'` literal with X in A-D, before the next part.
        let next = lower
            .get(i..)
            .and_then(|rest| rest.find("to_tsvector("))
            .map_or(lower.len(), |n| i + n);
        let weight = expression
            .get(i..next)
            .unwrap_or("")
            .as_bytes()
            .windows(3)
            .find(|w| matches!(w, [b'\'', b'A'..=b'D', b'\'']))
            .and_then(|w| w.get(1).map(|b| char::from(*b)));
        let Some(weight) = weight else {
            break;
        };
        out.push(Part {
            config,
            column,
            weight,
        });
        at = next;
    }
    out
}

/// The first `'…'` literal at or after `from`.
fn quoted(text: &str, from: usize) -> Option<String> {
    let rest = text.get(from..)?;
    let open = rest.find('\'')?;
    let body = rest.get(open + 1..)?;
    let close = body.find('\'')?;
    body.get(..close).map(str::to_owned)
}

/// What is wrong with a `search_vector` expression for `spec` (`None`: it fits).
pub(crate) fn compare(spec: &Resolved, expression: &str) -> Option<String> {
    let found = parts(expression);
    let want: Vec<Part> = spec
        .texts
        .iter()
        .map(|(c, w)| Part {
            config: spec.language.pg_config().to_owned(),
            column: c.clone(),
            weight: w.letter(),
        })
        .collect();
    (found != want).then(|| {
        let show = |ps: &[Part]| {
            ps.iter()
                .map(|p| format!("{} ({}, {})", p.column, p.weight, p.config))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "`{PG_COLUMN}` indexes {}, the spec has {}",
            if found.is_empty() {
                "something else".to_owned()
            } else {
                show(&found)
            },
            show(&want)
        )
    })
}

/// The table has the `search_vector` column generated from the spec's columns, weights and configuration, and its
/// GIN index.
pub(super) async fn check(db: &Db, spec: &Resolved) -> Result<Option<String>> {
    let rows = db
        .query_with(
            "SELECT COALESCE(generation_expression::text, '') AS expr FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = $1 AND column_name = $2",
            [spec.table.into(), PG_COLUMN.into()],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(Some(format!(
            "`{}` has no `{PG_COLUMN}` column",
            spec.table
        )));
    };
    let expression: String = row.try_get("", "expr")?;
    if let Some(problem) = compare(spec, &expression) {
        return Ok(Some(problem));
    }
    let index = super::pg_index_name(spec.table);
    let rows = db
        .query_with(
            "SELECT indexdef::text AS def FROM pg_indexes WHERE schemaname = current_schema() \
             AND tablename = $1 AND indexname = $2",
            [spec.table.into(), index.clone().into()],
        )
        .await?;
    let gin = rows
        .first()
        .and_then(|r| r.try_get::<String>("", "def").ok())
        .is_some_and(|def| {
            let def = def.to_ascii_lowercase();
            def.contains("using gin") && def.contains(PG_COLUMN)
        });
    Ok((!gin).then(|| format!("there is no GIN index `{index}` on `{PG_COLUMN}`")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// What PostgreSQL 16 stores for the column `SearchIndex` writes (`information_schema.columns
    /// .generation_expression`): casts added, identifiers unquoted.
    const STORED: &str = "(setweight(to_tsvector('simple'::regconfig, (COALESCE(title, ''::character varying))::text), \
                          'A'::\"char\") || setweight(to_tsvector('simple'::regconfig, COALESCE(body, ''::text)), 'B'::\"char\"))";

    fn part(config: &str, column: &str, weight: char) -> Part {
        Part {
            config: config.into(),
            column: column.into(),
            weight,
        }
    }

    #[test]
    fn the_stored_expression_is_read_in_order() {
        assert_eq!(
            parts(STORED),
            [part("simple", "title", 'A'), part("simple", "body", 'B')]
        );
        // The DDL `SearchIndex` writes reads the same.
        let ddl = crate::migration::SearchIndex::on("posts")
            .text("title", crate::Weight::A)
            .text("body", crate::Weight::B)
            .create_sql(smeltery_core::db::Backend::Postgres)
            .unwrap();
        assert_eq!(parts(&ddl[0]), parts(STORED));
        assert!(parts("to_tsvector(title)").is_empty());
        assert!(parts("").is_empty());
    }

    mod post {
        use smeltery_core::db::prelude::*;

        #[sea_orm::model]
        #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
        #[sea_orm(table_name = "posts")]
        pub struct Model {
            #[sea_orm(primary_key)]
            pub id: i64,
            pub title: String,
            pub body: String,
        }

        impl ActiveModelBehavior for ActiveModel {}
    }

    impl crate::Searchable for post::Model {
        fn index(i: &mut crate::IndexSpec) {
            i.text("title").weight(crate::Weight::A);
            i.text("body");
        }
    }

    #[test]
    fn a_different_configuration_weight_or_column_list_is_refused() {
        let spec = crate::spec::resolve::<post::Model>().unwrap();
        assert_eq!(compare(&spec, STORED), None);
        let english = STORED.replace("'simple'", "'english'");
        assert!(compare(&spec, &english).unwrap().contains("english"));
        let weights = STORED.replace("'B'::", "'A'::");
        assert!(compare(&spec, &weights).is_some());
        let swapped = STORED
            .replace("COALESCE(title", "COALESCE(x")
            .replace("COALESCE(body", "COALESCE(title");
        assert!(compare(&spec, &swapped).is_some());
        assert!(compare(&spec, "").unwrap().contains("something else"));
    }
}
