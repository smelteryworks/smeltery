//! Error types.

use std::fmt;

/// A boxed, thread-safe error.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Errors from the Watchfire runtime: registration, commands and the queue.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An agent, group or schedule name failed validation.
    #[error("invalid name {name:?}: {reason}")]
    InvalidName {
        /// The rejected name.
        name: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// Two agents (or schedule entries, or jobs) share a name.
    #[error("{name:?} is registered twice")]
    Duplicate {
        /// The duplicated name.
        name: String,
    },
    /// No agent with this name is registered.
    #[error("unknown agent {name:?}")]
    UnknownAgent {
        /// The requested name.
        name: String,
    },
    /// `start` on an agent that is already starting or running.
    #[error("agent {name:?} is already running")]
    AlreadyRunning {
        /// The agent name.
        name: String,
    },
    /// `stop` or `pause` on an agent that is neither running nor waiting to restart.
    #[error("agent {name:?} is not running")]
    NotRunning {
        /// The agent name.
        name: String,
    },
    /// `start` on a paused agent (resume it instead).
    #[error("agent {name:?} is paused; resume it")]
    Paused {
        /// The agent name.
        name: String,
    },
    /// `resume` on an agent that is not paused.
    #[error("agent {name:?} is not paused")]
    NotPaused {
        /// The agent name.
        name: String,
    },
    /// The agent is a singleton that another process runs now ([`AgentState::Standby`](crate::AgentState));
    /// send the command to that process.
    #[error("agent {name:?} runs in another process (standby here)")]
    Standby {
        /// The agent name.
        name: String,
    },
    /// The runtime is shutting down (or never started) and takes no commands.
    #[error("Watchfire is shutting down")]
    ShuttingDown,
    /// Invalid configuration: a cron expression, a time of day, a missing setup.
    #[error("{0}")]
    Config(String),
    /// A store or queue operation failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<Error> for smeltery_core::Error {
    fn from(error: Error) -> Self {
        Self::other(error)
    }
}

/// A failed store or queue operation.
#[derive(Debug)]
pub struct StoreError {
    op: &'static str,
    source: BoxError,
}

impl StoreError {
    pub(crate) fn new(op: &'static str, source: impl Into<BoxError>) -> Self {
        Self {
            op,
            source: source.into(),
        }
    }

    /// The operation that failed, e.g. `"upsert_run"`.
    pub fn operation(&self) -> &'static str {
        self.op
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "store operation `{}` failed: {}", self.op, self.source)
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// The error an agent run, a job or a scheduled call returns.
///
/// Any `std::error::Error + Send + Sync + 'static` (including `smeltery::Error`) converts into it
/// with `?`. For a message (`&str`, `String`, anything `Display`) use [`AgentError::msg`].
///
/// It deliberately does not implement `std::error::Error` itself, so that the blanket `From`
/// conversion is possible (the trade-off `anyhow::Error` makes).
///
/// ```
/// use smeltery_watchfire::AgentError;
///
/// fn parse(text: &str) -> Result<u8, AgentError> {
///     Ok(text.parse::<u8>()?)
/// }
/// assert!(parse("300").is_err());
/// assert_eq!(AgentError::msg("upstream returned 503").to_string(), "upstream returned 503");
/// ```
pub struct AgentError(BoxError);

impl AgentError {
    /// An error carrying only a message.
    pub fn msg(message: impl fmt::Display) -> Self {
        Self(message.to_string().into())
    }

    /// Wrap an already boxed error.
    pub fn from_boxed(error: BoxError) -> Self {
        Self(error)
    }

    /// The wrapped error.
    pub fn into_inner(self) -> BoxError {
        self.0
    }
}

impl<E> From<E> for AgentError
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(error: E) -> Self {
        Self(Box::new(error))
    }
}

impl fmt::Display for AgentError {
    /// The error followed by its `source()` chain, joined with `": "`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(cause) = source {
            write!(f, ": {cause}")?;
            source = cause.source();
        }
        Ok(())
    }
}

impl fmt::Debug for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("outer")]
    struct Outer(#[source] std::io::Error);

    #[test]
    fn agent_error_display_includes_source_chain() {
        let err: AgentError = Outer(std::io::Error::other("inner")).into();
        assert_eq!(err.to_string(), "outer: inner");
        let err: AgentError = smeltery_core::Error::internal("db down").into();
        assert_eq!(err.to_string(), "db down");
    }

    #[test]
    fn store_error_names_operation() {
        let err = StoreError::new("load_agent", "disk on fire");
        assert_eq!(err.operation(), "load_agent");
        assert_eq!(
            err.to_string(),
            "store operation `load_agent` failed: disk on fire"
        );
    }
}
