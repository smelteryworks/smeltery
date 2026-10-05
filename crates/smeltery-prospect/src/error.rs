//! Prospect's errors. Every one is a 500 when it reaches a handler (a mistake in the app's code or setup, never a
//! user's fault); the message names the model's table and the column.

/// What went wrong in a search, a registration or a migration helper.
///
/// The search calls return [`smeltery_core::Result`]; a Prospect error inside it can be read back with
/// [`ProspectError::of`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProspectError {
    /// The model is scoped (`scoped_by`) and the search named neither `within(value)` nor `across_scopes()`.
    #[error(
        "`{table}` is searched without a scope: call `.within(…)` or `.across_scopes()` (it is scoped by `{column}`)"
    )]
    ScopeMissing {
        /// The model's table.
        table: &'static str,
        /// The scope column.
        column: String,
    },
    /// A column that is not declared for this use in the model's `IndexSpec`.
    #[error("`{column}` is not a declared {what} column of `{table}` (see its `impl Searchable`)")]
    Undeclared {
        /// The model's table.
        table: &'static str,
        /// The column named.
        column: String,
        /// `filter`, `sort` or `text`.
        what: &'static str,
    },
    /// A filter value of the wrong type for its column.
    #[error("the filter value for `{column}` of `{table}` must be {expected}")]
    FilterType {
        /// The model's table.
        table: &'static str,
        /// The column.
        column: String,
        /// The type the column takes.
        expected: &'static str,
    },
    /// A string filter value an engine filter cannot carry: engines take `[A-Za-z0-9_.:@-]{1,128}` only.
    #[error(
        "a filter string for `{column}` of `{table}` may hold only letters, digits and `_ . : @ -` (1 to 128 characters)"
    )]
    FilterValue {
        /// The model's table.
        table: &'static str,
        /// The column.
        column: String,
    },
    /// A bound was passed (`where_in` values, highlight columns).
    #[error("{0}")]
    Limit(String),
    /// The call needs another driver (`query(…)` works only on the database driver).
    #[error("{0}")]
    Unsupported(String),
    /// The model is not registered with `.prospect(|p| p.model::<M>())`.
    #[error(
        "this model is not registered with Prospect: add `p.model::<…>()` to `.prospect(…)` in bootstrap/app.rs"
    )]
    NotRegistered,
    /// A model's `IndexSpec` does not fit its entity (checked when the app builds).
    #[error("the search spec of `{table}` is invalid: {reason}")]
    Spec {
        /// The model's table.
        table: &'static str,
        /// What is wrong.
        reason: String,
    },
    /// The database has no search index for the model, or one over other columns.
    #[error(
        "the search index of `{table}` is missing or does not match its spec ({reason}): write a migration with \
         `SearchIndex::on(\"{table}\")…` and run `migrate`"
    )]
    IndexMissing {
        /// The model's table.
        table: &'static str,
        /// What the check found.
        reason: String,
    },
    /// A `SearchIndex` name or option is invalid.
    #[error("{0}")]
    Migration(String),
    /// An invalid `PROSPECT_*` setting.
    #[error("{0}")]
    Settings(String),
}

impl ProspectError {
    /// The Prospect error inside a [`smeltery_core::Error`], if it is one.
    pub fn of(error: &smeltery_core::Error) -> Option<&ProspectError> {
        match error {
            smeltery_core::Error::Other(inner) => inner.downcast_ref::<ProspectError>(),
            _ => None,
        }
    }
}

impl From<ProspectError> for smeltery_core::Error {
    fn from(error: ProspectError) -> Self {
        smeltery_core::Error::other(error)
    }
}
