//! The memory engine (`testing::fake`): documents written by model events, hydration that re-checks every hit in
//! SQL, the engine rules (no `query`, safe filter strings), import / sync / flush, console commands, build checks.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::process::ExitCode;

use smeltery_core::db::PageQuery;
use smeltery_core::db::prelude::*;
use smeltery_core::testing::TestApp;
use smeltery_prospect::{Driver, IndexSpec, ProspectError, ProspectExt as _, Searchable, testing};
use support::*;

fn prospect_error(e: &smeltery_core::Error) -> &ProspectError {
    ProspectError::of(e).unwrap_or_else(|| panic!("not a Prospect error: {e}"))
}

#[test]
fn model_events_write_the_memory_engine() {
    let app = app();
    let fake = testing::fake(&app);
    assert_eq!(prospect(&app).driver(), Driver::Memory);
    let db = app.db();
    let a = post(&app, "forge", Some("body"), 1);
    fake.assert_indexed::<Post>(a.id);
    fake.assert_synced_times::<Post>(1);
    // Only declared columns are in a document.
    let mut columns = fake.indexed_columns::<Post>(a.id).unwrap();
    columns.sort();
    assert_eq!(
        columns,
        ["body", "created_at", "id", "published", "title", "user_id"]
    );
    let a = app
        .block_on(a.update(&db, |m| m.published = Set(false)))
        .unwrap();
    fake.assert_not_indexed::<Post>(a.id);
    app.block_on(a.update(&db, |m| m.published = Set(true)))
        .unwrap();
    fake.assert_indexed::<Post>(a.id);
    app.block_on(a.delete(&db)).unwrap();
    fake.assert_not_indexed::<Post>(a.id);
    fake.assert_synced_times::<Post>(4);
    // Paused writes are not seen; `sync` brings them in.
    let b = app
        .block_on(prospect(&app).paused(Post::create(
            &db,
            post::ActiveModel {
                title: Set("quiet".into()),
                user_id: Set(1),
                published: Set(true),
                ..Default::default()
            },
        )))
        .unwrap();
    fake.assert_not_indexed::<Post>(b.id);
    app.block_on(prospect(&app).sync::<Post>([b.id, 999]))
        .unwrap();
    fake.assert_indexed::<Post>(b.id);
    assert_eq!(
        fake.indexed_text::<Post>(b.id, "title").as_deref(),
        Some("quiet")
    );
}

#[test]
fn memory_searches_follow_the_same_rules() {
    let app = app();
    let _fake = testing::fake(&app);
    post(&app, "Notes", Some("about the forge"), 1);
    post(&app, "Forge basics", Some("notes"), 2);
    assert_eq!(titles(&app, "forg"), ["Forge basics", "Notes"]);
    assert_eq!(titles(&app, "forge basics"), ["Forge basics"]);
    assert!(titles(&app, "f").is_empty());
    let prospect = prospect(&app);
    let page = app
        .block_on(
            Post::search(&prospect, "forge")
                .where_eq("user_id", 1)
                .highlight(["body"])
                .paginate(PageQuery::default()),
        )
        .unwrap();
    assert_eq!(page.total, 1);
    let body = page.items[0].highlights.get("body").unwrap();
    assert_eq!(body.segments().iter().filter(|s| s.matched).count(), 1);
}

#[test]
fn a_stale_engine_document_is_not_returned() {
    let app = app();
    let fake = testing::fake(&app);
    let db = app.db();
    let secret = doc(&app, "merger plans", 1, "memo");
    fake.assert_indexed::<Doc>(secret.id);
    // Moved to another team by a write the engine never hears about (raw SQL): its document still says team 1.
    app.block_on(db.execute_with(
        "UPDATE docs SET team_id = 2 WHERE id = ?",
        [secret.id.into()],
    ))
    .unwrap();
    let prospect = prospect(&app);
    let hits = app
        .block_on(Doc::search(&prospect, "merger").within(1).get(10))
        .unwrap();
    assert!(hits.is_empty(), "team 1 must not see a record of team 2");
    // Unpublished by raw SQL: hidden the same way.
    let p = post(&app, "draft forge", None, 1);
    app.block_on(db.execute_with("UPDATE posts SET published = 0 WHERE id = ?", [p.id.into()]))
        .unwrap();
    assert!(titles(&app, "forge").is_empty());
}

#[test]
fn engine_rules_query_is_database_only_and_filter_strings_are_checked() {
    let app = app();
    let _fake = testing::fake(&app);
    doc(&app, "plans", 1, "memo");
    let prospect = prospect(&app);
    let err = app
        .block_on(
            Post::search(&prospect, "x")
                .query(|s| s.filter(post::Column::UserId.eq(1)))
                .get(5),
        )
        .unwrap_err();
    assert!(
        matches!(prospect_error(&err), ProspectError::Unsupported(_)),
        "{err}"
    );
    let err = app
        .block_on(
            Doc::search(&prospect, "plans")
                .within(1)
                .where_eq("kind", "a' OR '1'='1")
                .get(5),
        )
        .unwrap_err();
    assert!(
        matches!(prospect_error(&err), ProspectError::FilterValue { .. }),
        "{err}"
    );
    assert_eq!(
        app.block_on(
            Doc::search(&prospect, "plans")
                .within(1)
                .where_eq("kind", "memo")
                .count()
        )
        .unwrap(),
        1
    );
}

#[test]
fn import_and_flush() {
    let app = app();
    let a = post(&app, "early", None, 1);
    let fake = testing::fake(&app);
    fake.assert_not_indexed::<Post>(a.id);
    let prospect = prospect(&app);
    assert_eq!(app.block_on(prospect.import::<Post>()).unwrap(), 1);
    fake.assert_indexed::<Post>(a.id);
    app.block_on(prospect.flush::<Post>()).unwrap();
    assert_eq!(fake.count::<Post>(), 0);
}

#[test]
fn the_database_driver_refuses_flush_and_import_rebuilds() {
    let app = app();
    post(&app, "forge", None, 1);
    let prospect = prospect(&app);
    assert!(app.block_on(prospect.flush::<Post>()).is_err());
    assert_eq!(app.block_on(prospect.import::<Post>()).unwrap(), 1);
    assert_eq!(titles(&app, "forge"), ["forge"]);
}

fn command(words: &[&str]) -> (ExitCode, String) {
    let app = app();
    post(&app, "forge", None, 1);
    let mut settings = smeltery_core::config::Settings::from_env();
    settings.env = "testing".to_owned();
    settings.database_url = "sqlite::memory:".to_owned();
    let builder = build(smeltery_core::AppBuilder::new(settings));
    let words: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    let mut out = Vec::new();
    let result = app.block_on(smeltery_core::console::dispatch(builder, &words, &mut out));
    let mut text = String::from_utf8(out).unwrap();
    match result {
        Ok(code) => (code, text),
        Err(e) => {
            text.push_str(&e.to_string());
            (ExitCode::FAILURE, text)
        }
    }
}

#[test]
fn console_commands() {
    // A fresh in-memory database: no tables, so the status names the missing index.
    let (code, text) = command(&["prospect:status"]);
    assert_eq!(code, ExitCode::SUCCESS, "{text}");
    assert!(text.contains("Driver: database"), "{text}");
    assert!(text.contains("posts:") && text.contains("docs:"), "{text}");
    assert!(
        text.contains("MISSING") || text.contains("not checked"),
        "{text}"
    );
    let (code, text) = command(&["prospect:flush", "posts"]);
    assert_ne!(code, ExitCode::SUCCESS, "{text}");
    assert!(text.contains("nothing to flush"), "{text}");
    let (code, text) = command(&["prospect:import", "nope\u{1b}[31m"]);
    assert_ne!(code, ExitCode::SUCCESS, "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");
}

mod broken {
    //! Models whose specs do not fit their entity.
    pub mod note {
        use smeltery_core::db::prelude::*;

        #[sea_orm::model]
        #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
        #[sea_orm(table_name = "notes")]
        pub struct Model {
            #[sea_orm(primary_key)]
            pub id: i64,
            pub body: String,
            pub count: i32,
        }

        impl ActiveModelBehavior for ActiveModel {}
    }
}

use broken::note::Model as Note;

impl Searchable for Note {
    fn index(i: &mut IndexSpec) {
        i.text("count");
    }
}

#[test]
fn bad_specs_and_double_registrations_fail_the_build() {
    let build_err = |register: fn(&mut smeltery_prospect::Models)| {
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.env = "testing".to_owned();
        settings.database_url = "sqlite::memory:".to_owned();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(
            smeltery_core::AppBuilder::new(settings)
                .prospect(register)
                .build(),
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default()
    };
    let err = build_err(|p| {
        p.model::<Note>();
    });
    assert!(err.contains("`count` must be a string column"), "{err}");
    let err = build_err(|p| {
        p.model::<Post>().model::<Post>();
    });
    assert!(err.contains("registered twice"), "{err}");
    assert!(
        build_err(|p| {
            p.model::<Post>();
        })
        .is_empty()
    );
}

#[test]
fn an_unregistered_model_is_an_error() {
    let app = TestApp::new(|b| b.prospect(|_| {}));
    let prospect = prospect(&app);
    let err = app
        .block_on(Post::search(&prospect, "x").get(5))
        .unwrap_err();
    assert!(
        matches!(prospect_error(&err), ProspectError::NotRegistered),
        "{err}"
    );
}
