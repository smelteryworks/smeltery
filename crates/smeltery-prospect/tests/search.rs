//! The database driver on SQLite (FTS5), in memory: the migration helper, the triggers, ranking, prefixes,
//! highlights, filters, scopes, refinements, bounds and the attacks of SECURITY.md §3.19.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use smeltery_core::db::migration::{Migrator, Schema};
use smeltery_core::db::prelude::*;
use smeltery_core::db::{Page, PageQuery};
use smeltery_core::testing::TestApp;
use smeltery_prospect::migration::{SearchIndex, Weight};
use smeltery_prospect::{Direction, Hit, ProspectError, ProspectExt as _, Searchable as _};
use support::*;

fn prospect_error(e: &smeltery_core::Error) -> &ProspectError {
    ProspectError::of(e).unwrap_or_else(|| panic!("not a Prospect error: {e}"))
}

#[test]
fn the_triggers_keep_the_index_current_for_every_kind_of_write() {
    let app = app();
    let db = app.db();
    let forge = post(&app, "The forge", Some("hammer and anvil"), 1);
    assert_eq!(titles(&app, "forge"), ["The forge"]);
    // Record update.
    let forge = app
        .block_on(forge.update(&db, |m| m.title = Set("The smithy".into())))
        .unwrap();
    assert!(titles(&app, "forge").is_empty());
    assert_eq!(titles(&app, "smithy"), ["The smithy"]);
    // Raw SQL and SeaORM's bulk calls do not go through `Record`, and the index follows them all the same.
    app.block_on(db.execute_with(
        "INSERT INTO posts (title, body, user_id, published) VALUES (?, ?, ?, ?)",
        ["Raw bellows".into(), "".into(), 1_i64.into(), true.into()],
    ))
    .unwrap();
    assert_eq!(titles(&app, "bellows"), ["Raw bellows"]);
    app.block_on(
        post::Entity::update_many()
            .col_expr(post::Column::Title, Expr::value("Bulk crucible"))
            .filter(post::Column::Title.eq("Raw bellows"))
            .exec(db.conn()),
    )
    .unwrap();
    assert!(titles(&app, "bellows").is_empty());
    assert_eq!(titles(&app, "crucible"), ["Bulk crucible"]);
    // Record delete, then a bulk delete.
    app.block_on(forge.delete(&db)).unwrap();
    assert!(titles(&app, "smithy").is_empty());
    app.block_on(post::Entity::delete_many().exec(db.conn()))
        .unwrap();
    assert!(titles(&app, "crucible").is_empty());
}

#[test]
fn title_matches_rank_above_body_matches_and_the_last_term_is_a_prefix() {
    let app = app();
    post(&app, "Notes", Some("about the forge"), 1);
    post(&app, "Forge basics", Some("notes"), 1);
    assert_eq!(titles(&app, "forge"), ["Forge basics", "Notes"]);
    // Prefix: two letters or more.
    assert_eq!(titles(&app, "forg"), ["Forge basics", "Notes"]);
    assert_eq!(titles(&app, "fo").len(), 2);
    assert!(titles(&app, "f").is_empty(), "one letter is a whole word");
    // Every term must match.
    assert_eq!(titles(&app, "forge basics"), ["Forge basics"]);
    assert!(titles(&app, "forge zebra").is_empty());
    let prospect = prospect(&app);
    let hits = app
        .block_on(Post::search(&prospect, "forge").get(10))
        .unwrap();
    assert!(hits[0].score.unwrap() > hits[1].score.unwrap());
}

#[test]
fn highlights_are_text_segments_and_markup_stays_text() {
    let app = app();
    post(
        &app,
        "<script>alert(1)</script> forge",
        Some(&format!(
            "{} forge {}",
            "word ".repeat(500),
            "tail ".repeat(500)
        )),
        1,
    );
    let prospect = prospect(&app);
    let hits: Vec<Hit<Post>> = app
        .block_on(
            Post::search(&prospect, "forge")
                .highlight(["title", "body"])
                .get(5),
        )
        .unwrap();
    let title = hits[0].highlights.get("title").unwrap();
    let matched: Vec<&str> = title
        .segments()
        .iter()
        .filter(|s| s.matched)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(matched, ["forge"]);
    assert_eq!(title.plain(), "<script>alert(1)</script> forge");
    // A long column gets a snippet, not the whole text.
    let body = hits[0].highlights.get("body").unwrap();
    assert!(body.has_match());
    assert!(body.plain().len() < 1_000, "{}", body.plain().len());
    // Serialized for a template or Alloy: the model's fields, `_score`, `_highlights` as segments.
    let json = serde_json::to_value(&hits[0]).unwrap();
    assert_eq!(json["title"], "<script>alert(1)</script> forge");
    assert!(json["_score"].is_f64());
    assert_eq!(json["_highlights"]["title"][1]["matched"], true);
}

#[test]
fn markup_in_content_is_escaped_in_highlights() {
    let app = app();
    post(&app, "a <b onmouseover=x>forge</b> \u{E000}<i>", None, 1);
    let prospect = prospect(&app);
    let hits = app
        .block_on(Post::search(&prospect, "forge").highlight(["title"]).get(5))
        .unwrap();
    let rendered: String = hits[0]
        .highlights
        .get("title")
        .unwrap()
        .segments()
        .iter()
        .map(|s| {
            let text = smeltery_core::html::escape(&s.text).into_owned();
            if s.matched {
                format!("<mark>{text}</mark>")
            } else {
                text
            }
        })
        .collect();
    assert!(
        !rendered.contains("<b ") && !rendered.contains("<i>"),
        "{rendered}"
    );
    assert!(rendered.contains("<mark>forge</mark>"), "{rendered}");
}

/// Sweep W7-01: FTS5 copies a stored U+E000 … U+E001 pair into its highlight; it must not come back as a match.
#[test]
fn a_stored_marker_pair_never_marks_words() {
    let app = app();
    post(&app, "a \u{E000}evil\u{E001} rust", None, 1);
    let prospect = prospect(&app);
    let hits = app
        .block_on(Post::search(&prospect, "rust").highlight(["title"]).get(5))
        .unwrap();
    let h = hits[0].highlights.get("title").unwrap();
    for s in h.segments() {
        assert!(!(s.matched && s.text.contains("evil")), "{h:?}");
    }
    assert!(
        h.segments().iter().any(|s| s.matched && s.text == "rust"),
        "{h:?}"
    );
    assert_eq!(h.plain(), "a evil rust");
}

#[test]
fn fts5_operators_in_user_text_are_terms() {
    let app = app();
    post(&app, "near title and or not", Some("x y"), 1);
    for q in [
        "title:x",
        "NEAR(",
        "\"",
        "*",
        "^",
        "-",
        "AND",
        "OR",
        "NOT",
        "NEAR(near title)",
        "title:near",
        "\"near",
        "near*",
        "(near) OR title",
        "{title}: near",
        "near + title",
        "'); DROP TABLE posts; --",
    ] {
        let prospect = prospect(&app);
        let result = app.block_on(Post::search(&prospect, q).paginate(PageQuery::default()));
        assert!(result.is_ok(), "{q}: {:?}", result.err());
    }
    // Operator words are plain words.
    assert_eq!(titles(&app, "near AND title"), ["near title and or not"]);
    assert_eq!(titles(&app, "title:near"), ["near title and or not"]);
    assert_eq!(titles(&app, "NOT"), ["near title and or not"]);
    assert!(
        titles(&app, "-near").len() == 1,
        "a minus is not an exclusion"
    );
}

/// Random text from a pool of FTS5 syntax, quotes, Unicode and control characters never makes a search fail.
#[test]
fn random_text_never_errors() {
    let app = app();
    post(&app, "forge anvil", Some("hammer"), 1);
    let prospect = prospect(&app);
    let pool: Vec<char> = "abcXYZ019 \"'*:^-+(){}[],.;!?\\/|&%$#@~`<>=_\t\n\r\u{0}\u{E000}\u{E001}éß東京\u{301}\u{202E}NEAROT"
        .chars()
        .collect();
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    app.block_on(async {
        for _ in 0..10_000 {
            let len = usize::try_from(next() % 24).unwrap();
            let q: String = (0..len)
                .map(|_| pool[usize::try_from(next() % pool.len() as u64).unwrap()])
                .collect();
            let result = Post::search(&prospect, &q).count().await;
            assert!(result.is_ok(), "{q:?}: {:?}", result.err());
        }
    });
}

#[test]
fn filters_sorts_and_only_when_are_sql_conditions() {
    let app = app();
    let db = app.db();
    let a = post(&app, "forge one", None, 1);
    let b = post(&app, "forge two", None, 2);
    let c = post(&app, "forge three", None, 3);
    let prospect = prospect(&app);
    let search = |f: &dyn Fn(
        smeltery_prospect::Search<Post>,
    ) -> smeltery_prospect::Search<Post>|
     -> Vec<i64> {
        app.block_on(f(Post::search(&prospect, "forge")).keys(50))
            .unwrap()
    };
    assert_eq!(search(&|s| s.where_eq("user_id", 2)), [b.id]);
    assert_eq!(search(&|s| s.where_in("user_id", [1, 3])), [c.id, a.id]);
    assert_eq!(search(&|s| s.where_not_in("user_id", [1, 3])), [b.id]);
    assert_eq!(search(&|s| s.where_between("user_id", 2, 3)), [c.id, b.id]);
    assert!(search(&|s| s.where_in("user_id", Vec::<i64>::new())).is_empty());
    assert_eq!(
        search(&|s| s.order_by("id", Direction::Asc)),
        [a.id, b.id, c.id]
    );
    // Unpublished rows are not found.
    app.block_on(b.update(&db, |m| m.published = Set(false)))
        .unwrap();
    assert_eq!(search(&|s| s), [c.id, a.id]);
    // A refinement in the same statement: pages and totals stay exact.
    let page: Page<Hit<Post>> = app
        .block_on(
            Post::search(&prospect, "forge")
                .query(|select| select.filter(post::Column::UserId.ne(3)))
                .paginate(PageQuery::default()),
        )
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, a.id);
    // The count agrees with a plain query.
    let count = app
        .block_on(Post::search(&prospect, "forge").count())
        .unwrap();
    let plain = app
        .block_on(
            Post::query()
                .filter(post::Column::Published.eq(true))
                .count(db.conn()),
        )
        .unwrap();
    assert_eq!(count, plain);
}

#[test]
fn without_terms_the_filtered_rows_come_newest_first() {
    let app = app();
    let a = post(&app, "one", None, 1);
    let b = post(&app, "two", None, 1);
    let prospect = prospect(&app);
    let page = app
        .block_on(Post::search(&prospect, "  ").paginate(PageQuery::default()))
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(
        page.items.iter().map(|h| h.id).collect::<Vec<_>>(),
        [b.id, a.id]
    );
    assert!(page.items[0].score.is_none() && page.items[0].highlights.is_empty());
}

#[test]
fn a_scoped_search_without_a_scope_fails() {
    let app = app();
    doc(&app, "plans", 1, "memo");
    doc(&app, "plans", 2, "memo");
    let prospect = prospect(&app);
    let err = app
        .block_on(Doc::search(&prospect, "plans").get(10))
        .unwrap_err();
    assert!(
        matches!(prospect_error(&err), ProspectError::ScopeMissing { .. }),
        "{err}"
    );
    assert_eq!(err.status(), 500);
    let mine = app
        .block_on(Doc::search(&prospect, "plans").within(1).get(10))
        .unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].team_id, 1);
    let all = app
        .block_on(Doc::search(&prospect, "plans").across_scopes().count())
        .unwrap();
    assert_eq!(all, 2);
    // The wrong type for the scope column is refused before any query.
    let err = app
        .block_on(Doc::search(&prospect, "plans").within("1").get(10))
        .unwrap_err();
    assert!(
        matches!(prospect_error(&err), ProspectError::FilterType { .. }),
        "{err}"
    );
}

#[test]
fn undeclared_columns_are_refused() {
    let app = app();
    let prospect = prospect(&app);
    let run = |s: smeltery_prospect::Search<Post>| app.block_on(s.get(5)).unwrap_err();
    for err in [
        run(Post::search(&prospect, "x").where_eq("title", "x")),
        run(Post::search(&prospect, "x").where_eq("password", 1)),
        run(Post::search(&prospect, "x").order_by("title", Direction::Asc)),
        run(Post::search(&prospect, "x").order_by("id; DROP TABLE posts", Direction::Asc)),
        run(Post::search(&prospect, "x").highlight(["user_id"])),
        run(Post::search(&prospect, "x").within(1)),
    ] {
        assert!(
            matches!(prospect_error(&err), ProspectError::Undeclared { .. }),
            "{err}"
        );
    }
    let err = run(Post::search(&prospect, "x").where_eq("user_id", "1"));
    assert!(
        matches!(prospect_error(&err), ProspectError::FilterType { .. }),
        "{err}"
    );
}

#[test]
fn per_page_and_page_are_clamped() {
    let app = app();
    for i in 0..5 {
        post(&app, &format!("forge {i}"), None, 1);
    }
    let prospect = prospect(&app);
    let page = app
        .block_on(
            Post::search(&prospect, "forge").paginate(PageQuery::from_query("page=2&per_page=2")),
        )
        .unwrap();
    assert_eq!(
        (page.page, page.per_page, page.total, page.last_page),
        (2, 2, 5, 3)
    );
    assert_eq!(page.items.len(), 2);
    let huge = app
        .block_on(
            Post::search(&prospect, "forge").paginate(PageQuery::from_query("per_page=100000")),
        )
        .unwrap();
    assert_eq!(huge.per_page, 100);
    let past = app
        .block_on(Post::search(&prospect, "forge").paginate(PageQuery::from_query("page=99999999")))
        .unwrap();
    assert!(past.is_empty());
    assert_eq!(past.page, 10_000);
    // Bounds on the builder: where_in values, highlight columns.
    let err = app
        .block_on(
            Post::search(&prospect, "forge")
                .where_in("user_id", 0..101)
                .get(5),
        )
        .unwrap_err();
    assert!(
        matches!(prospect_error(&err), ProspectError::Limit(_)),
        "{err}"
    );
    let ok = app.block_on(
        Post::search(&prospect, "forge")
            .where_in("user_id", 0..100)
            .get(500),
    );
    assert_eq!(ok.unwrap().len(), 5);
}

#[test]
fn a_missing_index_is_reported_and_the_migration_helper_runs_up_down_up() {
    let app = TestApp::new(|b| {
        b.prospect(|p| {
            p.model::<Post>();
        })
    });
    let db = app.db();
    let schema = Schema::new(&db);
    app.block_on(async {
        schema
            .create("posts", |t| {
                t.id();
                t.string("title");
                t.text("body").nullable();
                t.big_integer("user_id");
                t.boolean("published").default(true);
                t.timestamps();
            })
            .await
    })
    .unwrap();
    let prospect = prospect(&app);
    let err = app
        .block_on(Post::search(&prospect, "x").get(5))
        .unwrap_err();
    assert!(
        matches!(prospect_error(&err), ProspectError::IndexMissing { .. }),
        "{err}"
    );
    assert!(
        err.to_string().contains("SearchIndex::on(\"posts\")"),
        "{err}"
    );
    // An index over other columns is not accepted either.
    let wrong = SearchIndex::on("posts").text("title", Weight::A);
    app.block_on(wrong.create(&schema)).unwrap();
    assert!(app.block_on(Post::search(&prospect, "x").get(5)).is_err());
    app.block_on(wrong.drop(&schema)).unwrap();
    let right = SearchIndex::on("posts")
        .text("title", Weight::A)
        .text("body", Weight::B);
    // Rows that exist before the index are indexed by its `rebuild`.
    post(&app, "early forge", None, 1);
    app.block_on(right.create(&schema)).unwrap();
    assert_eq!(titles(&app, "forge"), ["early forge"]);
    // Creating it twice fails; down, then up again, works.
    assert!(app.block_on(right.create(&schema)).is_err());
    app.block_on(right.drop(&schema)).unwrap();
    app.block_on(right.drop(&schema)).unwrap();
    app.block_on(right.create(&schema)).unwrap();
    assert_eq!(titles(&app, "forge"), ["early forge"]);
    app.block_on(right.rebuild(&schema)).unwrap();
    assert_eq!(titles(&app, "forge"), ["early forge"]);
}

#[test]
fn migrate_fresh_twice_with_search_indexes() {
    let app = app();
    post(&app, "forge", None, 1);
    let db = app.db();
    let mut migrator = Migrator::new();
    migrations(&mut migrator);
    for _ in 0..2 {
        app.block_on(migrator.fresh(&db)).unwrap();
        assert!(titles(&app, "forge").is_empty());
        post(&app, "forge", None, 1);
        assert_eq!(titles(&app, "forge"), ["forge"]);
    }
    // Rolled back: the triggers and the FTS5 table go with the migration.
    app.block_on(migrator.rollback(&db, Some(1))).unwrap();
    let left = app
        .block_on(db.query_with(
            "SELECT name FROM sqlite_master WHERE name LIKE '%search%'",
            [],
        ))
        .unwrap();
    assert!(left.is_empty(), "{} objects left", left.len());
}

/// The README's Mold template renders highlight segments escaped, the matches in `<mark>`.
#[test]
fn the_readme_template_renders_highlights_escaped() {
    let app = app();
    post(&app, "<b>forge</b> & anvil", None, 1);
    let prospect = prospect(&app);
    let posts = app
        .block_on(
            Post::search(&prospect, "forge")
                .highlight(["title"])
                .paginate(PageQuery::default()),
        )
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("search.mold.html"), TEMPLATE).unwrap();
    let engine = smeltery::mold::Engine::new(dir.path());
    // As the README says: the handler hands the template each hit's segments in a field.
    let rows: Vec<_> = posts
        .items
        .iter()
        .map(|hit| {
            let title = hit
                .highlights
                .get("title")
                .map(|h| h.segments().to_vec())
                .unwrap_or_default();
            serde_json::json!({ "title": title })
        })
        .collect();
    let data =
        smeltery::mold::to_value(&serde_json::json!({ "posts": { "items": rows } })).unwrap();
    let html = engine
        .render("search", &data, &smeltery::mold::NoHost)
        .unwrap();
    assert!(
        html.contains("&lt;b&gt;<mark>forge</mark>&lt;/b&gt; &amp; anvil"),
        "{html}"
    );
}

/// The template in the README's "Search" section, byte for byte.
const TEMPLATE: &str = "@for(row in posts.items)
  <h2>@for(s in row.title)@if(s.matched)<mark>{{ s.text }}</mark>@else{{ s.text }}@endif{{-- --}}@endfor</h2>
@endfor
";

#[test]
fn the_readme_holds_the_tested_template() {
    let readme = include_str!("../../../README.md");
    assert!(
        readme.contains(TEMPLATE),
        "update the README's search template"
    );
}

async fn search_route(
    prospect: smeltery_prospect::Prospect,
    page: PageQuery,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> smeltery_core::Result<axum::Json<Page<Hit<Post>>>> {
    let text = q.get("q").cloned().unwrap_or_default();
    let posts = Post::search(&prospect, &text)
        .query(|select| select.filter(post::Column::UserId.gt(0)))
        .highlight(["title"])
        .paginate(page)
        .await?;
    Ok(axum::Json(posts))
}

/// A search runs in a handler (its future is `Send`, `query(…)` included) and answers the page as JSON.
#[test]
fn a_handler_searches_and_answers_json() {
    let app = TestApp::new(|b| {
        build(b).routes(|r| {
            r.get("/search", search_route);
        })
    });
    post(&app, "forge <b>", None, 1);
    let res = app.get("/search?q=%22forge%22%20(*)%20-&per_page=500");
    assert_eq!(res.status(), 200, "{}", res.text());
    let json: serde_json::Value = serde_json::from_str(&res.text()).unwrap();
    assert_eq!(json["per_page"], 100);
    assert_eq!(json["total"], 1);
    assert_eq!(json["items"][0]["title"], "forge <b>");
    assert_eq!(json["items"][0]["_highlights"]["title"][0]["matched"], true);
}

/// `INSERT OR REPLACE` (raw SQL) fires the delete trigger of the replaced row (core turns on SQLite's
/// `recursive_triggers`), so the index never keeps the old words, and FTS5's integrity check passes.
#[test]
fn insert_or_replace_keeps_the_index_right() {
    let app = app();
    let db = app.db();
    let p = post(&app, "forge alpha", None, 1);
    app.block_on(db.execute_with(
        "INSERT OR REPLACE INTO posts (id, title, body, user_id, published) VALUES (?, 'anvil beta', NULL, 1, 1)",
        [p.id.into()],
    ))
    .unwrap();
    assert!(titles(&app, "forge").is_empty());
    assert_eq!(titles(&app, "anvil"), ["anvil beta"]);
    app.block_on(
        db.execute("INSERT INTO posts_search(posts_search, rank) VALUES('integrity-check', 1)"),
    )
    .unwrap();
}

/// Sweep W7-07: a row whose key changes is indexed under its new key; a later row given the old key does not match
/// the old words.
#[test]
fn a_changed_key_moves_the_index_row() {
    let app = app();
    let db = app.db();
    let p = post(&app, "forge alpha", None, 1);
    app.block_on(db.execute_with(
        "UPDATE posts SET id = ? WHERE id = ?",
        [(p.id + 100).into(), p.id.into()],
    ))
    .unwrap();
    app.block_on(db.execute_with(
        "INSERT INTO posts (id, title, body, user_id, published) VALUES (?, 'anvil beta', NULL, 1, 1)",
        [p.id.into()],
    ))
    .unwrap();
    assert_eq!(titles(&app, "forge"), ["forge alpha"]);
    assert_eq!(titles(&app, "anvil"), ["anvil beta"]);
    app.block_on(
        db.execute("INSERT INTO posts_search(posts_search, rank) VALUES('integrity-check', 1)"),
    )
    .unwrap();
}

/// An FTS5 index keyed by another column than the model's key is refused (the join would return other rows).
#[test]
fn an_index_keyed_by_another_column_is_refused() {
    let app = TestApp::new(|b| {
        b.prospect(|p| {
            p.model::<Post>();
        })
    });
    let db = app.db();
    let schema = Schema::new(&db);
    app.block_on(async {
        schema
            .create("posts", |t| {
                t.id();
                t.string("title");
                t.text("body").nullable();
                t.big_integer("user_id");
                t.boolean("published").default(true);
                t.timestamps();
            })
            .await
    })
    .unwrap();
    let wrong = SearchIndex::on("posts")
        .key("user_id")
        .text("title", Weight::A)
        .text("body", Weight::B);
    app.block_on(wrong.create(&schema)).unwrap();
    let prospect = prospect(&app);
    let err = app
        .block_on(Post::search(&prospect, "x").get(5))
        .unwrap_err();
    assert!(
        matches!(prospect_error(&err), ProspectError::IndexMissing { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("content_rowid='id'"), "{err}");
}
