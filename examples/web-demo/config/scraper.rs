//! Settings of the `scraper` agent.

use std::time::Duration;

use smeltery::config::env;

/// What the scraper crawls and how politely.
#[derive(Debug, Clone)]
pub struct ScraperConfig {
    /// The first page (`SCRAPER_START_URL`). Empty: the scraper idles and says so in its log.
    pub start_url: String,
    /// The most pages one crawl stores (`SCRAPER_MAX_PAGES`).
    pub max_pages: usize,
    /// The pause between two requests to the same host (`SCRAPER_DELAY_MS`), enforced by the host's rate limit.
    pub delay: Duration,
    /// Retries of a failed request before the run fails and the agent restarts with backoff (`SCRAPER_RETRIES`).
    pub retries: u32,
}

/// Reads the scraper settings.
pub fn scraper() -> ScraperConfig {
    ScraperConfig {
        start_url: env("SCRAPER_START_URL", ""),
        max_pages: env("SCRAPER_MAX_PAGES", 50),
        delay: Duration::from_millis(env("SCRAPER_DELAY_MS", 2000)),
        retries: env("SCRAPER_RETRIES", 3),
    }
}
