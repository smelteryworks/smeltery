//! Temper's own errors: set-up mistakes found while the app builds.

/// A Temper set-up mistake. `.temper(…)` turns it into a build error, so the app stops at boot with this message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TemperError {
    /// A page route is on but its view is missing.
    #[error(
        "Temper: the `{page}` page has no view: call `TemperViews::{page}(…)` in `.views(…)`, leave the route out \
         with `Temper::without_route(\"{route}\")`, or answer JSON only with `.views(false)`"
    )]
    MissingView {
        /// The page (and the `TemperViews` method).
        page: &'static str,
        /// The route name of the page.
        route: &'static str,
    },
    /// `Temper::without_route` names no Temper route.
    #[error("Temper: `without_route(\"{name}\")` names no Temper route; the names are: {known}")]
    UnknownRoute {
        /// The name given.
        name: String,
        /// Every route name Temper has.
        known: String,
    },
    /// Invalid two-factor options.
    #[error("Temper: invalid two-factor option {0}")]
    InvalidTwoFactor(String),
    /// An invalid `Temper::prefix`.
    #[error(
        "Temper: the prefix `{0}` is invalid: write a path such as `/auth` (a leading `/`, no trailing `/`, no `{{`)"
    )]
    InvalidPrefix(String),
}

impl From<TemperError> for smeltery_core::Error {
    fn from(error: TemperError) -> Self {
        Self::internal(error.to_string())
    }
}
