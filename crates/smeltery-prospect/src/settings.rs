//! Prospect's settings (`PROSPECT_*` in `.env`).

use smeltery_core::config::env;

use crate::error::ProspectError;

/// Which engine answers searches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Driver {
    /// The database's own full-text search (SQLite FTS5, PostgreSQL `tsvector`, MySQL `FULLTEXT`), kept current by
    /// the database itself.
    Database,
    /// An in-process engine for tests: documents are written by model events and kept in memory.
    Memory,
}

impl Driver {
    fn parse(value: &str) -> Result<Self, ProspectError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "database" => Ok(Self::Database),
            "memory" => Ok(Self::Memory),
            other => Err(ProspectError::Settings(format!(
                "PROSPECT_DRIVER `{other}` is not a driver: use `database` or `memory`"
            ))),
        }
    }

    /// The name used in `.env`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::Memory => "memory",
        }
    }
}

/// Prospect's settings: read from `.env` by [`ProspectSettings::from_env`], changed with the builder methods.
///
/// | `.env` | Default | Meaning |
/// |---|---|---|
/// | `PROSPECT_DRIVER` | `database` | `database` or `memory`; anything else stops the build |
/// | `PROSPECT_MAX_QUERY_LENGTH` | `200` | characters of search text used (1 to 1,000) |
/// | `PROSPECT_MAX_PER_PAGE` | `100` | the largest page or `get` limit (1 to 1,000) |
/// | `PROSPECT_BATCH` | `500` | rows per chunk when `prospect:import` fills the memory engine (1 to 10,000) |
///
/// Values out of range are clamped, with a warning naming the key.
#[derive(Clone)]
#[non_exhaustive]
pub struct ProspectSettings {
    driver: Result<Driver, String>,
    max_query_length: usize,
    max_per_page: u64,
    batch: u64,
}

impl std::fmt::Debug for ProspectSettings {
    // Hand-written like every settings type (SECURITY.md §3.5).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProspectSettings")
            .field("driver", &self.driver)
            .field("max_query_length", &self.max_query_length)
            .field("max_per_page", &self.max_per_page)
            .field("batch", &self.batch)
            .finish()
    }
}

impl Default for ProspectSettings {
    fn default() -> Self {
        Self {
            driver: Ok(Driver::Database),
            max_query_length: 200,
            max_per_page: 100,
            batch: 500,
        }
    }
}

impl ProspectSettings {
    /// The settings from `.env` (and the process environment).
    pub fn from_env() -> Self {
        let driver =
            Driver::parse(&env::<String>("PROSPECT_DRIVER", "")).map_err(|e| e.to_string());
        Self {
            driver,
            max_query_length: usize::try_from(clamp(
                "PROSPECT_MAX_QUERY_LENGTH",
                env::<u64>("PROSPECT_MAX_QUERY_LENGTH", 200),
                1,
                1_000,
            ))
            .unwrap_or(200),
            max_per_page: clamp(
                "PROSPECT_MAX_PER_PAGE",
                env::<u64>("PROSPECT_MAX_PER_PAGE", 100),
                1,
                1_000,
            ),
            batch: clamp(
                "PROSPECT_BATCH",
                env::<u64>("PROSPECT_BATCH", 500),
                1,
                10_000,
            ),
        }
    }

    /// Use `driver`.
    #[must_use]
    pub fn driver(mut self, driver: Driver) -> Self {
        self.driver = Ok(driver);
        self
    }

    /// Use at most `chars` characters of search text (clamped to 1..=1,000).
    #[must_use]
    pub fn max_query_length(mut self, chars: usize) -> Self {
        self.max_query_length = chars.clamp(1, 1_000);
        self
    }

    /// Allow pages (and `get` limits) of at most `n` (clamped to 1..=1,000).
    #[must_use]
    pub fn max_per_page(mut self, n: u64) -> Self {
        self.max_per_page = n.clamp(1, 1_000);
        self
    }

    /// The driver, or the reason `PROSPECT_DRIVER` was refused.
    pub(crate) fn checked_driver(&self) -> Result<Driver, ProspectError> {
        self.driver.clone().map_err(ProspectError::Settings)
    }

    /// The characters of search text used.
    pub fn query_length(&self) -> usize {
        self.max_query_length
    }

    /// The largest page.
    pub fn per_page_limit(&self) -> u64 {
        self.max_per_page
    }

    /// Rows per import chunk.
    pub fn batch(&self) -> u64 {
        self.batch
    }
}

fn clamp(key: &str, value: u64, min: u64, max: u64) -> u64 {
    let out = value.clamp(min, max);
    if out != value {
        tracing::warn!(
            key,
            value,
            used = out,
            "prospect: setting out of range, clamped"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drivers_parse_and_unknown_ones_are_refused() {
        assert_eq!(Driver::parse("").unwrap(), Driver::Database);
        assert_eq!(Driver::parse(" Database ").unwrap(), Driver::Database);
        assert_eq!(Driver::parse("memory").unwrap(), Driver::Memory);
        assert!(Driver::parse("elastic").is_err());
        assert_eq!(clamp("K", 0, 1, 10), 1);
        assert_eq!(clamp("K", 99, 1, 10), 10);
        let s = ProspectSettings::default()
            .max_per_page(0)
            .max_query_length(5000);
        assert_eq!((s.per_page_limit(), s.query_length()), (1, 1000));
        assert!(format!("{s:?}").contains("ProspectSettings"));
    }
}
