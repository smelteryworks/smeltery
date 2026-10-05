//! Registration: [`Watchfire`] and its builder handles.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::ops::RangeInclusive;
use std::time::Duration;

use crate::agent::{Agent, DynAgent, Event, EventAgent, EveryAgent, FnAgent, agent_fn};
use crate::config::{AgentConfig, validate_name};
use crate::ctx::AgentCtx;
use crate::error::{AgentError, Error};
use crate::policy::Restart;
use crate::queue::{Job, JobRegistry};
use crate::schedule::{self, ScheduleBuilder};
use crate::time::Rate;

/// One registered agent.
pub(crate) struct Registration {
    pub(crate) name: String,
    pub(crate) config: AgentConfig,
    pub(crate) agent: Box<dyn DynAgent>,
}

/// Names Watchfire uses for its own agents.
const RESERVED: [&str; 2] = ["scheduler", "queue"];

/// Everything an app runs in the background: agents, jobs, the schedule, groups and rate
/// limits. `app/agents/mod.rs`'s `register` function fills it; `AppBuilder::agents` (from
/// [`AgentsExt`](crate::AgentsExt)) runs it.
///
/// ```
/// use smeltery_watchfire::prelude::*;
///
/// pub fn register(w: &mut Watchfire) {
///     w.every(30.secs(), "heartbeat", |ctx| async move {
///         ctx.log().info("tick");
///         Ok(())
///     });
///     w.run("scraper", |ctx| async move {
///         ctx.cancelled().await;
///         Ok(())
///     })
///     .restart(Restart::OnFailure)
///     .backoff(1.secs()..=30.secs())
///     .group("scrapers");
///     w.group("scrapers").limit(2);
///     w.rate_limit("example.com", 2.per_second());
///     w.schedule()
///         .call("cleanup", |_ctx| async move { Ok(()) })
///         .every(5.mins())
///         .overlap(Overlap::Skip);
/// }
/// # let mut w = Watchfire::new();
/// # register(&mut w);
/// # assert_eq!(w.agent_names(), ["heartbeat", "scraper"]);
/// ```
#[derive(Default)]
pub struct Watchfire {
    pub(crate) agents: Vec<Registration>,
    pub(crate) groups: BTreeMap<String, usize>,
    pub(crate) rates: Vec<(String, Rate)>,
    pub(crate) jobs: JobRegistry,
    pub(crate) schedule: Vec<schedule::Entry>,
    pub(crate) errors: Vec<String>,
    pub(crate) alert_hooks: Vec<crate::alert::AlertHook>,
    pub(crate) gate: Option<crate::web::GateFn>,
}

impl std::fmt::Debug for Watchfire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watchfire")
            .field("agents", &self.agent_names())
            .field("jobs", &self.jobs)
            .field("schedule", &self.schedule)
            .finish_non_exhaustive()
    }
}

impl Watchfire {
    /// Nothing registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// The registered agent names, in order (pool members as `name#i`).
    pub fn agent_names(&self) -> Vec<&str> {
        self.agents.iter().map(|r| r.name.as_str()).collect()
    }

    /// The registered job names, sorted.
    pub fn job_names(&self) -> Vec<&'static str> {
        self.jobs.names()
    }

    fn push(
        &mut self,
        name: String,
        config: AgentConfig,
        agent: Box<dyn DynAgent>,
    ) -> AgentBuilder<'_> {
        let start = self.agents.len();
        self.agents.push(Registration {
            name,
            config,
            agent,
        });
        AgentBuilder {
            regs: self.agents.get_mut(start..).unwrap_or_default(),
        }
    }

    /// A supervised agent whose every run calls `f` (it should loop until cancelled).
    pub fn run<F, Fut>(&mut self, name: &str, f: F) -> AgentBuilder<'_>
    where
        F: FnMut(AgentCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), AgentError>> + Send + 'static,
    {
        let agent: FnAgent<F> = agent_fn(name, f);
        self.push(name.to_owned(), AgentConfig::default(), Box::new(agent))
    }

    /// An agent calling `f` every `period` (the first call at once). An error ends the run
    /// like any failure (restart policy and backoff apply).
    pub fn every<F, Fut>(&mut self, period: Duration, name: &str, f: F) -> AgentBuilder<'_>
    where
        F: FnMut(AgentCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), AgentError>> + Send + 'static,
    {
        let agent = EveryAgent {
            name: name.to_owned(),
            period,
            f,
        };
        self.push(name.to_owned(), AgentConfig::default(), Box::new(agent))
    }

    /// An agent calling `f` for every `event` emitted (`ctx.emit(event, payload)`) while it
    /// runs.
    pub fn on_event<F, Fut>(&mut self, event: &str, name: &str, f: F) -> AgentBuilder<'_>
    where
        F: FnMut(AgentCtx, Event) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), AgentError>> + Send + 'static,
    {
        let agent = EventAgent {
            name: name.to_owned(),
            event: event.to_owned(),
            f,
        };
        self.push(name.to_owned(), AgentConfig::default(), Box::new(agent))
    }

    /// A full agent, with its own name and config.
    pub fn agent(&mut self, agent: impl Agent) -> AgentBuilder<'_> {
        let name = agent.name();
        let config = agent.config();
        self.push(name, config, Box::new(agent))
    }

    /// `count` instances named `name#0` … `name#<count-1>`, each built by `make(index)` and
    /// supervised on its own.
    pub fn pool<A, F>(&mut self, count: usize, name: &str, mut make: F) -> AgentBuilder<'_>
    where
        A: Agent,
        F: FnMut(usize) -> A,
    {
        if let Err(e) = validate_name(name) {
            self.errors.push(e.to_string());
        }
        if count == 0 {
            self.errors
                .push(format!("pool `{name}` needs at least one instance"));
        }
        let start = self.agents.len();
        for i in 0..count {
            let agent = make(i);
            let config = agent.config();
            self.agents.push(Registration {
                name: format!("{name}#{i}"),
                config,
                agent: Box::new(agent),
            });
        }
        AgentBuilder {
            regs: self.agents.get_mut(start..).unwrap_or_default(),
        }
    }

    /// A group of agents, to limit how many run at once.
    pub fn group(&mut self, name: &str) -> GroupBuilder<'_> {
        if let Err(e) = validate_name(name) {
            self.errors.push(e.to_string());
        }
        GroupBuilder {
            groups: &mut self.groups,
            name: name.to_owned(),
        }
    }

    /// Limit requests made through `ctx.http()` to `host` (exact host name, e.g.
    /// `example.com`): a token bucket of `rate`.
    pub fn rate_limit(&mut self, host: &str, rate: Rate) -> &mut Self {
        self.rates.retain(|(h, _)| !h.eq_ignore_ascii_case(host));
        self.rates.push((host.to_owned(), rate));
        self
    }

    /// Register a job type, so the queue workers can run it.
    pub fn job<J: Job>(&mut self) -> &mut Self {
        if !self.jobs.add::<J>() {
            self.errors
                .push(format!("job `{}` is registered twice", J::NAME));
        }
        self
    }

    /// Call `f` for every alert: an agent `failed` (its run failed under `Restart::Never`, or it
    /// hit its restart limit), a `stalled` run, a `dead_letter` job. Hooks run one at a time
    /// in a task of their own (at most 10 s each), so supervision never waits for them.
    ///
    /// ```
    /// use smeltery_watchfire::prelude::*;
    ///
    /// fn register(w: &mut Watchfire) {
    ///     w.on_alert(|alert| async move {
    ///         eprintln!("[{}] {}: {}", alert.kind.as_str(), alert.agent, alert.message);
    ///     });
    /// }
    /// # register(&mut Watchfire::new());
    /// ```
    pub fn on_alert<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: Fn(crate::alert::Alert) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.alert_hooks.push(crate::alert::hook(f));
        self
    }

    /// Decide who may use the dashboard at `/_watchfire` (its page, its buttons and its live panels) outside
    /// local development: `gate` gets the signed-in user's [`Auth`](smeltery_core::auth::Auth) and the app, and
    /// answers `Ok(true)` to let them in. Guests never reach it (they are sent to the `login` route), and without
    /// a gate nobody gets in: the dashboard is open without sign-in only under `APP_ENV=local`, to requests from
    /// the machine itself (see `WATCHFIRE_DASHBOARD`). An error from the gate is answered as a server error.
    ///
    /// ```
    /// use smeltery_watchfire::prelude::*;
    ///
    /// fn register(w: &mut Watchfire) {
    ///     // Only the user with id 1 (load the model with `auth.user::<User>().await?` for a role check).
    ///     w.dashboard_gate(|auth, _app| async move { Ok(auth.id() == Some(1)) });
    /// }
    /// # register(&mut Watchfire::new());
    /// ```
    pub fn dashboard_gate<F, Fut>(&mut self, gate: F) -> &mut Self
    where
        F: Fn(smeltery_core::auth::Auth, smeltery_core::App) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = smeltery_core::Result<bool>> + Send + 'static,
    {
        self.gate = Some(std::sync::Arc::new(move |auth, app| {
            Box::pin(gate(auth, app))
        }));
        self
    }

    /// Add to the schedule.
    pub fn schedule(&mut self) -> ScheduleBuilder<'_> {
        ScheduleBuilder {
            entries: &mut self.schedule,
        }
    }

    /// Check everything registered: names, duplicates, groups, the schedule.
    ///
    /// # Errors
    /// The first problem found.
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(error) = self.errors.first() {
            return Err(Error::Config(error.clone()));
        }
        let mut seen = HashSet::new();
        for reg in &self.agents {
            let base = reg
                .name
                .split_once('#')
                .map_or(reg.name.as_str(), |(b, _)| b);
            validate_name(base)?;
            if RESERVED.contains(&base) {
                return Err(Error::InvalidName {
                    name: reg.name.clone(),
                    reason: "is reserved for Watchfire's own agents",
                });
            }
            if !seen.insert(reg.name.as_str()) {
                return Err(Error::Duplicate {
                    name: reg.name.clone(),
                });
            }
            if let Some(group) = &reg.config.group {
                validate_name(group)?;
            }
        }
        schedule::validate(&self.schedule)?;
        for entry in &self.schedule {
            if let schedule::Target::Agent(name) = &entry.target
                && !seen.contains(name.as_str())
            {
                return Err(Error::Config(format!(
                    "schedule `{}` starts the unknown agent `{name}`",
                    entry.name
                )));
            }
        }
        Ok(())
    }

    /// Whether anything is registered to run.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty() && self.jobs.is_empty() && self.schedule.is_empty()
    }
}

/// Settings for what `Watchfire::run` / `every` / `on_event` / `agent` / `pool` registered
/// (every pool member). They override the agent's own `config()`.
#[derive(Debug)]
pub struct AgentBuilder<'a> {
    regs: &'a mut [Registration],
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("name", &self.name)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl AgentBuilder<'_> {
    fn each(self, f: impl Fn(&mut AgentConfig)) -> Self {
        for reg in self.regs.iter_mut() {
            f(&mut reg.config);
        }
        self
    }

    /// The restart policy.
    pub fn restart(self, policy: Restart) -> Self {
        self.each(|c| c.restart = policy)
    }

    /// Backoff `initial..=cap` (doubling, full jitter).
    pub fn backoff(self, range: RangeInclusive<Duration>) -> Self {
        let backoff = crate::policy::Backoff::new(range);
        self.each(|c| c.backoff = backoff.clone())
    }

    /// At most `count` automatic restarts within `per`, then `Failed` (with an alert).
    pub fn max_restarts(self, count: u32, per: Duration) -> Self {
        self.each(|c| c.max_restarts = Some((count, per)))
    }

    /// No heartbeat for this long: the run is stalled and restarted.
    pub fn heartbeat_timeout(self, timeout: Duration) -> Self {
        self.each(|c| c.heartbeat_timeout = Some(timeout))
    }

    /// Put it in a group (see [`Watchfire::group`]).
    pub fn group(self, group: &str) -> Self {
        self.each(|c| c.group = Some(group.to_owned()))
    }

    /// Start on launch (default `true`).
    pub fn autostart(self, autostart: bool) -> Self {
        self.each(|c| c.autostart = autostart)
    }

    /// How long a cancelled run may take to return before it is dropped (`Killed`).
    pub fn shutdown_timeout(self, timeout: Duration) -> Self {
        self.each(|c| c.shutdown_timeout = timeout)
    }

    /// Permits for [`AgentCtx::acquire`].
    pub fn concurrency(self, permits: usize) -> Self {
        let permits = permits.clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
        self.each(|c| c.concurrency = permits)
    }

    /// Run in every process instead of one at a time (see [`AgentConfig::per_process`]); for a pool, every
    /// process then runs all its members.
    pub fn per_process(self) -> Self {
        self.each(|c| c.per_process = true)
    }
}

/// [`Watchfire::group`].
#[derive(Debug)]
pub struct GroupBuilder<'a> {
    groups: &'a mut BTreeMap<String, usize>,
    name: String,
}

impl GroupBuilder<'_> {
    /// At most `n` agents of the group run at once; the others wait in `Starting` (0 is
    /// treated as 1).
    pub fn limit(self, n: usize) {
        self.groups.insert(self.name, n.max(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::DurationExt;

    #[test]
    fn validation_catches_mistakes() {
        let ok = |_: AgentCtx| async { Ok::<(), AgentError>(()) };
        let mut w = Watchfire::new();
        w.run("a", ok);
        w.run("a", ok);
        assert!(matches!(w.validate(), Err(Error::Duplicate { .. })));

        let mut w = Watchfire::new();
        w.run("Bad Name", ok);
        assert!(matches!(w.validate(), Err(Error::InvalidName { .. })));

        let mut w = Watchfire::new();
        w.run("scheduler", ok);
        assert!(w.validate().unwrap_err().to_string().contains("reserved"));

        let mut w = Watchfire::new();
        w.pool(3, "fetch", |i| agent_fn(format!("ignored{i}"), ok))
            .group("scrapers")
            .restart(Restart::Always);
        w.group("scrapers").limit(2);
        w.schedule().agent("fetch#1").every(5.mins());
        w.validate().unwrap();
        assert_eq!(w.agent_names(), ["fetch#0", "fetch#1", "fetch#2"]);
        assert!(
            w.agents
                .iter()
                .all(|r| r.config.group.as_deref() == Some("scrapers")
                    && r.config.restart == Restart::Always)
        );
        assert_eq!(w.groups.get("scrapers"), Some(&2));

        let mut w = Watchfire::new();
        w.schedule().agent("ghost").hourly();
        assert!(
            w.validate()
                .unwrap_err()
                .to_string()
                .contains("unknown agent")
        );
    }
}
