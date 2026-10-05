//! Web routes: pages for the browser. They run with sessions and CSRF protection.

use crate::app::controllers::auth::{login, password, register};
use crate::app::controllers::{dashboard, home};

/// Registers the web routes.
pub fn routes(r: &mut smeltery::routing::Router) {
    r.get("/", home::index).name("home");

    // Authentication: `guest` sends logged-in users to the dashboard, `auth` sends guests to the login page.
    r.get("/register", register::create)
        .name("register")
        .middleware("guest");
    r.post("/register", register::store).middleware("guest");
    r.get("/login", login::create)
        .name("login")
        .middleware("guest");
    r.post("/login", login::store)
        .middleware("throttle:30,1")
        .middleware("guest");
    r.post("/logout", login::destroy)
        .name("logout")
        .middleware("auth");
    r.get("/forgot-password", password::request)
        .name("password.request")
        .middleware("guest");
    r.post("/forgot-password", password::email)
        .name("password.email")
        .middleware("guest");
    r.get("/reset-password/{token}", password::edit)
        .name("password.reset")
        .middleware("guest");
    r.post("/reset-password/{token}", password::update)
        .name("password.update")
        .middleware("guest");

    r.get("/dashboard", dashboard::index)
        .name("dashboard")
        .middleware("auth");
    r.resource("/posts")
        .index(crate::app::controllers::posts::index)
        .create(crate::app::controllers::posts::create)
        .store(crate::app::controllers::posts::store)
        .show(crate::app::controllers::posts::show)
        .edit(crate::app::controllers::posts::edit)
        .update(crate::app::controllers::posts::update)
        .destroy(crate::app::controllers::posts::destroy)
        .middleware("auth");
    // smeltery:routes
}
