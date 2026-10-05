//! The home page.

use crate::config::app::AppConfig;

/// The welcome page, rendered from `resources/views/home.mold.html`.
#[derive(smeltery::Mold)]
#[mold("home")]
pub struct HomePage {
    /// The app name.
    pub name: String,
}

/// Shows the welcome page with the app name.
pub async fn index(app: smeltery::App) -> HomePage {
    let name = app
        .config::<AppConfig>()
        .map(|c| c.name.clone())
        .unwrap_or_default();
    HomePage { name }
}
