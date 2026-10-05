//! Application settings.

use smeltery::config::env;

/// The application settings.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Display name (`APP_NAME`).
    pub name: String,
    /// Environment: `local`, `production`, ... (`APP_ENV`).
    pub env: String,
    /// Debug mode (`APP_DEBUG`).
    pub debug: bool,
    /// Public base URL (`APP_URL`).
    pub url: String,
}

/// Reads the application settings.
pub fn app() -> AppConfig {
    AppConfig {
        name: env("APP_NAME", "Headless Demo"),
        env: env("APP_ENV", "local"),
        debug: env("APP_DEBUG", true),
        url: env("APP_URL", "http://127.0.0.1:8000"),
    }
}
