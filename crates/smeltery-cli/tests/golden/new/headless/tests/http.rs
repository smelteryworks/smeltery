//! HTTP tests: requests against the app in memory, no server needed. Every `TestApp` starts with a fresh, migrated
//! database.

use smeltery::testing::TestApp;

#[test]
fn app_builds_without_routes() {
    let app = TestApp::new(my_app::build);
    assert_eq!(app.get("/").status(), 404);
}
