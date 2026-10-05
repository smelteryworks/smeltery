//! HTTP tests: requests against the app in memory, no server needed. Every `TestApp` starts with a fresh, migrated
//! database and keeps its cookies between requests, so sessions carry over.

use smeltery::sparks::testing::TestSpark;
use smeltery::testing::TestApp;

#[test]
fn home_page_works() {
    let app = TestApp::new(my_app::build);
    let res = app.get("/");
    assert_eq!(res.status(), 200);
    assert!(res.text().contains("My App"));
}

#[test]
fn the_counter_spark_counts() {
    let app = TestApp::new(my_app::build);
    let html = app.get("/").text();
    assert!(html.contains("wire:name=\"counter\""));
    let mut counter =
        TestSpark::from_html(&html, "counter").expect("the home page shows the counter");
    assert_eq!(counter.data()["count"], 0);
    assert_eq!(
        counter
            .call("increment", smeltery::json!([]))
            .send(&app)
            .status(),
        200
    );
    assert_eq!(counter.data()["count"], 1);
    counter.set("step", 5);
    assert_eq!(
        counter
            .call("increment", smeltery::json!([]))
            .send(&app)
            .status(),
        200
    );
    assert_eq!(counter.data()["count"], 6);
}

#[test]
fn health_check_works() {
    let app = TestApp::new(my_app::build);
    let res = app.get("/api/health");
    assert_eq!(res.status(), 200);
    assert!(res.text().contains("ok"));
}
