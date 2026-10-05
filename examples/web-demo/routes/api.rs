//! API routes: JSON endpoints.

/// Registers the API routes.
pub fn routes(r: &mut smeltery::routing::Router) {
    r.get("/health", health).name("api.health");
    // smeltery:routes
}

/// Reports that the app is up.
pub async fn health() -> impl smeltery::http::IntoResponse {
    smeltery::http::Json(smeltery::json!({ "status": "ok" }))
}
