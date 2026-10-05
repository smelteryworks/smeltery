//! HTTP tests: requests against the app in memory, no server and no Node.js needed. Every `TestApp` starts with a
//! fresh, migrated database and keeps its cookies between requests, so sessions carry over.
//! `get_alloy` visits a page the way Inertia's client does and gets the page object as JSON.

use smeltery::alloy::testing::{AlloyAssertions as _, AlloyRequests as _, assert_page_file_exists};
use smeltery::testing::TestApp;

#[test]
fn home_page_works() {
    let app = TestApp::new(my_app::build);
    let res = app.get("/");
    assert_eq!(res.status(), 200);
    // The first visit is HTML from resources/views/app.mold.html with the page object inside.
    assert!(res.text().contains("<title>My App</title>"));
    res.assert_component("welcome")
        .assert_prop("app.name", "My App")
        .assert_prop("version", smeltery::VERSION);
}

#[test]
fn every_page_has_its_file() {
    // Component names are strings the browser resolves to resources/js/pages/<name>.tsx.
    assert_page_file_exists("welcome");
}

#[test]
fn the_forge_answers_a_partial_reload() {
    let app = TestApp::new(my_app::build);
    // `forge` is optional: a visit never computes it.
    app.get_alloy("/").assert_missing("forge");
    let res = app.reload_alloy("/", "welcome", &["forge"]);
    assert_eq!(res.status(), 200);
    let temperature = res
        .prop("forge.temperature")
        .and_then(|t| t.as_u64())
        .expect("a reading");
    assert!((1_150..1_500).contains(&temperature), "{temperature}");
    // Only the prop that was asked for comes back.
    res.assert_missing("version");
}

#[test]
fn health_check_works() {
    let app = TestApp::new(my_app::build);
    let res = app.get("/api/health");
    assert_eq!(res.status(), 200);
    assert!(res.text().contains("ok"));
}
