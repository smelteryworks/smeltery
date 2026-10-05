//! The search suite on PostgreSQL and MySQL (`--features backend-tests`, ignored without a server):
//! `DATABASE_URL_PG` / `DATABASE_URL_MYSQL` name a database the run may empty (it migrates fresh). The same suite runs
//! on a SQLite file in every `--all-features` run, so the suite itself is always checked.
#![cfg(feature = "backend-tests")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use smeltery_core::db::PageQuery;
use smeltery_core::db::migration::Migrator;
use smeltery_core::testing::TestApp;
use smeltery_prospect::{ProspectError, Searchable as _};
use support::*;

fn app_on(url: String) -> TestApp {
    let app = TestApp::new(move |b| {
        let mut b = build(b);
        b.settings_mut().database_url = url;
        b
    });
    let mut migrator = Migrator::new();
    migrations(&mut migrator);
    app.block_on(migrator.fresh(&app.db())).unwrap();
    app
}

fn suite(url: String) {
    let app = app_on(url);
    let db = app.db();
    post(&app, "Notes", Some("about the forge and the anvil"), 1);
    let basics = post(&app, "Forge basics", Some("notes"), 2);
    // Ranking (title above body, except on MySQL, which has no column weights), prefixes, AND.
    let mut found = titles(&app, "forge");
    found.sort();
    assert_eq!(found, ["Forge basics", "Notes"]);
    assert_eq!(titles(&app, "forge basics"), ["Forge basics"]);
    assert_eq!(titles(&app, "anv").len(), 1, "prefix");
    // Triggers / the generated column / FULLTEXT follow raw SQL writes.
    app.block_on(db.execute_with(
        match db.backend() {
            smeltery_core::db::Backend::Postgres => "UPDATE posts SET title = $1 WHERE id = $2",
            _ => "UPDATE posts SET title = ? WHERE id = ?",
        },
        ["Crucible basics".into(), basics.id.into()],
    ))
    .unwrap();
    assert_eq!(titles(&app, "crucible"), ["Crucible basics"]);
    // Search syntax from users never reaches the parser.
    let prospect = prospect(&app);
    for q in [
        "title:x",
        "NEAR(",
        "\"",
        "*",
        "-",
        "+forge -notes",
        "!forge & notes",
        "forge:*",
        "'); --",
        "(a|b)",
    ] {
        let r = app.block_on(Post::search(&prospect, q).paginate(PageQuery::default()));
        assert!(r.is_ok(), "{q}: {:?}", r.err());
    }
    // Highlights as segments; filters and scopes.
    let hits = app
        .block_on(Post::search(&prospect, "anvil").highlight(["body"]).get(5))
        .unwrap();
    assert!(hits[0].highlights.get("body").unwrap().has_match());
    assert_eq!(
        app.block_on(
            Post::search(&prospect, "crucible")
                .where_eq("user_id", 1)
                .count()
        )
        .unwrap(),
        0
    );
    doc(&app, "plans", 1, "memo");
    let err = app
        .block_on(Doc::search(&prospect, "plans").get(5))
        .unwrap_err();
    assert!(matches!(
        ProspectError::of(&err),
        Some(ProspectError::ScopeMissing { .. })
    ));
    assert_eq!(
        app.block_on(Doc::search(&prospect, "plans").within(1).count())
            .unwrap(),
        1
    );
}

#[test]
#[ignore = "needs DATABASE_URL_PG"]
fn search_on_postgres() {
    suite(std::env::var("DATABASE_URL_PG").expect("DATABASE_URL_PG"));
}

#[test]
#[ignore = "needs DATABASE_URL_MYSQL"]
fn search_on_mysql() {
    suite(std::env::var("DATABASE_URL_MYSQL").expect("DATABASE_URL_MYSQL"));
}

#[test]
fn search_on_a_sqlite_file() {
    let dir = tempfile::tempdir().unwrap();
    suite(format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("app.sqlite").display()
    ));
}
