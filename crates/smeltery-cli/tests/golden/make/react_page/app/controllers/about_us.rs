//! The `about_us` page.

use smeltery::alloy::{self, Page};

/// `GET /about-us`: the page `resources/js/pages/about-us.tsx`.
pub async fn show() -> Page {
    alloy::render("about-us")
}
