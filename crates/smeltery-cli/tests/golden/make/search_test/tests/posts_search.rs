//! The `/posts` list searches (`smeltery make:model Post … --searchable`): every record without a search,
//! the matching ones with their `title` highlighted. The records come from `PostFactory`.

use smeltery::db::factory::Factory as _;
use smeltery::db::prelude::*;
use smeltery::testing::TestApp;

use blog::database::factories::post_factory::PostFactory;

/// An app with two records, `Anvil care` and `Forge notes`.
fn app_with_posts() -> TestApp {
    let app = TestApp::new(blog::build);
    let db = app.db();
    for title in ["Anvil care", "Forge notes"] {
        app.block_on(PostFactory.create_with(&db, |m| m.title = Set(title.to_owned())))
            .expect("creating a record");
    }
    app
}

#[test]
fn the_posts_list_shows_every_record_without_a_search() {
    let html = app_with_posts().get("/posts").text();
    assert!(html.contains(">Anvil care</a>"), "{html}");
    assert!(html.contains(">Forge notes</a>"), "{html}");
    assert!(!html.contains("@else"), "{html}");
}

#[test]
fn the_posts_list_finds_and_highlights_matches() {
    let html = app_with_posts().get("/posts?q=anvil").text();
    assert!(html.contains("><mark>Anvil</mark> care</a>"), "{html}");
    assert!(!html.contains("Forge notes"), "{html}");
    assert!(!html.contains("@else"), "{html}");
}
