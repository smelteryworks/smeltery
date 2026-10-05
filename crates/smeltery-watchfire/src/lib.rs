//! Watchfire, the agent runtime of the [Smeltery](https://github.com/smelteryworks/smeltery)
//! framework: supervised long-running agents, background jobs on a queue, and a scheduler, on
//! one Tokio runtime. Apps use it as `smeltery::watchfire`.
#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod agent;
mod alert;
mod app;
#[cfg(test)]
mod backend_tests;
mod config;
mod console;
mod coord;
#[cfg(test)]
mod coord_tests;
mod ctx;
mod error;
pub mod http;
#[cfg(feature = "llm")]
#[cfg_attr(docsrs, doc(cfg(feature = "llm")))]
pub mod llm;
#[cfg(feature = "mail")]
#[cfg_attr(docsrs, doc(cfg(feature = "mail")))]
pub mod mail;
pub mod migrations;
mod policy;
mod queue;
mod registry;
mod remote;
mod runtime;
mod schedule;
mod status;
mod store;
pub mod testing;
mod time;
pub mod web;

pub use agent::{Agent, Event, FnAgent, agent_fn};
pub use alert::{Alert, AlertKind};
pub use app::{AgentsExt, WatchfireSettings};
pub use config::{AgentConfig, MAX_NAME_LEN};
pub use ctx::{AgentCtx, AgentLog, Counter, Permit, Ticker};
pub use error::{AgentError, BoxError, Error, StoreError};
pub use http::Http;
pub use policy::{Backoff, Restart};
pub use queue::{DeadLetter, Job, JobCtx, JobId, Queue, QueueStats};
pub use registry::{AgentBuilder, GroupBuilder, Watchfire};
pub use runtime::Agents;
pub use schedule::{Cron, Overlap, ScheduleBuilder, ScheduleInfo, ScheduledTask};
pub use status::{AgentState, AgentStatus, Health, LogLine, RunOutcome, RunRecord};
pub use time::{DurationExt, Rate, RateExt, format_utc};
pub use tokio_util::sync::CancellationToken;

/// Everything an agents or jobs file needs: `use smeltery::watchfire::prelude::*;`.
pub mod prelude {
    pub use crate::agent::{Agent, Event, agent_fn};
    pub use crate::app::AgentsExt;
    pub use crate::config::AgentConfig;
    pub use crate::ctx::AgentCtx;
    pub use crate::error::AgentError;
    pub use crate::policy::Restart;
    pub use crate::queue::{Job, JobCtx};
    pub use crate::registry::Watchfire;
    pub use crate::runtime::Agents;
    pub use crate::schedule::Overlap;
    pub use crate::status::{AgentState, RunOutcome};
    pub use crate::time::{DurationExt, RateExt};
}
