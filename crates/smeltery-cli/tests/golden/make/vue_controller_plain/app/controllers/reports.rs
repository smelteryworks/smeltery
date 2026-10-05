//! The `reports` controller.

use smeltery::alloy::{self, Page};

/// `GET /reports`: the page `resources/js/pages/reports/Index.vue`.
pub async fn index() -> Page {
    alloy::render("reports/Index")
}
