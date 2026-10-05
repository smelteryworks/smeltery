//! The [`Agent`] trait and the closure agents behind the quick forms.

use std::future::Future;
use std::time::Duration;

use serde::Serialize;
use smeltery_core::BoxFuture;
use tokio::sync::broadcast::error::RecvError;

use crate::config::AgentConfig;
use crate::ctx::AgentCtx;
use crate::error::AgentError;

/// A long-lived, supervised task: a poller, a scraper, a bot, a monitor.
///
/// `run` is called once per run. It should loop until the [`AgentCtx`] is cancelled and then
/// return promptly; returning on its own (with `Ok` or an error) ends the run, and the restart
/// policy decides whether a new run starts. The same value is reused across runs, so fields
/// survive restarts (and panics).
///
/// ```
/// use smeltery_watchfire::prelude::*;
///
/// /// Polls prices.
/// #[derive(Default)]
/// pub struct PricePoller {
///     last: Option<String>,
/// }
///
/// impl Agent for PricePoller {
///     fn name(&self) -> String {
///         "price_poller".into()
///     }
///
///     fn config(&self) -> AgentConfig {
///         AgentConfig::default()
///             .restart(Restart::OnFailure)
///             .backoff(1.secs()..=60.secs())
///             .heartbeat_timeout(2.mins())
///     }
///
///     async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
///         let mut ticker = ctx.interval(30.secs());
///         while ticker.tick().await {
///             self.last = Some("42".into());
///         }
///         Ok(())
///     }
/// }
/// ```
pub trait Agent: Send + 'static {
    /// The agent's name: 1 to 64 characters out of `a-z 0-9 _ -`.
    fn name(&self) -> String;

    /// How it is supervised. Settings on the registration handle override these.
    fn config(&self) -> AgentConfig {
        AgentConfig::default()
    }

    /// One run. Return when `ctx` is cancelled.
    fn run(&mut self, ctx: AgentCtx) -> impl Future<Output = Result<(), AgentError>> + Send;
}

/// Object-safe twin of [`Agent`], so the runtime can hold different agent types.
pub(crate) trait DynAgent: Send {
    fn run_boxed(&mut self, ctx: AgentCtx) -> BoxFuture<'_, Result<(), AgentError>>;
}

impl<A: Agent> DynAgent for A {
    fn run_boxed(&mut self, ctx: AgentCtx) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(self.run(ctx))
    }
}

/// An [`Agent`] made from a closure called once per run: what `Watchfire::run` registers, and
/// what [`agent_fn`] builds (for `spawn_child` and the test `Harness`).
pub struct FnAgent<F> {
    name: String,
    f: F,
}

impl<F> std::fmt::Debug for FnAgent<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FnAgent").field("name", &self.name).finish()
    }
}

/// An [`Agent`] named `name` whose every run calls `f`.
///
/// ```
/// use smeltery_watchfire::prelude::*;
///
/// let agent = agent_fn("waiter", |ctx: AgentCtx| async move {
///     ctx.cancelled().await;
///     Ok(())
/// });
/// assert_eq!(agent.name(), "waiter");
/// ```
pub fn agent_fn<F, Fut>(name: impl Into<String>, f: F) -> FnAgent<F>
where
    F: FnMut(AgentCtx) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), AgentError>> + Send + 'static,
{
    FnAgent {
        name: name.into(),
        f,
    }
}

impl<F, Fut> Agent for FnAgent<F>
where
    F: FnMut(AgentCtx) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), AgentError>> + Send + 'static,
{
    fn name(&self) -> String {
        self.name.clone()
    }

    fn run(&mut self, ctx: AgentCtx) -> impl Future<Output = Result<(), AgentError>> + Send {
        (self.f)(ctx)
    }
}

/// `Watchfire::every`: calls the closure on every tick (the first right away); an error ends
/// the run, like any failure.
pub(crate) struct EveryAgent<F> {
    pub(crate) name: String,
    pub(crate) period: Duration,
    pub(crate) f: F,
}

impl<F, Fut> Agent for EveryAgent<F>
where
    F: FnMut(AgentCtx) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), AgentError>> + Send + 'static,
{
    fn name(&self) -> String {
        self.name.clone()
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        let mut ticker = ctx.interval(self.period);
        while ticker.tick().await {
            // Cancellation also ends a call in progress, so stop / shutdown stay prompt.
            tokio::select! {
                biased;
                () = ctx.cancelled() => break,
                result = (self.f)(ctx.clone()) => result?,
            }
        }
        Ok(())
    }
}

/// An event from [`AgentCtx::emit`], delivered to `Watchfire::on_event` agents.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Event {
    /// The event name, e.g. `post.created`.
    pub name: String,
    /// The agent that emitted it.
    pub source: String,
    /// The payload as JSON.
    pub payload: serde_json::Value,
}

impl Event {
    /// The payload as `T`.
    ///
    /// # Errors
    /// The payload does not deserialize into `T`.
    pub fn payload_as<T: serde::de::DeserializeOwned>(&self) -> Result<T, AgentError> {
        Ok(serde_json::from_value(self.payload.clone())?)
    }
}

/// `Watchfire::on_event`: calls the closure for every event with the name, while running.
pub(crate) struct EventAgent<F> {
    pub(crate) name: String,
    pub(crate) event: String,
    pub(crate) f: F,
}

impl<F, Fut> Agent for EventAgent<F>
where
    F: FnMut(AgentCtx, Event) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), AgentError>> + Send + 'static,
{
    fn name(&self) -> String {
        self.name.clone()
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        let mut events = ctx.subscribe_events();
        loop {
            let received = tokio::select! {
                biased;
                () = ctx.cancelled() => return Ok(()),
                received = events.recv() => received,
            };
            ctx.heartbeat();
            match received {
                Ok(event) if event.name == self.event => {
                    tokio::select! {
                        biased;
                        () = ctx.cancelled() => return Ok(()),
                        result = (self.f)(ctx.clone(), event) => result?,
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "event listener fell behind; events were skipped");
                }
                Err(RecvError::Closed) => return Ok(()),
            }
        }
    }
}
