//! Web routes: pages for the browser. They run with sessions and CSRF protection.

use crate::app::controllers::home;

/// Registers the web routes.
pub fn routes(r: &mut smeltery::routing::Router) {
    r.get("/", home::index).name("home");
    r.get("/reports", crate::app::controllers::reports::index)
        .name("reports.index");
    r.get("/about-us", crate::app::controllers::about_us::show)
        .name("about-us");
    // smeltery:routes
}
