//! Models, migrations, seeders, factories and route model binding through the facade, the
//! way a generated app writes them.
#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod post {
    //! The `Post` model (table `posts`).
    use smeltery::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "posts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub title: String,
        pub body: String,
        pub created_at: Option<DateTimeUtc>,
        pub updated_at: Option<DateTimeUtc>,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

mod migrations {
    //! Database migrations, run in the order they are added below.
    pub mod m2026_10_03_120000_create_posts_table {
        //! Create the `posts` table.
        use smeltery::Result;
        use smeltery::db::migration::{Migration, Schema};

        /// Creates `posts`.
        pub struct CreatePostsTable;

        impl Migration for CreatePostsTable {
            fn name(&self) -> &'static str {
                "2026_10_03_120000_create_posts_table"
            }

            async fn up(&self, schema: &Schema) -> Result<()> {
                schema
                    .create("posts", |t| {
                        t.id();
                        t.string("title");
                        t.text("body");
                        t.timestamps();
                    })
                    .await
            }

            async fn down(&self, schema: &Schema) -> Result<()> {
                schema.drop_if_exists("posts").await
            }
        }
    }
    // smeltery:mods

    use smeltery::db::migration::Migrator;

    /// Register every migration, oldest first.
    pub fn register(m: &mut Migrator) {
        m.add(m2026_10_03_120000_create_posts_table::CreatePostsTable);
        // smeltery:migrations
    }
}

mod factories {
    use smeltery::db::factory::{Factory, Fake};
    use smeltery::db::prelude::*;

    pub struct PostFactory;

    impl Factory for PostFactory {
        type Entity = super::post::Entity;

        fn definition(&self, fake: &mut Fake) -> super::post::ActiveModel {
            super::post::ActiveModel {
                title: Set(fake.sentence(4)),
                body: Set(fake.paragraph()),
                ..Default::default()
            }
        }
    }
}

mod seeders {
    use smeltery::db::factory::Factory;
    use smeltery::db::seed::{Seeder, Seeders};

    pub struct DatabaseSeeder;

    impl Seeder for DatabaseSeeder {
        async fn run(&self, db: &smeltery::db::Db) -> smeltery::Result<()> {
            super::factories::PostFactory.count(2).create(db).await?;
            Ok(())
        }
    }

    pub fn register(s: &mut Seeders) {
        s.add(DatabaseSeeder);
        // smeltery:seeders
    }
}

use post::Model as Post;
use smeltery::db::prelude::sea_orm::Statement;
use smeltery::db::prelude::{ConnectionTrait, Set};
use smeltery::prelude::*;
use smeltery::testing::TestApp;

async fn index(db: Db) -> Result<Json<Vec<Post>>> {
    Ok(Json(Post::all(&db).await?))
}

async fn show(Found(post): Found<Post>) -> String {
    post.title
}

async fn store(
    db: Db,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> Result<String> {
    let post = Post::create(
        &db,
        post::ActiveModel {
            title: Set(form.get("title").cloned().unwrap_or_default()),
            body: Set(String::new()),
            ..Default::default()
        },
    )
    .await?;
    Ok(post.id.to_string())
}

fn build(app: AppBuilder) -> AppBuilder {
    app.migrations(migrations::register)
        .seeders(seeders::register)
        .routes(|r| {
            r.resource("/posts").index(index).show(show).store(store);
            r.get("/users/{user}/posts/{post}", show);
        })
}

#[test]
fn route_model_binding_answers_200_and_404() {
    let app = TestApp::new(build);
    let db = app.db();
    let post = app
        .block_on(Post::create(
            &db,
            post::ActiveModel {
                title: Set("Hello".into()),
                body: Set("Body".into()),
                ..Default::default()
            },
        ))
        .unwrap();
    assert!(post.created_at.is_some());

    let res = app.get(&format!("/posts/{}", post.id));
    assert_eq!((res.status(), res.text().as_str()), (200, "Hello"));
    assert_eq!(app.get("/posts/999").status(), 404);
    assert_eq!(app.get("/posts/not-a-number").status(), 404);
    // The last path parameter is the key.
    assert_eq!(app.get("/users/999/posts/1").status(), 200);
    assert_eq!(app.get("/users/1/posts/2").status(), 404);
}

#[test]
fn handlers_take_the_db_and_every_test_app_starts_empty() {
    let app = TestApp::new(build);
    assert_eq!(app.post_form("/posts", &[("title", "Form")]).text(), "1");
    let list = app.get_json("/posts").json();
    assert_eq!(list[0]["title"], "Form");
    assert_eq!(app.block_on(Post::count(&app.db())).unwrap(), 1);

    let other = TestApp::new(build);
    assert_eq!(other.block_on(Post::count(&other.db())).unwrap(), 0);
}

#[test]
fn seeders_and_factories_fill_the_test_database() {
    let app = TestApp::new(build);
    let db = app.db();
    let seeded = app.block_on(app.app().seeders().run(&db, None)).unwrap();
    assert_eq!(seeded, ["DatabaseSeeder"]);
    assert_eq!(app.block_on(Post::count(&db)).unwrap(), 2);
    let raw = app
        .block_on(db.conn().query_all_raw(Statement::from_string(
            db.conn().get_database_backend(),
            "SELECT title FROM posts",
        )))
        .unwrap();
    assert_eq!(raw.len(), 2);
}

#[test]
fn status_lists_the_ran_migrations() {
    let app = TestApp::new(build);
    let status = app
        .block_on(app.app().migrator().status(&app.db()))
        .unwrap();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].name, "2026_10_03_120000_create_posts_table");
    assert_eq!(status[0].batch, Some(1));
}
