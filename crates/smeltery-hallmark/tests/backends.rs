//! The token suite on PostgreSQL and MySQL (`--features backend-tests`, ignored without a server): timestamps,
//! the unique index, the cap's ordering, pruning and the conditional `last_used_at` update on each backend.
//! `DATABASE_URL_PG` / `DATABASE_URL_MYSQL` name a database the run may empty (it migrates fresh).
#![cfg(feature = "backend-tests")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::time::Duration;

use sea_orm::prelude::ChronoUtc;
use smeltery_core::testing::TestApp;
use smeltery_hallmark::Hallmark;
use support::*;

fn app_on(url: String) -> TestApp {
    TestApp::new(move |b| {
        let mut b = build(Hallmark::new().max_tokens_per_user(2))(b);
        b.settings_mut().database_url = url;
        b
    })
}

fn suite(url: String) {
    let app = app_on(url);
    let ada = create_user(&app, "ada@example.com", "secret one");
    let first = create(&app, &ada, &["orders:read"]);
    // The cap is two: the second stays unused, so it is the least recently used when a third comes.
    let second = create(&app, &ada, &["*"]);
    let auth = format!("Bearer {}", first.plain_text());
    assert_eq!(get_with(&app, "/api/orders", &auth).status(), 200);
    // Upper-case hex is refused before the lookup, whatever the column's collation.
    let upper = format!("Bearer smt_{}", first.plain_text()[4..].to_uppercase());
    assert_eq!(get_with(&app, "/api/me", &upper).status(), 401);
    wait_until(&app, |app| last_used(app, &ada, first.token().id).is_some());
    let t = tokens(&app);
    let used = app
        .block_on(t.find(ada.id, first.token().id))
        .unwrap()
        .unwrap();
    assert!(used.last_used_at.is_some());
    let at = used.expires_at.unwrap();
    let want = ChronoUtc::now() + Duration::from_secs(365 * 86_400);
    assert!((want - at).num_seconds().abs() < 5, "{at}");
    let third = create(&app, &ada, &["*"]);
    let ids: Vec<i64> = app
        .block_on(t.list(ada.id))
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&first.token().id) && !ids.contains(&second.token().id));
    // Pruning compares timestamps in SQL. Creating the expired one evicts `first` (last used before `third` was
    // created), so `third` and the expired one remain, and pruning removes the expired one.
    app.block_on(t.create(
        ada.id,
        "old",
        &["*"],
        Some(ChronoUtc::now() - Duration::from_secs(3 * 86_400)),
    ))
    .unwrap();
    assert_eq!(
        app.block_on(t.prune_expired(Duration::from_secs(86_400)))
            .unwrap(),
        1
    );
    let left: Vec<i64> = app
        .block_on(t.list(ada.id))
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(left, [third.token().id]);
    assert_eq!(app.block_on(t.revoke_all(ada.id)).unwrap(), 1);
    assert_eq!(
        get_with(&app, "/api/me", &format!("Bearer {}", third.plain_text())).status(),
        401
    );
}

#[test]
#[ignore = "needs DATABASE_URL_PG"]
fn tokens_on_postgres() {
    suite(std::env::var("DATABASE_URL_PG").expect("DATABASE_URL_PG"));
}

#[test]
#[ignore = "needs DATABASE_URL_MYSQL"]
fn tokens_on_mysql() {
    suite(std::env::var("DATABASE_URL_MYSQL").expect("DATABASE_URL_MYSQL"));
}

/// The same suite on a SQLite file, so the suite itself is checked in every `--all-features` run.
#[test]
fn tokens_on_a_sqlite_file() {
    let dir = tempfile::tempdir().unwrap();
    suite(format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("app.sqlite").display()
    ));
}
