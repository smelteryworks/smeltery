//! `Record` and factories over a model written like `make:model` writes one.
#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use smeltery_core::Result;
use smeltery_core::db::Db;
use smeltery_core::db::factory::{Factory, Fake};
use smeltery_core::db::migration::{Migration, Migrator, Schema};

mod post {
    //! The `Post` model (table `posts`).
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "posts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub title: String,
        pub body: Option<String>,
        pub views: i32,
        pub created_at: Option<DateTimeUtc>,
        pub updated_at: Option<DateTimeUtc>,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

mod tag {
    //! A model without timestamps.
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "tags")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

use post::Model as Post;
use smeltery_core::db::prelude::*;
use tag::Model as Tag;

struct CreateTables;

impl Migration for CreateTables {
    fn name(&self) -> &'static str {
        "2026_10_03_000001_create_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("posts", |t| {
                t.id();
                t.string("title");
                t.text("body").nullable();
                t.integer("views").default(0);
                t.timestamps();
            })
            .await?;
        schema
            .create("tags", |t| {
                t.id();
                t.string("name").unique();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("posts").await?;
        schema.drop_if_exists("tags").await
    }
}

async fn db() -> Db {
    let db = Db::connect("sqlite::memory:").await.unwrap();
    let mut m = Migrator::new();
    m.add(CreateTables);
    m.migrate(&db).await.unwrap();
    db
}

fn new_post(title: &str) -> post::ActiveModel {
    post::ActiveModel {
        title: Set(title.to_owned()),
        ..Default::default()
    }
}

#[tokio::test]
async fn create_find_count_all() {
    let db = db().await;
    assert_eq!(Post::count(&db).await.unwrap(), 0);
    let first = Post::create(&db, new_post("First")).await.unwrap();
    let second = Post::create(&db, new_post("Second")).await.unwrap();
    assert_eq!((first.id, second.id), (1, 2));
    assert_eq!(first.views, 0, "the column default applies");
    assert_eq!(Post::count(&db).await.unwrap(), 2);
    assert_eq!(
        Post::all(&db)
            .await
            .unwrap()
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        ["First", "Second"]
    );
    assert_eq!(Post::find(&db, 2).await.unwrap(), Some(second.clone()));
    assert_eq!(Post::find(&db, 9).await.unwrap(), None);
    assert_eq!(Post::find_or_404(&db, 1).await.unwrap(), first);
    let missing = Post::find_or_404(&db, 9).await.unwrap_err();
    assert_eq!(missing.status(), 404);
}

#[tokio::test]
async fn timestamps_are_set_on_create_and_bumped_on_update() {
    let db = db().await;
    let before = ChronoUtc::now();
    let post = Post::create(&db, new_post("Hi")).await.unwrap();
    let created = post.created_at.expect("created_at is set");
    assert_eq!(post.created_at, post.updated_at);
    assert!(created >= before - chrono_slack());

    tokio::time::sleep(Duration::from_millis(5)).await;
    let updated = post
        .update(&db, |m| {
            m.title = Set("Hello".into());
            m.views = Set(7);
        })
        .await
        .unwrap();
    assert_eq!((updated.title.as_str(), updated.views), ("Hello", 7));
    assert_eq!(updated.created_at, post.created_at);
    assert!(updated.updated_at.unwrap() > created, "updated_at moves");
    assert_eq!(Post::find(&db, post.id).await.unwrap(), Some(updated));

    // A timestamp set by the caller is kept on create.
    let fixed = ChronoUtc::now() - chrono_slack() * 1000;
    let mut values = new_post("Backdated");
    values.created_at = Set(Some(fixed));
    let backdated = Post::create(&db, values).await.unwrap();
    assert_eq!(backdated.created_at, Some(fixed));
    assert_ne!(backdated.updated_at, Some(fixed));
}

fn chrono_slack() -> Duration {
    Duration::from_secs(1)
}

#[tokio::test]
async fn delete_and_query() {
    let db = db().await;
    for title in ["a", "b", "c"] {
        Post::create(&db, new_post(title)).await.unwrap();
    }
    let b = Post::find_or_404(&db, 2).await.unwrap();
    b.delete(&db).await.unwrap();
    assert_eq!(Post::count(&db).await.unwrap(), 2);
    assert_eq!(Post::find(&db, 2).await.unwrap(), None);

    let found = Post::query()
        .filter(post::Column::Title.eq("c"))
        .one(db.conn())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, 3);
    let ordered: Vec<i64> = Post::query()
        .order_by_desc(post::Column::Id)
        .all(db.conn())
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ordered, [3, 1]);
}

#[tokio::test]
async fn models_without_timestamps_work() {
    let db = db().await;
    let tag = Tag::create(
        &db,
        tag::ActiveModel {
            name: Set("rust".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let same = tag.update(&db, |_| {}).await.unwrap();
    assert_eq!(same, tag, "no change, no query");
    let renamed = tag
        .update(&db, |m| m.name = Set("tokio".into()))
        .await
        .unwrap();
    assert_eq!(renamed.name, "tokio");
    // A constraint violation is an error (500), not a panic.
    Tag::create(
        &db,
        tag::ActiveModel {
            name: Set("x".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let dup = Tag::create(
        &db,
        tag::ActiveModel {
            name: Set("x".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(dup.status(), 500);
}

struct PostFactory;

impl Factory for PostFactory {
    type Entity = post::Entity;

    fn definition(&self, fake: &mut Fake) -> post::ActiveModel {
        post::ActiveModel {
            title: Set(fake.sentence(4)),
            body: Set(Some(fake.paragraph())),
            views: Set(i32::try_from(fake.int(0..=100)).unwrap()),
            ..Default::default()
        }
    }
}

#[tokio::test]
async fn factories_make_and_create() {
    let db = db().await;
    let one = PostFactory.create(&db).await.unwrap();
    assert!(one.title.ends_with('.') && one.body.is_some());
    assert!(one.created_at.is_some());
    let many = PostFactory.count(3).create(&db).await.unwrap();
    assert_eq!(many.len(), 3);
    assert_eq!(Post::count(&db).await.unwrap(), 4);
    let titled = PostFactory
        .create_with(&db, |m| m.title = Set("Pinned".into()))
        .await
        .unwrap();
    assert_eq!(titled.title, "Pinned");

    // `make` builds values without saving them, deterministically per seed.
    let a = PostFactory.make(&mut Fake::seeded(3));
    let b = PostFactory.make(&mut Fake::seeded(3));
    assert_eq!(a.title, b.title);
    assert_eq!(PostFactory.count(2).make(&mut Fake::seeded(1)).len(), 2);
    assert_eq!(Post::count(&db).await.unwrap(), 5);
}
