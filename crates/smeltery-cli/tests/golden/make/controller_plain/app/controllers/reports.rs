//! The `reports` controller.

/// The page `resources/views/reports/index.mold.html`.
#[derive(smeltery::Mold)]
#[mold("reports/index")]
pub struct IndexView {}

/// Shows the index page.
pub async fn index() -> IndexView {
    IndexView {}
}
