//! The `/posts` list searches (`smeltery make:model Post … --searchable`): every record without a search,
//! the matching ones with their `title` highlighted. The records come from `PostFactory`.

use smeltery::alloy::testing::AlloyRequests as _;
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
    let page = app_with_posts().get_alloy("/posts").json();
    let items = page["props"]["posts"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let titles: Vec<&str> = items.iter().filter_map(|i| i["title"].as_str()).collect();
    assert_eq!(items.len(), 2, "{page}");
    assert!(
        titles.contains(&"Anvil care") && titles.contains(&"Forge notes"),
        "{page}"
    );
}

#[test]
fn the_posts_list_finds_and_highlights_matches() {
    let page = app_with_posts().get_alloy("/posts?q=anvil").json();
    let items = page["props"]["posts"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(items.len(), 1, "{page}");
    assert_eq!(
        items[0]["_highlights"]["title"],
        smeltery::json!([{ "text": "Anvil", "matched": true }, { "text": " care", "matched": false }])
    );
    assert_eq!(page["props"]["q"], "anvil");
}
