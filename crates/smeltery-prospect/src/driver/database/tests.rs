//! The statements of the database driver, per backend (no server needed): the search text is always a bound value,
//! identifiers come from the spec.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use sea_orm::sea_query::{MysqlQueryBuilder, PostgresQueryBuilder, SqliteQueryBuilder};
use smeltery_core::db::Backend;

use super::build;
use crate::search::{Direction, Filter, FilterOp, FilterValue, Plan};
use crate::spec::resolve;
use crate::text::SearchText;
use crate::{IndexSpec, Searchable, Weight};

mod post {
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "posts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub title: String,
        pub body: Option<String>,
        pub team_id: i64,
        pub published: bool,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

impl Searchable for post::Model {
    fn index(i: &mut IndexSpec) {
        i.text("title").weight(Weight::A);
        i.text("body");
        i.scoped_by("team_id");
        i.only_when("published");
        i.sort("id");
    }
}

const HOSTILE: &str = "title:x NEAR( \"a\" ') OR 1=1 -- * ^c";

fn plan(text: &str, highlight: bool) -> Plan<post::Entity> {
    Plan {
        text: SearchText::parse(text, 200),
        filters: vec![Filter {
            column: "team_id".to_owned(),
            op: FilterOp::Eq(FilterValue::Int(7)),
        }],
        order: None,
        highlight: if highlight {
            vec!["title".to_owned()]
        } else {
            Vec::new()
        },
        refine: None,
    }
}

#[test]
fn sqlite_statements_bind_the_fts5_string() {
    let spec = resolve::<post::Model>().unwrap();
    let built = build::<post::Model>(Backend::Sqlite, &spec, plan(HOSTILE, true), 0, 30, 15);
    let (sql, values) = built.page.build(SqliteQueryBuilder);
    assert!(
        sql.contains(
            "INNER JOIN \"posts_search\" ON \"posts_search\".\"rowid\" = \"posts\".\"id\""
        ),
        "{sql}"
    );
    assert!(sql.contains("\"posts_search\" MATCH ?"), "{sql}");
    assert!(
        sql.contains("bm25(\"posts_search\", 10.0, 5.0) AS \"_prospect_score\""),
        "{sql}"
    );
    assert!(
        sql.contains("highlight(\"posts_search\", 0, char(57344), char(57345))"),
        "{sql}"
    );
    assert!(sql.contains("\"posts\".\"published\" = ?"), "{sql}");
    assert!(
        sql.contains("ORDER BY \"_prospect_score\" ASC, \"posts\".\"id\" DESC LIMIT ? OFFSET ?"),
        "{sql}"
    );
    // Nothing the user typed is SQL text.
    for piece in ["NEAR", "1=1", "--", "title:x", "OR 1"] {
        assert!(!sql.contains(piece), "{piece} in {sql}");
    }
    let bound = format!("{values:?}");
    assert!(
        bound.contains(r#"\"title\" \"x\" \"near\" \"a\" \"or\" \"1\" \"1\" \"c\""#),
        "{bound}"
    );
    let (count, _) = built.count.build(SqliteQueryBuilder);
    assert!(
        count.starts_with("SELECT COUNT(*) AS \"n\" FROM (SELECT"),
        "{count}"
    );
    assert!(!count.contains("LIMIT"), "{count}");
    assert!(built.negate && built.scored);
}

#[test]
fn postgres_statements_bind_the_tsquery_and_headline_only_the_page() {
    let spec = resolve::<post::Model>().unwrap();
    let built = build::<post::Model>(Backend::Postgres, &spec, plan("rust forg", true), 0, 0, 15);
    let (sql, values) = built.page.build(PostgresQueryBuilder);
    assert!(
        sql.contains("\"posts\".\"search_vector\" @@ to_tsquery('simple', $"),
        "{sql}"
    );
    assert!(
        sql.contains("ts_rank_cd(\"posts\".\"search_vector\", to_tsquery('simple', $"),
        "{sql}"
    );
    // The headline runs in an outer select over the limited page.
    // Up to 2,000 characters the whole value with every match (HighlightAll), above that fragments of the first
    // 20,000 characters.
    assert!(
        sql.starts_with(
            "SELECT \"_p\".*, CASE WHEN length(\"_p\".\"title\") > 2000 THEN ts_headline('simple', left(\"_p\".\"title\", 20000), to_tsquery('simple', $"
        ),
        "{sql}"
    );
    assert!(
        sql.contains("ELSE ts_headline('simple', \"_p\".\"title\", to_tsquery('simple', $"),
        "{sql}"
    );
    let bound = format!("{values:?}");
    assert!(
        bound.contains("HighlightAll=true") && bound.contains("MaxFragments=2"),
        "{bound}"
    );
    assert!(
        sql.contains(") AS \"_p\" ORDER BY \"_p\".\"_prospect_score\" DESC, \"_p\".\"id\" DESC"),
        "{sql}"
    );
    let inner_limit = sql.find("LIMIT").unwrap();
    assert!(inner_limit < sql.find(") AS \"_p\"").unwrap(), "{sql}");
    assert!(format!("{values:?}").contains("'rust' & 'forg':*"));
}

#[test]
fn mysql_statements_bind_the_boolean_string_and_drop_short_terms() {
    let spec = resolve::<post::Model>().unwrap();
    let built = build::<post::Model>(Backend::MySql, &spec, plan("go rust forg", true), 3, 0, 15);
    let (sql, values) = built.page.build(MysqlQueryBuilder);
    assert!(
        sql.contains("MATCH(`posts`.`title`, `posts`.`body`) AGAINST (? IN BOOLEAN MODE)"),
        "{sql}"
    );
    assert!(format!("{values:?}").contains("+rust +forg*"));
    assert!(!format!("{values:?}").contains("+go"));
    assert_eq!(built.in_rust, ["title"]);
    // Only words InnoDB does not index (short words, stopwords): no text condition, like an empty search.
    let built = build::<post::Model>(Backend::MySql, &spec, plan("the a of", false), 3, 0, 15);
    let (sql, _) = built.page.build(MysqlQueryBuilder);
    assert!(!sql.contains("MATCH") && !built.scored, "{sql}");
}

#[test]
fn without_terms_there_is_no_text_condition_and_the_newest_key_comes_first() {
    let spec = resolve::<post::Model>().unwrap();
    let built = build::<post::Model>(Backend::Sqlite, &spec, plan("  (*) ", true), 0, 0, 15);
    let (sql, _) = built.page.build(SqliteQueryBuilder);
    assert!(
        !sql.contains("MATCH") && !sql.contains("posts_search"),
        "{sql}"
    );
    assert!(sql.contains("ORDER BY \"posts\".\"id\" DESC"), "{sql}");
    assert!(!built.scored && built.marked.is_empty());
    let mut ordered = plan("rust", false);
    ordered.order = Some(("id".to_owned(), Direction::Asc));
    let built = build::<post::Model>(Backend::Sqlite, &spec, ordered, 0, 0, 15);
    let (sql, _) = built.page.build(SqliteQueryBuilder);
    assert!(
        sql.contains("ORDER BY \"posts\".\"id\" ASC, \"posts\".\"id\" DESC"),
        "{sql}"
    );
}
