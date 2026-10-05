//! The `reports` controller.

use smeltery::alloy::{self, Page};

/// `GET /reports`: the page `resources/js/pages/reports/index.tsx`.
pub async fn index() -> Page {
    alloy::render("reports/index")
}
