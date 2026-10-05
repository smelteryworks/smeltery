//! Sparks through the facade: `#[derive(Spark)]` and `#[actions]` with their default paths.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde::{Deserialize, Serialize};
use smeltery::prelude::*;
use smeltery::sparks::testing::TestSpark;
use smeltery::testing::TestApp;

#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(dir = "tests/app/resources/views")]
pub struct Clicker {
    pub count: i64,
}

#[actions]
impl Clicker {
    pub async fn increment(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        self.count += 1;
        ctx.dispatch("clicked", smeltery::json!({ "count": self.count }));
        Ok(())
    }
}

#[derive(Mold)]
#[mold("clicker_page", dir = "tests/app/resources/views")]
struct ClickerPage {}

async fn page() -> ClickerPage {
    ClickerPage {}
}

fn register(s: &mut Sparks) {
    s.add::<Clicker>();
}

#[test]
fn a_spark_through_the_facade() {
    let app = TestApp::new(|mut b| {
        b.settings_mut().root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        b.sparks(register).routes(|r| {
            r.get("/", page);
        })
    });
    let html = app.get("/").text();
    assert!(html.contains("/_sparks/sparks.js?v="), "{html}");
    let mut clicker = TestSpark::from_html(&html, "clicker").unwrap();
    assert!(clicker.html().contains("Clicked 2 times"));
    assert_eq!(
        clicker
            .call("increment", serde_json::json!([]))
            .send(&app)
            .status(),
        200
    );
    assert!(
        clicker.html().contains("Clicked 3 times"),
        "{}",
        clicker.html()
    );
    assert_eq!(clicker.effects()["dispatches"][0]["event"], "clicked");
}
