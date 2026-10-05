//! Model listeners (`AppBuilder::model_listener`) and pagination (`Page`, `PageQuery`, `Record::paginate`).
#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::sync::{Arc, Mutex};

use smeltery_core::db::{
    ModelChange, ModelEvent, ModelListener, Page, PageQuery, listeners_paused, paginate,
    without_listeners,
};
use smeltery_core::testing::TestApp;
use smeltery_core::{BoxFuture, Result};

mod post {
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "posts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(unique)]
        pub title: String,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

use post::Model as Post;
use smeltery_core::db::prelude::*;

/// What a listener saw: the change, the table, the title, and whether the row was already visible in the
/// database (read through the same pool while the listener runs).
type Seen = Arc<Mutex<Vec<(ModelChange, &'static str, String, bool)>>>;

struct Recorder(Seen);

impl ModelListener for Recorder {
    fn changed<'a>(&'a self, db: &'a Db, event: ModelEvent<'a>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(post) = event.model::<Post>() else {
                return;
            };
            assert!(event.is::<Post>());
            // `sqlite::memory:` has one connection: had the write still held it (a transaction), this read would
            // wait for the pool's timeout and fail.
            let visible = Post::find(db, post.id).await.unwrap().is_some();
            self.0
                .lock()
                .unwrap()
                .push((event.change, event.table, post.title.clone(), visible));
        })
    }
}

struct Panics;

impl ModelListener for Panics {
    fn changed<'a>(&'a self, _db: &'a Db, _event: ModelEvent<'a>) -> BoxFuture<'a, ()> {
        Box::pin(async { panic!("a listener bug") })
    }
}

fn app(listeners: impl FnOnce(smeltery_core::AppBuilder) -> smeltery_core::AppBuilder) -> TestApp {
    let app = TestApp::new(listeners);
    let db = app.db();
    app.block_on(async {
        db.execute("CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT NOT NULL UNIQUE)")
            .await
            .unwrap();
    });
    app
}

fn new_post(title: &str) -> post::ActiveModel {
    post::ActiveModel {
        title: Set(title.to_owned()),
        ..Default::default()
    }
}

#[test]
fn listeners_hear_each_write_once_after_it_happened() {
    let seen = Seen::default();
    let recorder = Recorder(seen.clone());
    let app = app(move |b| b.model_listener(recorder));
    let db = app.db();
    app.block_on(async {
        let post = Post::create(&db, new_post("forge")).await.unwrap();
        let post = post
            .update(&db, |m| m.title = Set("anvil".into()))
            .await
            .unwrap();
        // Nothing set (and no `updated_at` column to stamp): no write, no event.
        let post = post.update(&db, |_| {}).await.unwrap();
        post.delete(&db).await.unwrap();
        // Already gone: SeaORM deletes nothing, and nobody is told twice.
        post.delete(&db).await.unwrap();
        // A failed write tells nobody.
        Post::create(&db, new_post("a")).await.unwrap();
        assert!(Post::create(&db, new_post("a")).await.is_err());
        // Writes that do not go through `Record` are not seen.
        db.execute("INSERT INTO posts (title) VALUES ('raw')")
            .await
            .unwrap();
        post::Entity::delete_many().exec(db.conn()).await.unwrap();
    });
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            (ModelChange::Created, "posts", "forge".to_owned(), true),
            (ModelChange::Updated, "posts", "anvil".to_owned(), true),
            // The row as it was, after it is gone.
            (ModelChange::Deleted, "posts", "anvil".to_owned(), false),
            (ModelChange::Created, "posts", "a".to_owned(), true),
        ]
    );
}

#[test]
fn without_listeners_skips_them_in_this_task_only() {
    let seen = Seen::default();
    let recorder = Recorder(seen.clone());
    let app = app(move |b| b.model_listener(recorder));
    let db = app.db();
    app.block_on(async {
        assert!(!listeners_paused());
        without_listeners(async {
            assert!(listeners_paused());
            Post::create(&db, new_post("quiet")).await.unwrap();
        })
        .await;
        Post::create(&db, new_post("loud")).await.unwrap();
    });
    let titles: Vec<String> = seen.lock().unwrap().iter().map(|s| s.2.clone()).collect();
    assert_eq!(titles, ["loud"]);
}

#[test]
fn a_panicking_listener_never_fails_the_write_or_the_others() {
    let seen = Seen::default();
    let recorder = Recorder(seen.clone());
    let app = app(move |b| b.model_listener(Panics).model_listener(recorder));
    let db = app.db();
    let post: Result<Post> = app.block_on(Post::create(&db, new_post("kept")));
    assert_eq!(post.unwrap().title, "kept");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn record_paginate_and_paginate_return_bounded_pages() {
    let app = app(|b| b);
    let db = app.db();
    app.block_on(async {
        for n in 1..=7 {
            Post::create(&db, new_post(&format!("p{n}"))).await.unwrap();
        }
        let page: Page<Post> = Post::paginate(&db, PageQuery::new(2, 3)).await.unwrap();
        let titles: Vec<&str> = page.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["p4", "p5", "p6"]);
        assert_eq!(
            (page.page, page.per_page, page.total, page.last_page),
            (2, 3, 7, 3)
        );
        assert!(page.has_next() && page.has_previous());

        let last = Post::paginate(&db, PageQuery::new(3, 3)).await.unwrap();
        assert_eq!(last.items.len(), 1);
        assert!(!last.has_next());
        // Past the end: no items, the totals still right.
        let past = Post::paginate(&db, PageQuery::new(9, 3)).await.unwrap();
        assert!(past.is_empty());
        assert_eq!((past.page, past.total, past.last_page), (9, 7, 3));

        // A filtered, ordered select.
        let select = Post::query()
            .filter(post::Column::Id.gt(2))
            .order_by_desc(post::Column::Id);
        let page = paginate(&db, select, PageQuery::from_query("per_page=2"))
            .await
            .unwrap();
        let titles: Vec<&str> = page.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["p7", "p6"]);
        assert_eq!((page.total, page.last_page), (5, 3));
    });
}

async fn listing(page: PageQuery) -> String {
    format!("{} {} {}", page.page(), page.per_page(), page.offset())
}

#[test]
fn the_page_query_extractor_clamps_and_never_rejects() {
    let app = TestApp::new(|b| {
        b.routes(|r| {
            r.get("/posts", listing);
        })
    });
    for (url, want) in [
        ("/posts", "1 15 0"),
        ("/posts?page=3&per_page=10", "3 10 20"),
        ("/posts?page=0&per_page=100000", "1 100 0"),
        ("/posts?page=abc&per_page=-1", "1 15 0"),
        ("/posts?page=99999999999999999999999", "10000 15 149985"),
    ] {
        let res = app.get(url);
        assert_eq!(res.status(), 200, "{url}");
        assert_eq!(res.text(), want, "{url}");
    }
}

/// Says it started, then waits for its gate.
struct Gated {
    started: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::Notify>,
}

impl ModelListener for Gated {
    fn changed<'a>(&'a self, _db: &'a Db, _event: ModelEvent<'a>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.started.notify_one();
            self.gate.notified().await;
        })
    }
}

/// On the app's database the listeners run on a task the app owns: a caller cancelled while they run (a request
/// timeout, a closed connection) does not cut them short. The caller is dropped as soon as the first listener has
/// started (no timing involved), then that listener's gate opens and the second listener must still run.
#[test]
fn a_cancelled_caller_does_not_skip_the_listeners() {
    let seen = Seen::default();
    let recorder = Recorder(seen.clone());
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let gated = Gated {
        started: started.clone(),
        gate: gate.clone(),
    };
    let app = app(move |b| b.model_listener(gated).model_listener(recorder));
    let db = app.db();
    app.block_on(async {
        let finished = tokio::select! {
            _ = Post::create(&db, new_post("kept")) => true,
            () = started.notified() => false,
        };
        assert!(
            !finished,
            "the caller was dropped while the first listener ran"
        );
        gate.notify_one();
        let start = std::time::Instant::now();
        while seen.lock().unwrap().is_empty() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "the second listener ran"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    });
    assert_eq!(
        seen.lock().unwrap().clone(),
        [(ModelChange::Created, "posts", "kept".to_owned(), true)]
    );
}

#[test]
fn listeners_can_be_unit_tested_with_a_built_event() {
    let seen = Seen::default();
    let app = app(|b| b);
    let db = app.db();
    let post = app.block_on(Post::create(&db, new_post("unit"))).unwrap();
    let recorder = Recorder(seen.clone());
    app.block_on(recorder.changed(&db, ModelEvent::new(ModelChange::Updated, "posts", &post)));
    assert_eq!(seen.lock().unwrap()[0].2, "unit");
}

/// A listener that writes the table it listens to, without `without_listeners`.
struct Echo(Arc<std::sync::atomic::AtomicU32>);

impl ModelListener for Echo {
    fn changed<'a>(&'a self, db: &'a Db, event: ModelEvent<'a>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(post) = event.model::<Post>() else {
                return;
            };
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = Post::create(db, new_post(&format!("{}+", post.title))).await;
        })
    }
}

/// Sweep W7-05: a listener that writes what it listens to stops after `MAX_LISTENER_DEPTH` levels instead of looping
/// forever (the original write returns).
#[test]
fn a_listener_that_writes_its_own_table_stops() {
    let heard = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let echo = Echo(Arc::clone(&heard));
    let app = app(move |b| b.model_listener(echo));
    let db = app.db();
    app.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            Post::create(&db, new_post("a")),
        )
        .await
        .expect("the chain of listener writes never ended")
        .unwrap();
    });
    let max = smeltery_core::db::MAX_LISTENER_DEPTH;
    assert_eq!(heard.load(std::sync::atomic::Ordering::SeqCst), max);
    let count = app.block_on(async { post::Entity::find().count(db.conn()).await.unwrap() });
    assert_eq!(count, u64::from(max) + 1);
}
