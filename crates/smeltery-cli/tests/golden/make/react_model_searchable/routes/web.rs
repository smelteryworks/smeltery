//! Web routes: pages for the browser. They run with sessions and CSRF protection.

use crate::app::controllers::{dashboard, home, settings};

/// Registers the web routes.
pub fn routes(r: &mut smeltery::routing::Router) {
    r.get("/", home::index).name("home");

    // Login, registration, password reset, e-mail verification, password confirmation and two-factor routes come
    // from Temper (`app/providers/temper.rs`; `smeltery route:list` lists them).
    r.get("/dashboard", dashboard::index)
        .name("dashboard")
        .middleware("auth")
        .middleware("verified");
    // `password.confirm`: the address decides where password reset links go, so the profile form (and Temper's
    // `PUT /user/profile-information`) asks for the password again (once every `AUTH_PASSWORD_TIMEOUT` seconds).
    r.get("/settings/profile", settings::profile)
        .name("settings.profile")
        .middleware("auth")
        .middleware("verified")
        .middleware("password.confirm");
    r.get("/settings/password", settings::password)
        .name("settings.password")
        .middleware("auth")
        .middleware("verified");
    // `password.confirm`: the page shows the two-factor key while enrolling, so it asks for the password again
    // (once every `AUTH_PASSWORD_TIMEOUT` seconds).
    r.get("/settings/two-factor", settings::two_factor)
        .name("settings.two-factor")
        .middleware("auth")
        .middleware("verified")
        .middleware("password.confirm");
    // /posts: viewing and searching are public (searches are throttled); creating, editing
    // and deleting need a signed-in user.
    r.resource("/posts")
        .index(crate::app::controllers::posts::index)
        .middleware("throttle:60,1");
    r.resource("/posts")
        .show(crate::app::controllers::posts::show);
    r.resource("/posts")
        .create(crate::app::controllers::posts::create)
        .store(crate::app::controllers::posts::store)
        .edit(crate::app::controllers::posts::edit)
        .update(crate::app::controllers::posts::update)
        .destroy(crate::app::controllers::posts::destroy)
        .middleware("auth");
    // smeltery:routes
}
