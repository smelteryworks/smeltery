//! Alloy, the bridge to Vue: the HTML of a first visit and the props every page gets.

use smeltery::alloy::{Props, SharedCtx};

/// The HTML of a first visit, `resources/views/app.mold.html`: it loads the Vite assets and embeds the page. Later
/// visits get the page as JSON.
#[derive(smeltery::Mold, Default)]
#[mold("app")]
pub struct Root {}

/// Props every page gets (`usePage().props` in the browser); a page prop with the same key wins. Never put secrets
/// here: everything is sent to the browser.
pub async fn shared(ctx: SharedCtx) -> smeltery::Result<Props> {
    let app = smeltery::json!({ "name": ctx.app().settings().name });
    Ok(Props::new().with("app", app))
}
