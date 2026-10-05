//! API routes: JSON endpoints.

/// Registers the API routes.
pub fn routes(r: &mut smeltery::routing::Router) {
    r.get("/health", health).name("api.health");
    // API tokens (Hallmark): a mobile app, a desktop app or another client gets a token for an e-mail address and a
    // password, sends it as `Authorization: Bearer …` on `auth:hallmark` routes, and signs it out.
    r.post("/tokens", crate::app::controllers::api::tokens::store)
        .name("api.tokens.store")
        .middleware("throttle:10,1");
    r.delete(
        "/tokens/current",
        crate::app::controllers::api::tokens::destroy,
    )
    .name("api.tokens.destroy")
    .middleware("auth:hallmark");
    // `verified`: with `.verify_email` on, a token of an unverified address gets 403 here (it may still sign out).
    r.get("/user", crate::app::controllers::api::user::show)
        .name("api.user")
        .middleware("auth:hallmark")
        .middleware("verified");
    // smeltery:routes
}

/// Reports that the app is up.
pub async fn health() -> impl smeltery::http::IntoResponse {
    smeltery::http::Json(smeltery::json!({ "status": "ok" }))
}
