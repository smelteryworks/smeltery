# smeltery-prospect

Prospect, the full-text search of the [Smeltery](https://github.com/smelteryworks/smeltery) framework: a model declares
which columns are searched (with weights), filtered and sorted; a search returns a page of hits that hold real
models, ranked, with highlights. The `database` driver uses the database's own full-text search (SQLite FTS5,
PostgreSQL `tsvector` + GIN, MySQL / MariaDB `FULLTEXT`), which the database keeps current itself; no extra server.

Apps use it through the facade, as `smeltery::prospect`:

```rust,no_run
# mod post {
use smeltery::db::prelude::*;
use smeltery::prospect::{IndexSpec, Searchable, Weight};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "posts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub title: String,
    pub body: String,
    pub team_id: i64,
    pub published: bool,
}

impl ActiveModelBehavior for ActiveModel {}

impl Searchable for Model {
    fn index(i: &mut IndexSpec) {
        i.text("title").weight(Weight::A);
        i.text("body");
        i.only_when("published");
        i.scoped_by("team_id");
    }
}
# }
# use post::Model as Post;
use smeltery::db::{Page, PageQuery};
use smeltery::prelude::*;
use smeltery::prospect::{Hit, Prospect, ProspectExt as _};

#[derive(serde::Deserialize)]
struct Search {
    #[serde(default)]
    q: String,
}

async fn index(prospect: Prospect, Query(s): Query<Search>, page: PageQuery) -> Result<Json<Page<Hit<Post>>>> {
    let team = 1; // the signed-in user's team
    Ok(Json(
        Post::search(&prospect, &s.q)
            .within(team)
            .highlight(["title", "body"])
            .paginate(page)
            .await?,
    ))
}

fn build(app: AppBuilder) -> AppBuilder {
    app.prospect(|p| {
        p.model::<Post>();
    })
    .routes(|r| {
        r.get("/posts", index).middleware("throttle:60,1");
    })
}
# fn main() { let _ = build; }
```

The index comes from a migration (`smeltery::prospect::migration::SearchIndex`), and user text is reduced to
letters-and-digits terms before it reaches any search grammar. The full guide, with the driver differences, is the
"Search" section of the [Smeltery README](https://github.com/smelteryworks/smeltery#search).

## Licence

MIT OR Apache-2.0.
