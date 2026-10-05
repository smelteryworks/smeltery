//! The `about_us` page.

use smeltery::alloy::{self, Page};

/// `GET /about-us`: the page `resources/js/pages/AboutUs.vue`.
pub async fn show() -> Page {
    alloy::render("AboutUs")
}
