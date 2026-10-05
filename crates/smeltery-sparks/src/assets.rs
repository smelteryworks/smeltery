//! The client runtime: `GET /_sparks/sparks.js` and the `@sparksScripts` tags.

use axum::response::{IntoResponse, Response};
use http::header;
use smeltery_core::html::escape;

/// The client runtime, embedded in the crate.
pub const SPARKS_JS: &str = include_str!("../js/sparks.js");

/// `GET /_sparks/sparks.js`: the runtime, cached for a year (the URL carries the crate version).
pub(crate) async fn script() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        SPARKS_JS,
    )
        .into_response()
}

/// `@sparksScripts`: the CSRF meta tag (when the page has a session) and the runtime's script tag.
pub(crate) fn scripts_tag(csrf: Option<&str>) -> String {
    let meta = csrf
        .map(|t| format!("<meta name=\"csrf-token\" content=\"{}\">", escape(t)))
        .unwrap_or_default();
    format!(
        "{meta}<script src=\"/_sparks/sparks.js?v={}\" defer></script>",
        crate::VERSION
    )
}
