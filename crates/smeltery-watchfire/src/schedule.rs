//! The scheduler: tasks started by the clock. The clock itself is an agent named `scheduler`.

use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt as _;
use smeltery_core::BoxFuture;
use tokio::task::JoinSet;

use crate::agent::Agent;
use crate::ctx::AgentCtx;
use crate::error::{AgentError, Error};
use crate::queue::Job;
use crate::runtime::Agents;
use crate::status::{AgentState, RunOutcome, RunRecord};
use crate::time::{civil_from_days, duration_ms, weekday};

const MINUTE_MS: i64 = 60_000;

/// How long a scheduled call may take to stop once its run lease is lost (then it is dropped).
pub(crate) const CALL_STOP: Duration = Duration::from_secs(10);

/// What a scheduled call's task returns: its result, or the panic it caught.
type CallResult = Result<Result<(), AgentError>, Box<dyn std::any::Any + Send>>;

/// A 5-field cron expression (`minute hour day-of-month month day-of-week`, UTC): `*`, lists
/// (`1,15`), ranges (`1-5`), steps (`*/10`, `0-30/5`, `5/15`); day of week 0-7 (0 and 7 are
/// Sunday). When both day fields are restricted, a day matching either runs (classic cron).
///
/// ```
/// use smeltery_watchfire::Cron;
///
/// let cron = Cron::parse("30 3 * * 1-5").unwrap();
/// // 2026-10-03 is a Saturday: the next run is Monday 2026-10-05 03:30 UTC.
/// let saturday = 1_790_985_600_000; // 2026-10-03 00:00:00 UTC
/// assert_eq!(cron.next_after(saturday), Some(saturday + 2 * 86_400_000 + (3 * 60 + 30) * 60_000));
/// assert!(Cron::parse("61 * * * *").is_err());
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    source: String,
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    days_star: bool,
    weekdays_star: bool,
}

impl Cron {
    /// Parse an expression.
    ///
    /// # Errors
    /// [`Error::Config`] naming the bad field.
    pub fn parse(expr: &str) -> Result<Self, Error> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields.as_slice() else {
            return Err(Error::Config(format!(
                "cron expression `{expr}` must have 5 fields (minute hour day month weekday)"
            )));
        };
        let field = |text: &str, name: &str, min: u32, max: u32| {
            parse_field(text, min, max).map_err(|reason| {
                Error::Config(format!(
                    "cron expression `{expr}`: invalid {name} field `{text}`: {reason}"
                ))
            })
        };
        let mut weekdays = field(weekday, "day-of-week", 0, 7)?;
        if weekdays & (1 << 7) != 0 {
            weekdays = (weekdays | 1) & !(1 << 7);
        }
        Ok(Self {
            source: fields.join(" "),
            minutes: field(minute, "minute", 0, 59)?,
            hours: field(hour, "hour", 0, 23)?,
            days: field(day, "day-of-month", 1, 31)?,
            months: field(month, "month", 1, 12)?,
            weekdays,
            days_star: day.starts_with('*'),
            weekdays_star: weekday.starts_with('*'),
        })
    }

    /// The expression as written (fields joined by one space).
    pub fn as_str(&self) -> &str {
        &self.source
    }

    fn day_matches(&self, days_since_epoch: i64, day: u32) -> bool {
        let dom = self.days & (1 << day) != 0;
        let dow = self.weekdays & (1 << weekday(days_since_epoch)) != 0;
        match (self.days_star, self.weekdays_star) {
            (false, false) => dom || dow,
            _ => dom && dow,
        }
    }

    /// Whether the minute containing `ms` (Unix milliseconds) matches.
    pub fn matches(&self, ms: i64) -> bool {
        let minute = ms.div_euclid(MINUTE_MS);
        let days = minute.div_euclid(1440);
        let in_day = minute.rem_euclid(1440);
        let (_, month, day) = civil_from_days(days);
        bit(self.months, month)
            && self.day_matches(days, day)
            && bit(self.hours, u32::try_from(in_day / 60).unwrap_or(0))
            && bit(self.minutes, u32::try_from(in_day % 60).unwrap_or(0))
    }

    /// The first matching minute strictly after `ms`, in Unix milliseconds; `None` when nothing
    /// matches within 8 years (e.g. `0 0 31 2 *`).
    pub fn next_after(&self, ms: i64) -> Option<i64> {
        let start = ms.div_euclid(MINUTE_MS) + 1;
        let first_day = start.div_euclid(1440);
        for days in first_day..first_day + 366 * 8 {
            let (_, month, day) = civil_from_days(days);
            if !bit(self.months, month) || !self.day_matches(days, day) {
                continue;
            }
            let from = if days == first_day {
                start.rem_euclid(1440)
            } else {
                0
            };
            for in_day in from..1440 {
                let (h, m) = (in_day / 60, in_day % 60);
                if bit(self.hours, u32::try_from(h).unwrap_or(0))
                    && bit(self.minutes, u32::try_from(m).unwrap_or(0))
                {
                    return Some((days * 1440 + in_day) * MINUTE_MS);
                }
            }
        }
        None
    }
}

fn bit(mask: u64, n: u32) -> bool {
    n < 64 && mask & (1 << n) != 0
}

fn parse_field(text: &str, min: u32, max: u32) -> Result<u64, String> {
    let mut mask = 0_u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("`{step}` is not a step"))?;
                if step == 0 {
                    return Err("a step must be at least 1".to_owned());
                }
                (range, step)
            }
            None => (part, 1),
        };
        let number = |s: &str| -> Result<u32, String> {
            let n: u32 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
            if n < min || n > max {
                return Err(format!("{n} is outside {min}-{max}"));
            }
            Ok(n)
        };
        let (from, to) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let (a, b) = (number(a)?, number(b)?);
            if a > b {
                return Err(format!("range {a}-{b} is backwards"));
            }
            (a, b)
        } else {
            let a = number(range)?;
            // `5/15` means from 5 to the end, every 15.
            (a, if part.contains('/') { max } else { a })
        };
        let mut n = from;
        while n <= to {
            mask |= 1 << n;
            // A huge step (`59/4294967295`) ends the field instead of overflowing.
            let Some(next) = n.checked_add(step) else {
                break;
            };
            n = next;
        }
    }
    Ok(mask)
}

/// What to do when a task is due while its previous run still runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Overlap {
    /// Skip this occurrence (default).
    #[default]
    Skip,
    /// Run it once the previous run finishes (at most one waits).
    Queue,
    /// Run it anyway, in parallel.
    Allow,
}

/// When a scheduled task runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Timing {
    Every(Duration),
    Cron(Cron),
}

impl Timing {
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Every(d) => {
                let secs = d.as_secs();
                if d.subsec_millis() != 0 || secs == 0 {
                    format!("every {}ms", d.as_millis())
                } else if secs % 3600 == 0 {
                    format!("every {}h", secs / 3600)
                } else if secs % 60 == 0 {
                    format!("every {}m", secs / 60)
                } else {
                    format!("every {secs}s")
                }
            }
            Self::Cron(cron) => cron.as_str().to_owned(),
        }
    }

    /// The first run after `now` for a schedule starting at `now`.
    pub(crate) fn first_after(&self, now: i64) -> Option<i64> {
        match self {
            Self::Every(d) => Some(now.saturating_add(duration_ms(*d).max(1))),
            Self::Cron(cron) => cron.next_after(now),
        }
    }

    /// The first run after `now` that every process agrees on: `every(d)` on the next multiple of `d` since the
    /// Unix epoch, cron as usual.
    pub(crate) fn first_aligned(&self, now: i64) -> Option<i64> {
        match self {
            Self::Every(d) => {
                let step = duration_ms(*d).max(1);
                Some(now.div_euclid(step).saturating_add(1).saturating_mul(step))
            }
            Self::Cron(cron) => cron.next_after(now),
        }
    }

    /// How long the claim of one tick is kept: long enough for a process whose clock is behind to still find
    /// it (at least a minute, at most an hour).
    pub(crate) fn claim_ttl(&self) -> Duration {
        match self {
            Self::Every(d) => (*d).clamp(Duration::from_secs(60), Duration::from_secs(3600)),
            Self::Cron(_) => Duration::from_secs(3600),
        }
    }

    /// For `schedule:run` (called by system cron once a minute): whether the task is due in
    /// the minute containing `now`. `every(d)` is due when the minute count since the epoch is
    /// a multiple of `d` in minutes (always, for less than a minute).
    pub(crate) fn due_in_minute(&self, now: i64) -> bool {
        match self {
            Self::Every(d) => {
                let minutes = i64::try_from(d.as_secs() / 60).unwrap_or(i64::MAX);
                minutes <= 1 || now.div_euclid(MINUTE_MS) % minutes == 0
            }
            Self::Cron(cron) => cron.matches(now),
        }
    }
}

pub(crate) type CallFn =
    Arc<dyn Fn(AgentCtx) -> BoxFuture<'static, Result<(), AgentError>> + Send + Sync>;

pub(crate) enum Target {
    Job { name: &'static str, payload: String },
    Call(CallFn),
    Agent(String),
}

/// One registered schedule entry.
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) timing: Option<Timing>,
    pub(crate) overlap: Overlap,
    pub(crate) target: Target,
    pub(crate) error: Option<String>,
    /// Run in every process (no cross-process claim).
    pub(crate) per_process: bool,
}

/// The claim key of one due tick of a task.
pub(crate) fn tick_key(name: &str, at: i64) -> String {
    format!("watchfire:schedule:{name}:{at}")
}

/// The lease a non-overlapping call holds while it runs.
pub(crate) fn run_lock(name: &str) -> String {
    format!("watchfire:schedule-run:{name}")
}

impl Entry {
    /// A call that must not overlap itself across processes: it holds a run lease while it runs.
    pub(crate) fn needs_run_lease(&self) -> bool {
        matches!(self.target, Target::Call(_))
            && matches!(self.overlap, Overlap::Skip | Overlap::Queue)
            && !self.per_process
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self.target {
            Target::Job { .. } => "job",
            Target::Call(_) => "call",
            Target::Agent(_) => "agent",
        }
    }
}

/// `Watchfire::schedule()`: pick what runs.
#[derive(Debug)]
pub struct ScheduleBuilder<'a> {
    pub(crate) entries: &'a mut Vec<Entry>,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("name", &self.name)
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

impl<'a> ScheduleBuilder<'a> {
    fn push(self, name: String, target: Target, error: Option<String>) -> ScheduledTask<'a> {
        let entries = self.entries;
        entries.push(Entry {
            name,
            timing: None,
            overlap: Overlap::Skip,
            target,
            error,
            per_process: false,
        });
        ScheduledTask {
            entry: entries.last_mut(),
        }
    }

    /// Dispatch `job` (a copy of it, serialized now) on schedule. The entry is named after
    /// the job.
    pub fn job<J: Job>(self, job: J) -> ScheduledTask<'a> {
        let (payload, error) = match serde_json::to_string(&job) {
            Ok(payload) => (payload, None),
            Err(e) => (
                String::new(),
                Some(format!("job `{}` does not serialize: {e}", J::NAME)),
            ),
        };
        self.push(
            J::NAME.to_owned(),
            Target::Job {
                name: J::NAME,
                payload,
            },
            error,
        )
    }

    /// Call `f` on schedule, recorded as a run of the `scheduler` agent named `name`.
    pub fn call<F, Fut>(self, name: &str, f: F) -> ScheduledTask<'a>
    where
        F: Fn(AgentCtx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AgentError>> + Send + 'static,
    {
        let f: CallFn = Arc::new(move |ctx| Box::pin(f(ctx)));
        let error = crate::config::validate_name(name)
            .err()
            .map(|e| e.to_string());
        self.push(name.to_owned(), Target::Call(f), error)
    }

    /// Start the agent `name` on schedule, unless it is running, starting or paused.
    pub fn agent(self, name: &str) -> ScheduledTask<'a> {
        self.push(
            format!("agent:{name}"),
            Target::Agent(name.to_owned()),
            None,
        )
    }
}

/// A scheduled task: pick when it runs and what happens on overlap.
#[derive(Debug)]
pub struct ScheduledTask<'a> {
    entry: Option<&'a mut Entry>,
}

impl ScheduledTask<'_> {
    fn timing(mut self, timing: Result<Timing, Error>) -> Self {
        if let Some(entry) = self.entry.as_deref_mut() {
            match timing {
                Ok(timing) => entry.timing = Some(timing),
                Err(e) => entry.error = Some(e.to_string()),
            }
        }
        self
    }

    /// Every `period`, the first one `period` after launch.
    pub fn every(self, period: Duration) -> Self {
        if period.is_zero() {
            return self.timing(Err(Error::Config(
                "`every` needs a period above zero".into(),
            )));
        }
        self.timing(Ok(Timing::Every(period)))
    }

    /// Every minute (`* * * * *`).
    pub fn every_minute(self) -> Self {
        self.cron("* * * * *")
    }

    /// At the start of every hour (`0 * * * *`).
    pub fn hourly(self) -> Self {
        self.cron("0 * * * *")
    }

    /// Every day at midnight UTC (`0 0 * * *`).
    pub fn daily(self) -> Self {
        self.cron("0 0 * * *")
    }

    /// Every day at `HH:MM` UTC.
    pub fn daily_at(self, time: &str) -> Self {
        let parsed = time.split_once(':').and_then(|(h, m)| {
            let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
            (h < 24 && m < 60 && time.len() <= 5).then_some((h, m))
        });
        match parsed {
            Some((h, m)) => self.cron(&format!("{m} {h} * * *")),
            None => self.timing(Err(Error::Config(format!(
                "daily_at(\"{time}\"): expected HH:MM, e.g. \"03:00\""
            )))),
        }
    }

    /// Every Sunday at midnight UTC (`0 0 * * 0`).
    pub fn weekly(self) -> Self {
        self.cron("0 0 * * 0")
    }

    /// A 5-field cron expression (UTC), see [`Cron`].
    pub fn cron(self, expr: &str) -> Self {
        self.timing(Cron::parse(expr).map(Timing::Cron))
    }

    /// What happens when it is due while still running (default [`Overlap::Skip`]).
    pub fn overlap(mut self, overlap: Overlap) -> Self {
        if let Some(entry) = self.entry.as_deref_mut() {
            entry.overlap = overlap;
        }
        self
    }

    /// Run it in every process. By default, with a shared lock store (`WATCHFIRE_LOCK_STORE`), each due tick of a
    /// job or call runs in one process only, and a call with [`Overlap::Skip`] or [`Overlap::Queue`] never runs
    /// in two processes at once.
    pub fn per_process(mut self) -> Self {
        if let Some(entry) = self.entry.as_deref_mut() {
            entry.per_process = true;
        }
        self
    }

    /// Name the entry (default: the job name, the call name, or `agent:<name>`).
    pub fn name(mut self, name: &str) -> Self {
        if let Some(entry) = self.entry.as_deref_mut() {
            entry.name = name.to_owned();
        }
        self
    }
}

/// The schedule as `schedule:list` and the API show it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct ScheduleInfo {
    /// Entry name.
    pub name: String,
    /// `job`, `call` or `agent`.
    pub kind: &'static str,
    /// `every 5m` or the cron expression.
    pub expression: String,
    /// The next run after now, Unix milliseconds.
    pub next_run_ms: Option<i64>,
}

pub(crate) fn infos(entries: &[Entry], now: i64) -> Vec<ScheduleInfo> {
    entries
        .iter()
        .map(|e| ScheduleInfo {
            name: e.name.clone(),
            kind: e.kind(),
            expression: e.timing.as_ref().map(Timing::describe).unwrap_or_default(),
            next_run_ms: e.timing.as_ref().and_then(|t| t.first_after(now)),
        })
        .collect()
}

/// Check the entries: a timing each, no errors, unique names.
pub(crate) fn validate(entries: &[Entry]) -> Result<(), Error> {
    let mut seen = std::collections::HashSet::new();
    for e in entries {
        if let Some(error) = &e.error {
            return Err(Error::Config(format!("schedule `{}`: {error}", e.name)));
        }
        if e.timing.is_none() {
            return Err(Error::Config(format!(
                "schedule `{}` has no timing: add .every(..), .daily(), .cron(..) …",
                e.name
            )));
        }
        if !seen.insert(e.name.as_str()) {
            return Err(Error::Duplicate {
                name: format!("schedule {}", e.name),
            });
        }
    }
    Ok(())
}

/// The `scheduler` agent.
pub(crate) struct SchedulerAgent {
    pub(crate) entries: Arc<Vec<Entry>>,
}

struct Slot {
    next_at: Option<i64>,
    running: usize,
    queued: bool,
}

struct Running {
    slot: usize,
    record: RunRecord,
    /// Held while a non-overlapping call runs, with a shared lock store.
    lease: Option<crate::coord::Lease>,
}

impl Agent for SchedulerAgent {
    fn name(&self) -> String {
        "scheduler".to_owned()
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        let shared = Arc::clone(ctx.shared());
        let agents = Agents::new(Arc::clone(&shared));
        let start = shared.clock.now_ms();
        let coordinated = shared.coord.is_some();
        let mut slots: Vec<Slot> = self
            .entries
            .iter()
            .map(|e| Slot {
                // With a shared lock store every process must compute the same ticks.
                next_at: e.timing.as_ref().and_then(|t| {
                    if coordinated && !e.per_process {
                        t.first_aligned(start)
                    } else {
                        t.first_after(start)
                    }
                }),
                running: 0,
                queued: false,
            })
            .collect();
        let mut calls: JoinSet<CallResult> = JoinSet::new();
        let mut running: HashMap<tokio::task::Id, Running> = HashMap::new();
        loop {
            ctx.heartbeat();
            let now = shared.clock.now_ms();
            let mut fire = Vec::new();
            for (i, (slot, entry)) in slots.iter_mut().zip(self.entries.iter()).enumerate() {
                let Some(at) = slot.next_at else { continue };
                if at > now {
                    continue;
                }
                fire.push((i, at));
                slot.next_at = match &entry.timing {
                    Some(Timing::Every(d)) => {
                        // Skip occurrences missed while the process was busy or asleep.
                        let step = duration_ms(*d).max(1);
                        let behind = (now - at) / step + 1;
                        Some(at.saturating_add(step.saturating_mul(behind)))
                    }
                    Some(Timing::Cron(cron)) => cron.next_after(now),
                    None => None,
                };
            }
            // After one lock-store failure in a pass, the remaining claims of the pass are skipped at once, so an
            // unreachable store does not hold the loop for a store timeout per task.
            let mut down = false;
            for (i, at) in fire {
                self.fire(
                    i,
                    Some(at),
                    &mut down,
                    &ctx,
                    &agents,
                    &mut slots,
                    &mut calls,
                    &mut running,
                )
                .await;
            }
            let next = slots.iter().filter_map(|s| s.next_at).min();
            let wait = next.map_or(Duration::from_secs(3600), |at| {
                Duration::from_millis(u64::try_from((at - now).max(1)).unwrap_or(1))
            });
            tokio::select! {
                biased;
                () = ctx.cancelled() => break,
                Some(done) = calls.join_next_with_id() => {
                    let (id, result) = match done {
                        Ok((id, result)) => (id, Ok(result)),
                        Err(e) => (e.id(), Err(e)),
                    };
                    if let Some(Running { slot, mut record, lease }) = running.remove(&id) {
                        if let Some(lease) = lease {
                            lease.release().await;
                        }
                        let (outcome, error) = match result {
                            Ok(Ok(Ok(()))) => (RunOutcome::Completed, None),
                            Ok(Ok(Err(e))) => (RunOutcome::Failed, Some(e.to_string())),
                            Ok(Err(panic)) => (RunOutcome::Panicked, Some(crate::runtime::panic_message(&*panic))),
                            Err(e) => (RunOutcome::Killed, Some(e.to_string())),
                        };
                        if let Some(e) = &error {
                            tracing::warn!(task = %record.job.as_deref().unwrap_or(""), error = %e, "scheduled call failed");
                        }
                        record.outcome = outcome;
                        record.error = error;
                        record.ended_at_ms = Some(shared.clock.now_ms());
                        shared.persist_run(&record).await;
                        if let Some(s) = slots.get_mut(slot) {
                            s.running = s.running.saturating_sub(1);
                            if s.queued && s.running == 0 {
                                s.queued = false;
                                self.fire(slot, None, &mut false, &ctx, &agents, &mut slots, &mut calls, &mut running).await;
                            }
                        }
                    }
                }
                () = tokio::time::sleep(wait) => {}
            }
        }
        // Cancelled: running calls share the run's token; give them the grace, then record
        // the ones that did not finish as killed.
        let grace = shared.grace(Duration::from_secs(10));
        let deadline = tokio::time::Instant::now() + grace;
        while !running.is_empty() {
            match tokio::time::timeout_at(deadline, calls.join_next_with_id()).await {
                Ok(Some(done)) => {
                    let (id, outcome, error) = match done {
                        Ok((id, Ok(Ok(())))) => (id, RunOutcome::Stopped, None),
                        Ok((id, Ok(Err(e)))) => (id, RunOutcome::Stopped, Some(e.to_string())),
                        Ok((id, Err(panic))) => (
                            id,
                            RunOutcome::Panicked,
                            Some(crate::runtime::panic_message(&*panic)),
                        ),
                        Err(e) => (e.id(), RunOutcome::Killed, Some(e.to_string())),
                    };
                    if let Some(Running {
                        mut record, lease, ..
                    }) = running.remove(&id)
                    {
                        if let Some(lease) = lease {
                            lease.release().await;
                        }
                        record.outcome = outcome;
                        record.error = error;
                        record.ended_at_ms = Some(shared.clock.now_ms());
                        shared.persist_run(&record).await;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        calls.abort_all();
        for (
            _,
            Running {
                mut record, lease, ..
            },
        ) in running.drain()
        {
            // Freed now, so another process need not wait out the lease.
            if let Some(lease) = lease {
                lease.release().await;
            }
            record.outcome = RunOutcome::Killed;
            record.error = Some(format!("did not stop within {grace:?}"));
            record.ended_at_ms = Some(shared.clock.now_ms());
            shared.persist_run(&record).await;
        }
        Ok(())
    }
}

impl SchedulerAgent {
    #[allow(clippy::too_many_arguments)]
    /// Run entry `i`. `at` is the due tick (`None`: a queued run of a tick already claimed). `down` is set after
    /// a lock-store failure, and skips claims while set.
    async fn fire(
        &self,
        i: usize,
        at: Option<i64>,
        down: &mut bool,
        ctx: &AgentCtx,
        agents: &Agents,
        slots: &mut [Slot],
        calls: &mut JoinSet<CallResult>,
        running: &mut HashMap<tokio::task::Id, Running>,
    ) {
        let (Some(entry), Some(slot)) = (self.entries.get(i), slots.get_mut(i)) else {
            return;
        };
        let shared = ctx.shared();
        let coord = shared.coord.clone().filter(|_| !entry.per_process);
        // A job or call runs once per tick among the processes sharing the lock store. Agent targets need no
        // claim: a singleton agent starts only in the process that holds it.
        if let (Some(coord), Some(at), false) =
            (&coord, at, matches!(entry.target, Target::Agent(_)))
        {
            if *down {
                tracing::debug!(task = %entry.name, "the lock store is unreachable; skipped");
                return;
            }
            let ttl = entry
                .timing
                .as_ref()
                .map_or(Duration::from_secs(3600), Timing::claim_ttl);
            match coord.claim(&tick_key(&entry.name, at), ttl).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::debug!(task = %entry.name, "this tick runs in another process; skipped");
                    return;
                }
                Err(e) => {
                    tracing::warn!(task = %entry.name, error = %e, "cannot claim the scheduled tick; skipped (and the other tasks due now)");
                    *down = true;
                    return;
                }
            }
        }
        match &entry.target {
            Target::Job { name, payload } => {
                let Some(queue) = &shared.queue else { return };
                match queue.push_raw(name, payload, Duration::ZERO).await {
                    Ok(id) => {
                        tracing::info!(task = %entry.name, job_id = id, "scheduled job dispatched")
                    }
                    Err(e) => {
                        tracing::warn!(task = %entry.name, error = %e, "cannot dispatch scheduled job")
                    }
                }
            }
            Target::Agent(name) => match agents.status(name) {
                Ok(status)
                    if matches!(
                        status.state,
                        AgentState::Stopped
                            | AgentState::Completed
                            | AgentState::Failed
                            | AgentState::BackingOff
                    ) =>
                {
                    match agents.start(name).await {
                        Ok(_) => tracing::info!(task = %entry.name, "scheduled agent started"),
                        Err(e) => {
                            tracing::warn!(task = %entry.name, error = %e, "cannot start scheduled agent")
                        }
                    }
                }
                Ok(_) => {
                    tracing::debug!(task = %entry.name, "agent already running or paused; skipped")
                }
                Err(e) => {
                    tracing::warn!(task = %entry.name, error = %e, "scheduled agent is unknown")
                }
            },
            Target::Call(f) => {
                if slot.running > 0 {
                    match entry.overlap {
                        Overlap::Skip => {
                            tracing::debug!(task = %entry.name, "still running; skipped");
                            return;
                        }
                        Overlap::Queue => {
                            slot.queued = true;
                            return;
                        }
                        Overlap::Allow => {}
                    }
                }
                let lease = match (&coord, entry.overlap) {
                    (Some(coord), Overlap::Skip | Overlap::Queue) => {
                        match coord
                            .lease(&run_lock(&entry.name), CALL_STOP, |task| {
                                shared.spawn_task(task)
                            })
                            .await
                        {
                            Ok(Some(lease)) => Some(lease),
                            Ok(None) => {
                                tracing::debug!(task = %entry.name, "still running in another process; skipped");
                                return;
                            }
                            Err(e) => {
                                tracing::warn!(task = %entry.name, error = %e, "cannot take the scheduled call's lock; skipped");
                                *down = true;
                                return;
                            }
                        }
                    }
                    _ => None,
                };
                slot.running += 1;
                let record = RunRecord::started(
                    ctx.name(),
                    ctx.next_run_id(),
                    Some(entry.name.clone()),
                    shared.clock.now_ms(),
                );
                shared.persist_run(&record).await;
                ctx.persist_status().await;
                let fut = guarded(f, ctx, lease.as_ref().map(|l| l.lost().clone()));
                let handle = calls.spawn(AssertUnwindSafe(fut).catch_unwind());
                running.insert(
                    handle.id(),
                    Running {
                        slot: i,
                        record,
                        lease,
                    },
                );
            }
        }
    }
}

/// Call `f` under a token of its own: when `lost` (the call's run lease) is cancelled, the call is cancelled too and
/// dropped if it has not returned within [`CALL_STOP`] (another process may run it then).
fn guarded(
    f: &CallFn,
    ctx: &AgentCtx,
    lost: Option<tokio_util::sync::CancellationToken>,
) -> impl Future<Output = Result<(), AgentError>> + Send + 'static {
    let token = ctx.token().child_token();
    let fut = f(ctx.with_token(token.clone()));
    async move {
        let Some(lost) = lost else {
            return fut.await;
        };
        tokio::pin!(fut);
        tokio::select! {
            result = &mut fut => result,
            () = lost.cancelled() => {
                token.cancel();
                match tokio::time::timeout(CALL_STOP, &mut fut).await {
                    Ok(_) => Err(AgentError::msg("stopped: the run lease was lost")),
                    Err(_) => Err(AgentError::msg(format!(
                        "the run lease was lost and the call did not stop within {CALL_STOP:?}; dropped"
                    ))),
                }
            }
        }
    }
}

/// Run every entry due in the current minute once (`schedule:run`), writing a line per
/// entry. Calls run one after the other; agent targets are skipped (agents run in `serve` /
/// `work`).
pub(crate) async fn run_due(
    entries: &[Entry],
    ctx: &AgentCtx,
    out: &mut (dyn std::io::Write + Send),
) -> smeltery_core::Result<()> {
    let shared = ctx.shared();
    let now = shared.clock.now_ms();
    let mut ran = 0;
    for entry in entries {
        let Some(timing) = &entry.timing else {
            continue;
        };
        if !timing.due_in_minute(now) {
            continue;
        }
        ran += 1;
        let coord = shared.coord.clone().filter(|_| !entry.per_process);
        // With coordination `serve` / `work` run `every(d)` on multiples of `d`: only whole minutes meet the minute
        // ticks claimed here.
        if coord.is_some()
            && let Timing::Every(d) = timing
            && (d.as_secs() % 60 != 0 || d.subsec_nanos() != 0 || d.is_zero())
            && !matches!(entry.target, Target::Agent(_))
        {
            writeln!(
                out,
                "Skipped: {} (`every` of less than whole minutes runs only in `serve` / `work` when processes coordinate)",
                entry.name
            )?;
            continue;
        }
        // The tick of this minute runs once among the processes sharing the lock store (cron `schedule:run` on
        // several servers, or next to `serve`).
        if let Some(coord) = &coord
            && !matches!(entry.target, Target::Agent(_))
        {
            let minute = now.div_euclid(MINUTE_MS).saturating_mul(MINUTE_MS);
            match coord
                .claim(&tick_key(&entry.name, minute), timing.claim_ttl())
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    writeln!(out, "Skipped: {} (another process runs it)", entry.name)?;
                    continue;
                }
                Err(e) => {
                    writeln!(out, "Skipped: {} (cannot claim it: {e})", entry.name)?;
                    continue;
                }
            }
        }
        match &entry.target {
            Target::Job { name, payload } => {
                let queue = shared
                    .queue
                    .as_ref()
                    .ok_or_else(|| smeltery_core::Error::internal("no queue is configured"))?;
                let id = queue.push_raw(name, payload, Duration::ZERO).await?;
                writeln!(out, "Dispatched: {} (job #{id})", entry.name)?;
            }
            Target::Call(f) => {
                // A call that must not overlap holds its run lease, as in the scheduler agent.
                let lease = match (&coord, entry.overlap) {
                    (Some(coord), Overlap::Skip | Overlap::Queue) => {
                        match coord
                            .lease(&run_lock(&entry.name), CALL_STOP, |task| {
                                shared.spawn_task(task)
                            })
                            .await
                        {
                            Ok(Some(lease)) => Some(lease),
                            Ok(None) => {
                                writeln!(
                                    out,
                                    "Skipped: {} (still running in another process)",
                                    entry.name
                                )?;
                                continue;
                            }
                            Err(e) => {
                                writeln!(
                                    out,
                                    "Skipped: {} (cannot take its lock: {e})",
                                    entry.name
                                )?;
                                continue;
                            }
                        }
                    }
                    _ => None,
                };
                let mut record = RunRecord::started(
                    "scheduler",
                    ctx.next_run_id(),
                    Some(entry.name.clone()),
                    now,
                );
                let call = guarded(f, ctx, lease.as_ref().map(|l| l.lost().clone()));
                let result = AssertUnwindSafe(call).catch_unwind().await;
                if let Some(lease) = lease {
                    lease.release().await;
                }
                let (outcome, error) = match result {
                    Ok(Ok(())) => (RunOutcome::Completed, None),
                    Ok(Err(e)) => (RunOutcome::Failed, Some(e.to_string())),
                    Err(panic) => (
                        RunOutcome::Panicked,
                        Some(crate::runtime::panic_message(&*panic)),
                    ),
                };
                record.outcome = outcome;
                record.error.clone_from(&error);
                record.ended_at_ms = Some(shared.clock.now_ms());
                shared.persist_run(&record).await;
                match error {
                    None => writeln!(out, "Ran: {}", entry.name)?,
                    Some(e) => writeln!(out, "Failed: {}: {e}", entry.name)?,
                }
            }
            Target::Agent(name) => {
                writeln!(
                    out,
                    "Skipped: {} (agents start only in `serve` / `work`: {name})",
                    entry.name
                )?;
            }
        }
    }
    if ran == 0 {
        writeln!(out, "Nothing is due.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::days_from_civil;

    fn at(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
        (days_from_civil(y, mo, d) * 1440 + h * 60 + mi) * MINUTE_MS
    }

    #[test]
    fn parses_fields() {
        let c = Cron::parse("*/15 0-6/3 1,15 * 1-5").unwrap();
        assert_eq!(c.minutes, (1 << 0) | (1 << 15) | (1 << 30) | (1 << 45));
        assert_eq!(c.hours, (1 << 0) | (1 << 3) | (1 << 6));
        assert_eq!(c.days, (1 << 1) | (1 << 15));
        assert_eq!(c.weekdays, 0b11_1110);
        assert_eq!(
            Cron::parse("5/20 * * * *").unwrap().minutes,
            (1 << 5) | (1 << 25) | (1 << 45)
        );
        assert_eq!(Cron::parse("0 0 * * 7").unwrap().weekdays, 1, "7 is Sunday");
        assert_eq!(
            Cron::parse("  0   3 * *  * ").unwrap().as_str(),
            "0 3 * * *"
        );
        // A step near u32::MAX overflowed the field loop (a panic in debug builds).
        assert_eq!(
            Cron::parse("59/4294967295 * * * *").unwrap().minutes,
            1 << 59
        );
        assert_eq!(Cron::parse("0/4294967295 * * * *").unwrap().minutes, 1);
    }

    #[test]
    fn rejects_invalid_expressions() {
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * 32 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "5-1 * * * *",
            "a * * * *",
            "1-x * * * *",
            "* * * JAN *",
        ] {
            let err = Cron::parse(bad);
            assert!(err.is_err(), "{bad:?} should be rejected");
        }
        let msg = Cron::parse("0 25 * * *").unwrap_err().to_string();
        assert!(
            msg.contains("hour") && msg.contains("25 is outside 0-23"),
            "{msg}"
        );
    }

    #[test]
    fn next_run_computation() {
        let c = Cron::parse("0 3 * * *").unwrap();
        assert_eq!(
            c.next_after(at(2026, 10, 3, 2, 59)),
            Some(at(2026, 10, 3, 3, 0))
        );
        assert_eq!(
            c.next_after(at(2026, 10, 3, 3, 0)),
            Some(at(2026, 10, 4, 3, 0))
        );
        // Strictly after, at minute resolution.
        assert_eq!(
            Cron::parse("* * * * *")
                .unwrap()
                .next_after(at(2026, 10, 3, 3, 0) + 30_000),
            Some(at(2026, 10, 3, 3, 1))
        );
        // Month and year rollover, leap day.
        let c = Cron::parse("0 0 29 2 *").unwrap();
        assert_eq!(
            c.next_after(at(2026, 10, 3, 0, 0)),
            Some(at(2028, 2, 29, 0, 0))
        );
        let c = Cron::parse("30 23 31 12 *").unwrap();
        assert_eq!(
            c.next_after(at(2026, 12, 31, 23, 30)),
            Some(at(2027, 12, 31, 23, 30))
        );
        // Day of month OR day of week when both are restricted: the 13th or any Friday.
        let c = Cron::parse("0 0 13 * 5").unwrap();
        assert_eq!(
            c.next_after(at(2026, 10, 3, 0, 0)),
            Some(at(2026, 10, 9, 0, 0))
        );
        assert_eq!(
            c.next_after(at(2026, 10, 10, 0, 0)),
            Some(at(2026, 10, 13, 0, 0))
        );
        // Weekly: Sunday midnight; 2026-10-03 is a Saturday.
        let c = Cron::parse("0 0 * * 0").unwrap();
        assert_eq!(
            c.next_after(at(2026, 10, 3, 12, 0)),
            Some(at(2026, 10, 4, 0, 0))
        );
        // Never.
        assert_eq!(Cron::parse("0 0 31 2 *").unwrap().next_after(0), None);
        assert!(
            Cron::parse("15 3 * * *")
                .unwrap()
                .matches(at(2026, 10, 3, 3, 15) + 59_999)
        );
    }

    #[test]
    fn timings_describe_and_are_due() {
        assert_eq!(
            Timing::Every(Duration::from_secs(300)).describe(),
            "every 5m"
        );
        assert_eq!(
            Timing::Every(Duration::from_secs(30)).describe(),
            "every 30s"
        );
        assert_eq!(
            Timing::Every(Duration::from_secs(7200)).describe(),
            "every 2h"
        );
        let five = Timing::Every(Duration::from_secs(300));
        assert!(five.due_in_minute(at(2026, 10, 3, 3, 15)));
        assert!(!five.due_in_minute(at(2026, 10, 3, 3, 16)));
        assert!(Timing::Every(Duration::from_secs(10)).due_in_minute(at(2026, 10, 3, 3, 16)));
    }

    #[test]
    fn builder_records_errors() {
        let mut entries = Vec::new();
        ScheduleBuilder {
            entries: &mut entries,
        }
        .agent("a")
        .daily_at("25:00");
        ScheduleBuilder {
            entries: &mut entries,
        }
        .agent("b");
        assert!(
            validate(&entries)
                .unwrap_err()
                .to_string()
                .contains("HH:MM")
        );
        entries.remove(0);
        assert!(
            validate(&entries)
                .unwrap_err()
                .to_string()
                .contains("no timing")
        );
        let mut entries = Vec::new();
        ScheduleBuilder {
            entries: &mut entries,
        }
        .agent("a")
        .daily_at("03:07");
        ScheduleBuilder {
            entries: &mut entries,
        }
        .agent("a")
        .hourly();
        assert!(matches!(validate(&entries), Err(Error::Duplicate { .. })));
        assert_eq!(
            entries[0].timing,
            Some(Timing::Cron(Cron::parse("7 3 * * *").unwrap()))
        );
    }
}
