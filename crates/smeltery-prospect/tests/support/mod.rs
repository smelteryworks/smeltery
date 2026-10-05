//! The test app shared by the search suites: `posts` (searched, filtered, `only_when`), `docs` (scoped by team),
//! their search indexes, and helpers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code
)]

use smeltery_core::db::migration::{Migration, Migrator, Schema};
use smeltery_core::db::prelude::*;
use smeltery_core::testing::TestApp;
use smeltery_core::{AppBuilder, Result};
use smeltery_prospect::migration::{Language, SearchIndex};
use smeltery_prospect::{IndexSpec, Prospect, ProspectExt as _, Searchable, Weight};

pub mod post {
    //! `Post`: title (A), body (B), filters `user_id` / `published`, only published rows.
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "posts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub title: String,
        pub body: Option<String>,
        pub user_id: i64,
        pub published: bool,
        pub created_at: Option<DateTimeUtc>,
        pub updated_at: Option<DateTimeUtc>,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod doc {
    //! `Doc`: scoped by `team_id`.
    use smeltery_core::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "docs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub title: String,
        pub team_id: i64,
        pub kind: String,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub use doc::Model as Doc;
pub use post::Model as Post;

impl Searchable for Post {
    fn index(i: &mut IndexSpec) {
        i.text("title").weight(Weight::A);
        i.text("body");
        i.filter("user_id");
        i.filter("created_at");
        i.sort("created_at");
        i.sort("id");
        i.only_when("published");
    }
}

impl Searchable for Doc {
    fn index(i: &mut IndexSpec) {
        i.text("title");
        i.filter("kind");
        i.scoped_by("team_id");
    }
}

pub struct CreateTables;

impl Migration for CreateTables {
    fn name(&self) -> &'static str {
        "2026_10_05_000001_create_search_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("posts", |t| {
                t.id();
                t.string("title");
                t.text("body").nullable();
                t.big_integer("user_id");
                t.boolean("published").default(true);
                t.timestamps();
            })
            .await?;
        SearchIndex::on("posts")
            .text("title", Weight::A)
            .text("body", Weight::B)
            .language(Language::Simple)
            .create(schema)
            .await?;
        schema
            .create("docs", |t| {
                t.id();
                t.string("title");
                t.big_integer("team_id");
                t.string("kind");
            })
            .await?;
        SearchIndex::on("docs")
            .text("title", Weight::B)
            .create(schema)
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        SearchIndex::on("docs").drop(schema).await?;
        schema.drop_if_exists("docs").await?;
        SearchIndex::on("posts").drop(schema).await?;
        schema.drop_if_exists("posts").await
    }
}

pub fn migrations(m: &mut Migrator) {
    m.add(CreateTables);
}

pub fn build(b: AppBuilder) -> AppBuilder {
    b.migrations(migrations).prospect(|p| {
        p.model::<Post>().model::<Doc>();
    })
}

pub fn app() -> TestApp {
    TestApp::new(build)
}

pub fn prospect(app: &TestApp) -> Prospect {
    Prospect::of(app.app()).unwrap()
}

pub fn post(app: &TestApp, title: &str, body: Option<&str>, user_id: i64) -> Post {
    let db = app.db();
    app.block_on(Post::create(
        &db,
        post::ActiveModel {
            title: Set(title.to_owned()),
            body: Set(body.map(str::to_owned)),
            user_id: Set(user_id),
            published: Set(true),
            ..Default::default()
        },
    ))
    .unwrap()
}

pub fn doc(app: &TestApp, title: &str, team_id: i64, kind: &str) -> Doc {
    let db = app.db();
    app.block_on(Doc::create(
        &db,
        doc::ActiveModel {
            title: Set(title.to_owned()),
            team_id: Set(team_id),
            kind: Set(kind.to_owned()),
            ..Default::default()
        },
    ))
    .unwrap()
}

/// The titles of the hits of `q` (first page of 50).
pub fn titles(app: &TestApp, q: &str) -> Vec<String> {
    let prospect = prospect(app);
    app.block_on(Post::search(&prospect, q).get(50))
        .unwrap()
        .into_iter()
        .map(|h| h.model.title)
        .collect()
}
