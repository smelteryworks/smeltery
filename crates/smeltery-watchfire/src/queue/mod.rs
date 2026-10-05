//! Jobs and the queue: short, finite tasks dispatched from the app and handled by the worker
//! pool `queue#0..n`.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt as _;
use serde::Serialize;
use serde::de::DeserializeOwned;
use smeltery_core::{App, BoxFuture};
use tokio::sync::Notify;

use crate::agent::Agent;
use crate::ctx::AgentCtx;
use crate::error::{AgentError, Error, StoreError};
use crate::status::{RunOutcome, RunRecord};
use crate::time::{Clock, duration_ms};

pub(crate) mod db;
#[cfg(feature = "redis")]
pub(crate) mod redis;

/// The name of the `redis` driver's queue in its keys (Watchfire has one queue).
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
pub(crate) const REDIS_QUEUE: &str = "default";

/// The start of every key of the `redis` driver's `queue` under `prefix` (`QUEUE_PREFIX`): `<prefix>{<queue>}:`.
/// `{<queue>}` is a hash tag: every key of the queue is in one hash slot.
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
pub(crate) fn redis_key_base(prefix: &str, queue: &str) -> String {
    format!("{prefix}{{{queue}}}:")
}

/// A queued job's id.
pub type JobId = i64;

/// A background job: a serializable value whose `handle` runs on a queue worker.
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use smeltery_watchfire::prelude::*;
///
/// #[derive(Serialize, Deserialize)]
/// pub struct SendWelcome {
///     pub user_id: i64,
/// }
///
/// impl Job for SendWelcome {
///     const NAME: &'static str = "send_welcome";
///
///     async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
///         ctx.log().info(format!("welcoming user {}", self.user_id));
///         Ok(())
///     }
/// }
///
/// # async fn demo(app: smeltery_core::App) -> smeltery_core::Result<()> {
/// SendWelcome { user_id: 1 }.dispatch(&app).await?;
/// SendWelcome { user_id: 2 }.dispatch_later(&app, 10.mins()).await?;
/// # Ok(())
/// # }
/// ```
pub trait Job: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// The name stored with each queued job; unique per app.
    const NAME: &'static str;

    /// How many attempts before the job goes to the dead letters (default 3).
    fn max_attempts(&self) -> u32 {
        3
    }

    /// The delay before retry after failed attempt number `attempt` (1-based): 5 s doubling
    /// per attempt, at most 10 minutes.
    fn backoff(&self, attempt: u32) -> Duration {
        let factor = 2_u32.saturating_pow(attempt.saturating_sub(1).min(16));
        Duration::from_secs(5)
            .saturating_mul(factor)
            .min(Duration::from_secs(600))
    }

    /// Do the work.
    fn handle(&self, ctx: JobCtx) -> impl Future<Output = Result<(), AgentError>> + Send;

    /// Queue this job for the workers.
    ///
    /// # Errors
    /// Watchfire is not set up on the app (`.agents(...)`), the job does not serialize, or the
    /// queue fails.
    fn dispatch(&self, app: &App) -> impl Future<Output = smeltery_core::Result<JobId>> + Send
    where
        Self: Sized,
    {
        async move { Ok(push(app, self, Duration::ZERO).await?) }
    }

    /// Queue this job to run after `delay`.
    ///
    /// # Errors
    /// See [`Job::dispatch`].
    fn dispatch_later(
        &self,
        app: &App,
        delay: Duration,
    ) -> impl Future<Output = smeltery_core::Result<JobId>> + Send
    where
        Self: Sized,
    {
        async move { Ok(push(app, self, delay).await?) }
    }
}

/// What a job's `handle` receives: the worker's [`AgentCtx`] (through `Deref`: `log()`,
/// `http()`, `db()`, `app()`, `counter()` …) plus the job's id and attempt.
#[derive(Clone, Debug)]
pub struct JobCtx {
    ctx: AgentCtx,
    job: String,
    id: JobId,
    attempt: u32,
}

impl JobCtx {
    pub(crate) fn new(ctx: AgentCtx, job: &str, id: JobId, attempt: u32) -> Self {
        Self {
            ctx,
            job: job.to_owned(),
            id,
            attempt,
        }
    }

    /// The job name.
    pub fn job(&self) -> &str {
        &self.job
    }

    /// The queued job's id.
    pub fn id(&self) -> JobId {
        self.id
    }

    /// This attempt, counting from 1.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The worker's context.
    pub fn agent(&self) -> &AgentCtx {
        &self.ctx
    }
}

impl std::ops::Deref for JobCtx {
    type Target = AgentCtx;

    fn deref(&self) -> &AgentCtx {
        &self.ctx
    }
}

/// A job that failed for good.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct DeadLetter {
    /// Row id.
    pub id: i64,
    /// The job name.
    pub job: String,
    /// The payload (JSON).
    pub payload: String,
    /// The last error.
    pub error: String,
    /// Attempts made.
    pub attempts: u32,
    /// When it was given up, Unix milliseconds.
    pub failed_at_ms: i64,
}

/// Queue counts, for the API and dashboards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct QueueStats {
    /// Waiting (including delayed) jobs.
    pub pending: u64,
    /// Jobs a worker holds.
    pub reserved: u64,
    /// Dead letters.
    pub dead: u64,
}

/// A job a worker reserved.
///
/// `reserved_at` and `attempts` together are the reservation's token: a reservation released as stale and taken
/// again has another `reserved_at` and one more attempt, so `delete` / `retry` / `release` / `dead_letter` of the
/// earlier holder change nothing (they answer `false`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reserved {
    pub(crate) id: JobId,
    pub(crate) job: String,
    /// The payload; empty when it was too large to read (`oversized`).
    pub(crate) payload: String,
    /// Including this attempt.
    pub(crate) attempts: u32,
    /// When this reservation was made (the `now` given to `reserve`).
    pub(crate) reserved_at: i64,
    /// The payload's size in bytes when it is larger than the limit given to `reserve` (it was not read).
    pub(crate) oversized: Option<u64>,
}

type QResult<T> = Result<T, StoreError>;

/// Where queued jobs live.
pub(crate) trait Driver: Send + Sync + 'static {
    fn push<'a>(
        &'a self,
        job: &'a str,
        payload: &'a str,
        available_at: i64,
        now: i64,
    ) -> BoxFuture<'a, QResult<JobId>>;
    /// Atomically take the oldest available job, counting the attempt. A payload of more than `max_payload` bytes
    /// is not read: the job comes back with an empty payload and `oversized` set.
    fn reserve(&self, now: i64, max_payload: u64) -> BoxFuture<'_, QResult<Option<Reserved>>>;
    // `delete`, `retry`, `release` and `dead_letter` act only while `job`'s reservation is still the one held
    // (its `reserved_at` and `attempts`); otherwise they change nothing and answer `false`.
    fn delete<'a>(&'a self, job: &'a Reserved) -> BoxFuture<'a, QResult<bool>>;
    fn retry<'a>(&'a self, job: &'a Reserved, available_at: i64) -> BoxFuture<'a, QResult<bool>>;
    /// Give a job back without counting the attempt (shutdown).
    fn release<'a>(&'a self, job: &'a Reserved) -> BoxFuture<'a, QResult<bool>>;
    /// Move the job to the dead letters (its stored job name, payload and attempts) with `error`.
    fn dead_letter<'a>(
        &'a self,
        job: &'a Reserved,
        error: &'a str,
        now: i64,
    ) -> BoxFuture<'a, QResult<bool>>;
    /// Free reservations made before `before` (their worker died); the attempt still counts.
    fn release_stale(&self, before: i64) -> BoxFuture<'_, QResult<u64>>;
    fn stats(&self) -> BoxFuture<'_, QResult<QueueStats>>;
    fn dead_letters(&self, limit: u32) -> BoxFuture<'_, QResult<Vec<DeadLetter>>>;
    /// Remove a dead letter and return it (`None` when there is no such row, or another caller
    /// took it first).
    fn take_dead(&self, id: i64) -> BoxFuture<'_, QResult<Option<DeadLetter>>>;
    /// Move a dead letter back to the queue (attempts start over) in one step: on failure it stays a
    /// dead letter. `None` when there is no such row, or another caller took it first.
    fn requeue_dead(&self, id: i64, now: i64) -> BoxFuture<'_, QResult<Option<JobId>>>;
}

/// The longest error text stored with a run, an agent or a dead letter, in bytes; longer text is cut at a
/// character boundary and marked (an upstream error body can be huge, and MySQL's `TEXT` holds 64 KiB).
pub(crate) const MAX_STORED_ERROR: usize = 8 * 1024;

/// `text` cut to [`MAX_STORED_ERROR`] bytes, with a marker saying how long it was.
pub(crate) fn stored_error(text: &str) -> String {
    if text.len() <= MAX_STORED_ERROR {
        return text.to_owned();
    }
    let mut end = MAX_STORED_ERROR;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated, {} bytes)", &text[..end], text.len())
}

/// The default of `WATCHFIRE_MAX_PAYLOAD`: the largest job payload, in bytes (1 MiB).
pub(crate) const DEFAULT_MAX_PAYLOAD: u64 = 1024 * 1024;

/// The most dead letters the memory driver keeps; the oldest go first.
pub(crate) const MEMORY_DEAD_LETTERS: usize = 1000;

/// The app's queue: the driver, a wake-up for local workers and the clock. A service on the
/// app once Watchfire is set up (`.agents(...)`).
#[derive(Clone)]
pub struct Queue {
    pub(crate) driver: Arc<dyn Driver>,
    pub(crate) notify: Arc<Notify>,
    pub(crate) clock: Clock,
    pub(crate) timeout: Duration,
    /// The largest payload `push_raw` accepts and a worker reads (`WATCHFIRE_MAX_PAYLOAD`), in bytes.
    pub(crate) max_payload: u64,
    driver_name: &'static str,
}

impl std::fmt::Debug for Queue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Queue")
            .field("driver", &self.driver_name)
            .finish_non_exhaustive()
    }
}

impl Queue {
    pub(crate) fn memory(clock: Clock, timeout: Duration) -> Self {
        Self::with_driver(Arc::new(MemoryDriver::default()), "memory", clock, timeout)
    }

    pub(crate) fn database(db: smeltery_core::db::Db, clock: Clock, timeout: Duration) -> Self {
        Self::with_driver(Arc::new(db::DbDriver::new(db)), "database", clock, timeout)
    }

    /// The Redis queue on the server at `url` with its keys under `prefix`; connects on first use.
    #[cfg(feature = "redis")]
    pub(crate) fn redis(
        url: &str,
        prefix: &str,
        clock: Clock,
        timeout: Duration,
    ) -> Result<Self, Error> {
        let driver = redis::RedisDriver::new(url, prefix, timeout).map_err(Error::Config)?;
        Ok(Self::with_driver(Arc::new(driver), "redis", clock, timeout))
    }

    fn with_driver(
        driver: Arc<dyn Driver>,
        name: &'static str,
        clock: Clock,
        timeout: Duration,
    ) -> Self {
        Self {
            driver,
            notify: Arc::new(Notify::new()),
            clock,
            timeout,
            max_payload: DEFAULT_MAX_PAYLOAD,
            driver_name: name,
        }
    }

    /// The same queue with `bytes` as its payload limit (at least 1).
    #[must_use]
    pub(crate) fn with_max_payload(mut self, bytes: u64) -> Self {
        self.max_payload = bytes.max(1);
        self
    }

    /// `database`, `redis` or `memory`.
    pub fn driver(&self) -> &'static str {
        self.driver_name
    }

    pub(crate) async fn timed<T>(
        &self,
        op: &'static str,
        fut: impl Future<Output = QResult<T>>,
    ) -> QResult<T> {
        tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| StoreError::new(op, format!("timed out after {:?}", self.timeout)))?
    }

    /// Queue a job by name with a JSON payload (what `Job::dispatch` does after serializing).
    ///
    /// # Errors
    /// The payload is larger than `WATCHFIRE_MAX_PAYLOAD` (nothing is queued), or the queue fails or times out.
    pub async fn push_raw(
        &self,
        job: &str,
        payload: &str,
        delay: Duration,
    ) -> Result<JobId, Error> {
        let size = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        if size > self.max_payload {
            return Err(Error::Config(format!(
                "the payload of job `{job}` has {size} bytes, more than WATCHFIRE_MAX_PAYLOAD ({} bytes)",
                self.max_payload
            )));
        }
        let now = self.clock.now_ms();
        let at = now.saturating_add(duration_ms(delay));
        let id = self
            .timed("push", self.driver.push(job, payload, at, now))
            .await?;
        self.notify.notify_one();
        Ok(id)
    }

    /// Pending, reserved and dead counts.
    ///
    /// # Errors
    /// The queue fails or times out.
    pub async fn stats(&self) -> Result<QueueStats, Error> {
        Ok(self.timed("stats", self.driver.stats()).await?)
    }

    /// The latest dead letters, newest first.
    ///
    /// # Errors
    /// The queue fails or times out.
    pub async fn dead_letters(&self, limit: u32) -> Result<Vec<DeadLetter>, Error> {
        Ok(self
            .timed("dead_letters", self.driver.dead_letters(limit))
            .await?)
    }

    /// Queue a dead letter again (attempts start over); returns the new job id, `None` when
    /// there is no such dead letter.
    ///
    /// # Errors
    /// The queue fails or times out.
    pub async fn retry_dead(&self, id: i64) -> Result<Option<JobId>, Error> {
        let now = self.clock.now_ms();
        let id = self
            .timed("requeue_dead", self.driver.requeue_dead(id, now))
            .await?;
        if id.is_some() {
            self.notify.notify_one();
        }
        Ok(id)
    }

    /// Delete a dead letter; `false` when there is no such dead letter.
    ///
    /// # Errors
    /// The queue fails or times out.
    pub async fn delete_dead(&self, id: i64) -> Result<bool, Error> {
        Ok(self
            .timed("take_dead", self.driver.take_dead(id))
            .await?
            .is_some())
    }
}

/// Serialize and queue `job` on the app's queue.
pub(crate) async fn push<J: Job>(app: &App, job: &J, delay: Duration) -> Result<JobId, Error> {
    let queue = app.service::<Queue>().ok_or_else(|| {
        Error::Config(
            "Watchfire is not set up on this app: register agents and jobs with `.agents(...)`"
                .to_owned(),
        )
    })?;
    let payload = serde_json::to_string(job)
        .map_err(|e| Error::Config(format!("job `{}` does not serialize: {e}", J::NAME)))?;
    queue.push_raw(J::NAME, &payload, delay).await
}

type HandlerFuture = BoxFuture<'static, Result<(), AgentError>>;

/// A decoded job ready to run.
pub(crate) struct Decoded {
    pub(crate) max_attempts: u32,
    pub(crate) backoff: Duration,
    pub(crate) run: HandlerFuture,
}

type Handler = Arc<dyn Fn(&str, JobCtx) -> Result<Decoded, AgentError> + Send + Sync>;

/// The registered job types, by name.
#[derive(Clone, Default)]
pub(crate) struct JobRegistry {
    handlers: HashMap<&'static str, Handler>,
}

impl std::fmt::Debug for JobRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.handlers.keys()).finish()
    }
}

impl JobRegistry {
    /// `false` when the name is taken.
    pub(crate) fn add<J: Job>(&mut self) -> bool {
        if self.handlers.contains_key(J::NAME) {
            return false;
        }
        let handler: Handler = Arc::new(|payload: &str, ctx: JobCtx| {
            let job: J = decode_payload(payload)?;
            let attempt = ctx.attempt();
            Ok(Decoded {
                max_attempts: job.max_attempts().max(1),
                backoff: job.backoff(attempt),
                run: Box::pin(async move { job.handle(ctx).await }),
            })
        });
        self.handlers.insert(J::NAME, handler);
        true
    }

    pub(crate) fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.handlers.keys().copied().collect();
        names.sort_unstable();
        names
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    pub(crate) fn decode(
        &self,
        job: &str,
        payload: &str,
        ctx: JobCtx,
    ) -> Option<Result<Decoded, AgentError>> {
        self.handlers.get(job).map(|h| h(payload, ctx))
    }
}

/// Decode a job's payload. The error names the kind of problem and where it is, never a value of the payload: it
/// goes to the run history, the dead letter's error and alerts (webhook, mail), and the payload may hold personal
/// data (serde_json's own messages quote values, e.g. `invalid type: string "…"`).
pub(crate) fn decode_payload<J: serde::de::DeserializeOwned>(
    payload: &str,
) -> Result<J, AgentError> {
    serde_json::from_str(payload).map_err(|e| {
        let kind = match e.classify() {
            serde_json::error::Category::Io => "an I/O error",
            serde_json::error::Category::Syntax => "invalid JSON",
            serde_json::error::Category::Data => "JSON that does not fit the job's type",
            serde_json::error::Category::Eof => "JSON that ends too early",
        };
        AgentError::msg(format!(
            "the payload is {kind} (line {}, column {})",
            e.line(),
            e.column()
        ))
    })
}

/// The in-process driver: nothing survives the process.
#[derive(Debug, Default)]
pub(crate) struct MemoryDriver {
    inner: Mutex<MemoryQueue>,
}

#[derive(Debug, Default)]
struct MemoryQueue {
    next_id: JobId,
    jobs: BTreeMap<JobId, MemJob>,
    dead: Vec<DeadLetter>,
    next_dead: i64,
}

#[derive(Debug)]
struct MemJob {
    job: String,
    payload: String,
    attempts: u32,
    available_at: i64,
    reserved_at: Option<i64>,
}

impl MemoryDriver {
    fn with<T>(&self, f: impl FnOnce(&mut MemoryQueue) -> T) -> T {
        f(&mut self.inner.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl MemoryQueue {
    /// The job of `held` while its reservation is still the one held.
    fn held(&mut self, held: &Reserved) -> Option<&mut MemJob> {
        self.jobs
            .get_mut(&held.id)
            .filter(|j| j.reserved_at == Some(held.reserved_at) && j.attempts == held.attempts)
    }
}

fn ready<'a, T: Send + 'a>(value: T) -> BoxFuture<'a, QResult<T>> {
    Box::pin(async move { Ok(value) })
}

impl Driver for MemoryDriver {
    fn push<'a>(
        &'a self,
        job: &'a str,
        payload: &'a str,
        available_at: i64,
        _now: i64,
    ) -> BoxFuture<'a, QResult<JobId>> {
        let id = self.with(|q| {
            q.next_id += 1;
            q.jobs.insert(
                q.next_id,
                MemJob {
                    job: job.to_owned(),
                    payload: payload.to_owned(),
                    attempts: 0,
                    available_at,
                    reserved_at: None,
                },
            );
            q.next_id
        });
        ready(id)
    }

    fn reserve(&self, now: i64, max_payload: u64) -> BoxFuture<'_, QResult<Option<Reserved>>> {
        let found = self.with(|q| {
            let (id, job) = q
                .jobs
                .iter_mut()
                .filter(|(_, j)| j.reserved_at.is_none() && j.available_at <= now)
                .min_by_key(|(id, j)| (j.available_at, **id))?;
            job.reserved_at = Some(now);
            job.attempts += 1;
            let size = u64::try_from(job.payload.len()).unwrap_or(u64::MAX);
            let oversized = (size > max_payload).then_some(size);
            Some(Reserved {
                id: *id,
                job: job.job.clone(),
                payload: if oversized.is_some() {
                    String::new()
                } else {
                    job.payload.clone()
                },
                attempts: job.attempts,
                reserved_at: now,
                oversized,
            })
        });
        ready(found)
    }

    fn delete<'a>(&'a self, job: &'a Reserved) -> BoxFuture<'a, QResult<bool>> {
        let done = self.with(|q| q.held(job).is_some() && q.jobs.remove(&job.id).is_some());
        ready(done)
    }

    fn retry<'a>(&'a self, job: &'a Reserved, available_at: i64) -> BoxFuture<'a, QResult<bool>> {
        let done = self.with(|q| {
            q.held(job).map(|j| {
                j.reserved_at = None;
                j.available_at = available_at;
            })
        });
        ready(done.is_some())
    }

    fn release<'a>(&'a self, job: &'a Reserved) -> BoxFuture<'a, QResult<bool>> {
        let done = self.with(|q| {
            q.held(job).map(|j| {
                j.reserved_at = None;
                j.attempts = j.attempts.saturating_sub(1);
            })
        });
        ready(done.is_some())
    }

    fn dead_letter<'a>(
        &'a self,
        job: &'a Reserved,
        error: &'a str,
        now: i64,
    ) -> BoxFuture<'a, QResult<bool>> {
        let done = self.with(|q| {
            if q.held(job).is_none() {
                return false;
            }
            let Some(stored) = q.jobs.remove(&job.id) else {
                return false;
            };
            q.next_dead += 1;
            let id = q.next_dead;
            q.dead.push(DeadLetter {
                id,
                job: stored.job,
                payload: stored.payload,
                error: stored_error(error),
                attempts: stored.attempts,
                failed_at_ms: now,
            });
            if q.dead.len() > MEMORY_DEAD_LETTERS {
                let dropped = q.dead.remove(0);
                tracing::warn!(id = dropped.id, job = %dropped.job, "the memory queue keeps {MEMORY_DEAD_LETTERS} dead letters; the oldest was dropped");
            }
            true
        });
        ready(done)
    }

    fn release_stale(&self, before: i64) -> BoxFuture<'_, QResult<u64>> {
        let n = self.with(|q| {
            let mut n = 0;
            for job in q.jobs.values_mut() {
                if job.reserved_at.is_some_and(|at| at < before) {
                    job.reserved_at = None;
                    n += 1;
                }
            }
            n
        });
        ready(n)
    }

    fn stats(&self) -> BoxFuture<'_, QResult<QueueStats>> {
        let stats = self.with(|q| {
            let reserved = q.jobs.values().filter(|j| j.reserved_at.is_some()).count();
            QueueStats {
                pending: (q.jobs.len() - reserved) as u64,
                reserved: reserved as u64,
                dead: q.dead.len() as u64,
            }
        });
        ready(stats)
    }

    fn dead_letters(&self, limit: u32) -> BoxFuture<'_, QResult<Vec<DeadLetter>>> {
        let dead = self.with(|q| {
            q.dead
                .iter()
                .rev()
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .cloned()
                .collect()
        });
        ready(dead)
    }

    fn take_dead(&self, id: i64) -> BoxFuture<'_, QResult<Option<DeadLetter>>> {
        let taken = self.with(|q| {
            let index = q.dead.iter().position(|d| d.id == id)?;
            Some(q.dead.remove(index))
        });
        ready(taken)
    }

    fn requeue_dead(&self, id: i64, now: i64) -> BoxFuture<'_, QResult<Option<JobId>>> {
        let queued = self.with(|q| {
            let index = q.dead.iter().position(|d| d.id == id)?;
            let dead = q.dead.remove(index);
            q.next_id += 1;
            q.jobs.insert(
                q.next_id,
                MemJob {
                    job: dead.job,
                    payload: dead.payload,
                    attempts: 0,
                    available_at: now,
                    reserved_at: None,
                },
            );
            Some(q.next_id)
        });
        ready(queued)
    }
}

/// A queue worker: one member of the `queue` pool.
pub(crate) struct Worker {
    pub(crate) name: String,
}

/// How often an idle worker looks for due jobs (local dispatches wake it at once).
const POLL: Duration = Duration::from_secs(1);

impl Agent for Worker {
    fn name(&self) -> String {
        self.name.clone()
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        let shared = Arc::clone(ctx.shared());
        let Some(queue) = shared.queue.clone() else {
            ctx.cancelled().await;
            return Ok(());
        };
        let stale_every = queue.timeout.max(shared.job_timeout);
        let mut last_stale = None::<tokio::time::Instant>;
        while !ctx.is_cancelled() {
            ctx.heartbeat();
            if last_stale.is_none_or(|t| t.elapsed() >= stale_every) {
                last_stale = Some(tokio::time::Instant::now());
                let before = queue
                    .clock
                    .now_ms()
                    .saturating_sub(duration_ms(shared.job_timeout.saturating_mul(2)));
                match queue
                    .timed("release_stale", queue.driver.release_stale(before))
                    .await
                {
                    Ok(0) => {}
                    Ok(n) => tracing::warn!(jobs = n, "released stale job reservations"),
                    Err(e) => tracing::warn!(error = %e, "cannot release stale reservations"),
                }
            }
            let now = queue.clock.now_ms();
            match queue
                .timed("reserve", queue.driver.reserve(now, queue.max_payload))
                .await
            {
                Ok(Some(job)) => handle(&ctx, &queue, job).await,
                Ok(None) => {
                    tokio::select! {
                        biased;
                        () = ctx.cancelled() => break,
                        () = queue.notify.notified() => {}
                        () = tokio::time::sleep(POLL) => {}
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cannot reserve a job");
                    if !ctx.sleep(POLL).await {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Run one reserved job and record it as a run of the worker.
async fn handle(ctx: &AgentCtx, queue: &Queue, job: Reserved) {
    let shared = Arc::clone(ctx.shared());
    let run_id = ctx.next_run_id();
    let mut record = RunRecord::started(
        ctx.name(),
        run_id,
        Some(job.job.clone()),
        queue.clock.now_ms(),
    );
    shared.persist_run(&record).await;
    ctx.persist_status().await;
    ctx.reset_counters();
    let job_ctx = JobCtx::new(ctx.clone(), &job.job, job.id, job.attempts);

    let decoded = match job.oversized {
        // Too large to read: never decoded or run (WATCHFIRE_MAX_PAYLOAD); the dead letter keeps the stored payload.
        Some(size) => Err(format!(
            "the payload has {size} bytes, more than WATCHFIRE_MAX_PAYLOAD ({} bytes)",
            queue.max_payload
        )),
        None => match shared.jobs.decode(&job.job, &job.payload, job_ctx) {
            None => Err(format!("unknown job `{}`", job.job)),
            Some(Err(e)) => Err(format!("cannot decode job `{}`: {e}", job.job)),
            Some(Ok(decoded)) => Ok(decoded),
        },
    };
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(error) => {
            shared.alert(
                crate::alert::AlertKind::DeadLetter,
                ctx.name(),
                Some(&job.job),
                &format!("job #{} dead-lettered: {}", job.id, stored_error(&error)),
            );
            let stored = queue
                .timed(
                    "dead_letter",
                    queue.driver.dead_letter(&job, &error, queue.clock.now_ms()),
                )
                .await;
            outcome_stored(stored, &job, "dead_letter");
            finish(ctx, queue, &mut record, RunOutcome::Failed, Some(error)).await;
            return;
        }
    };

    // More attempts than allowed: an earlier attempt failed for good but could not be dead-lettered (or its worker
    // died). Running it again would repeat its side effects; dead-letter it now.
    if job.attempts > decoded.max_attempts {
        let allowed = decoded.max_attempts;
        drop(decoded);
        let error = format!(
            "attempt {} of at most {allowed}: an earlier attempt ended without its outcome being stored (its worker \
             stopped, or the dead letter could not be written); dead-lettered without running again",
            job.attempts
        );
        give_up(ctx, queue, &job, &mut record, error).await;
        return;
    }
    let timeout = shared.job_timeout;
    let mut fut =
        Box::pin(AssertUnwindSafe(tokio::time::timeout(timeout, decoded.run)).catch_unwind());
    let result = tokio::select! {
        biased;
        result = &mut fut => Some(result),
        () = ctx.cancelled() => None,
    };
    let result = match result {
        Some(result) => Some(result),
        None => {
            // Shutdown or stop: give the job its grace, then hand it back to the queue.
            let grace = shared.grace(shared.worker_shutdown_timeout);
            tokio::time::timeout(grace, &mut fut).await.ok()
        }
    };
    drop(fut);
    let (outcome, error) = match result {
        None => {
            let released = queue.timed("release", queue.driver.release(&job)).await;
            outcome_stored(released, &job, "release");
            finish(
                ctx,
                queue,
                &mut record,
                RunOutcome::Killed,
                Some("did not finish before shutdown; released to the queue".to_owned()),
            )
            .await;
            return;
        }
        Some(Ok(Ok(Ok(())))) => (RunOutcome::Completed, None),
        Some(Ok(Ok(Err(e)))) => (RunOutcome::Failed, Some(stored_error(&e.to_string()))),
        Some(Ok(Err(_))) => (
            RunOutcome::Failed,
            Some(format!("timed out after {timeout:?}")),
        ),
        Some(Err(panic)) => (
            RunOutcome::Panicked,
            Some(crate::runtime::panic_message(&*panic)),
        ),
    };
    let (op, name) = match (&error, job.attempts >= decoded.max_attempts) {
        (None, _) => (
            queue.timed("delete", queue.driver.delete(&job)).await,
            "delete",
        ),
        (Some(e), true) => {
            shared.alert(
                crate::alert::AlertKind::DeadLetter,
                ctx.name(),
                Some(&job.job),
                &format!(
                    "job `{}` #{} failed after {} attempts: {e}",
                    job.job, job.id, job.attempts
                ),
            );
            (
                queue
                    .timed(
                        "dead_letter",
                        queue.driver.dead_letter(&job, e, queue.clock.now_ms()),
                    )
                    .await,
                "dead_letter",
            )
        }
        (Some(e), false) => {
            tracing::warn!(job = %job.job, id = job.id, attempt = job.attempts, error = %e, "job failed; retrying");
            let at = queue
                .clock
                .now_ms()
                .saturating_add(duration_ms(decoded.backoff));
            (
                queue.timed("retry", queue.driver.retry(&job, at)).await,
                "retry",
            )
        }
    };
    outcome_stored(op, &job, name);
    finish(ctx, queue, &mut record, outcome, error).await;
}

/// Dead-letter `job` without running it (again) and record the run as failed.
async fn give_up(
    ctx: &AgentCtx,
    queue: &Queue,
    job: &Reserved,
    record: &mut RunRecord,
    error: String,
) {
    let error = stored_error(&error);
    ctx.shared().alert(
        crate::alert::AlertKind::DeadLetter,
        ctx.name(),
        Some(&job.job),
        &format!("job `{}` #{} dead-lettered: {error}", job.job, job.id),
    );
    let stored = queue
        .timed(
            "dead_letter",
            queue.driver.dead_letter(job, &error, queue.clock.now_ms()),
        )
        .await;
    outcome_stored(stored, job, "dead_letter");
    finish(ctx, queue, record, RunOutcome::Failed, Some(error)).await;
}

/// Log a job outcome the queue did not store: an error, or a reservation that is no longer this worker's (it was
/// released as stale and another worker holds the job now, so that worker's outcome counts).
fn outcome_stored(result: QResult<bool>, job: &Reserved, op: &'static str) {
    match result {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            id = job.id,
            job = %job.job,
            op,
            "the job's reservation was taken over (it ran past the stale cut-off, twice WATCHFIRE_JOB_TIMEOUT);              this outcome was not stored"
        ),
        Err(e) => tracing::warn!(error = %e, id = job.id, op, "cannot update the job"),
    }
}

async fn finish(
    ctx: &AgentCtx,
    queue: &Queue,
    record: &mut RunRecord,
    outcome: RunOutcome,
    error: Option<String>,
) {
    record.ended_at_ms = Some(queue.clock.now_ms());
    record.outcome = outcome;
    record.error = error.map(|e| stored_error(&e));
    record.counters = ctx.reset_counters();
    ctx.shared().persist_run(record).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reservation of job `id` this caller does not hold (never made, or made by someone else).
    pub(crate) fn not_held(id: JobId) -> Reserved {
        Reserved {
            id,
            job: "x".to_owned(),
            payload: String::new(),
            attempts: 0,
            reserved_at: -1,
            oversized: None,
        }
    }

    /// No limit worth the name, for tests that do not test it.
    pub(crate) const ANY: u64 = u64::MAX;

    pub(crate) async fn driver_contract(driver: &dyn Driver) {
        let a = driver.push("a", "{}", 100, 0).await.unwrap();
        let b = driver.push("b", "{\"x\":1}", 50, 0).await.unwrap();
        assert!(
            driver.reserve(10, ANY).await.unwrap().is_none(),
            "nothing due yet"
        );
        let first = driver.reserve(100, ANY).await.unwrap().unwrap();
        assert_eq!((first.id, first.job.as_str(), first.attempts), (b, "b", 1));
        assert_eq!((first.reserved_at, first.oversized), (100, None));
        let second = driver.reserve(100, ANY).await.unwrap().unwrap();
        assert_eq!(second.id, a);
        assert!(
            driver.reserve(100, ANY).await.unwrap().is_none(),
            "both reserved"
        );
        assert_eq!(
            driver.stats().await.unwrap(),
            QueueStats {
                pending: 0,
                reserved: 2,
                dead: 0
            }
        );
        assert!(driver.retry(&first, 200).await.unwrap());
        assert!(driver.reserve(150, ANY).await.unwrap().is_none());
        let again = driver.reserve(200, ANY).await.unwrap().unwrap();
        assert_eq!((again.id, again.attempts), (b, 2));
        // The first reservation is over: retrying, releasing or deleting with it changes nothing.
        assert!(!driver.retry(&first, 900).await.unwrap());
        assert!(!driver.release(&first).await.unwrap());
        assert!(!driver.delete(&first).await.unwrap());
        assert!(driver.release(&again).await.unwrap());
        let released = driver.reserve(200, ANY).await.unwrap().unwrap();
        assert_eq!(
            (released.id, released.attempts),
            (b, 2),
            "release does not count"
        );
        assert!(driver.dead_letter(&released, "boom", 300).await.unwrap());
        // `a` is still reserved since 100: stale before 150.
        assert_eq!(driver.release_stale(50).await.unwrap(), 0);
        assert_eq!(driver.release_stale(150).await.unwrap(), 1);
        let a_again = driver.reserve(400, ANY).await.unwrap().unwrap();
        assert_eq!((a_again.id, a_again.attempts), (a, 2));
        assert!(driver.delete(&a_again).await.unwrap());
        assert_eq!(
            driver.stats().await.unwrap(),
            QueueStats {
                pending: 0,
                reserved: 0,
                dead: 1
            }
        );
        let dead = driver.dead_letters(10).await.unwrap();
        assert_eq!(dead.len(), 1);
        assert_eq!(
            (
                dead[0].job.as_str(),
                dead[0].error.as_str(),
                dead[0].attempts
            ),
            ("b", "boom", 2)
        );
        assert_eq!(dead[0].payload, "{\"x\":1}");
        let taken = driver.take_dead(dead[0].id).await.unwrap().unwrap();
        assert_eq!(taken, dead[0]);
        assert!(driver.take_dead(dead[0].id).await.unwrap().is_none());
        // A job that is gone or not held: nothing is dead-lettered, deleted, retried or released.
        assert!(!driver.dead_letter(&not_held(b), "boom", 300).await.unwrap());
        let c = driver.push("c", "{}", 0, 0).await.unwrap();
        assert!(!driver.release(&not_held(c)).await.unwrap());
        assert!(!driver.dead_letter(&not_held(c), "boom", 1).await.unwrap());
        assert!(!driver.delete(&not_held(c)).await.unwrap());
        assert!(driver.dead_letters(10).await.unwrap().is_empty());
        let got = driver.reserve(0, ANY).await.unwrap().unwrap();
        assert_eq!(
            (got.id, got.attempts),
            (c, 1),
            "an unheld release changed nothing"
        );
        assert!(driver.delete(&got).await.unwrap());
        stale_holders_change_nothing(driver).await;
        oversized_payloads_are_not_read(driver).await;
    }

    /// Sweep W7-02: a reservation released as stale and taken by a second worker. The first worker's late outcome
    /// (delete, retry, release, dead letter) changes nothing; the second worker's counts.
    async fn stale_holders_change_nothing(driver: &dyn Driver) {
        let id = driver.push("s", "{\"n\":1}", 1_000, 1_000).await.unwrap();
        let first = driver.reserve(1_000, ANY).await.unwrap().unwrap();
        assert_eq!(first.id, id);
        assert_eq!(driver.release_stale(1_001).await.unwrap(), 1);
        let second = driver.reserve(5_000, ANY).await.unwrap().unwrap();
        assert_eq!((second.id, second.attempts), (id, 2));
        assert!(!driver.delete(&first).await.unwrap());
        assert!(!driver.retry(&first, 1_000).await.unwrap());
        assert!(!driver.release(&first).await.unwrap());
        assert!(!driver.dead_letter(&first, "late", 5_001).await.unwrap());
        assert_eq!(
            driver.stats().await.unwrap(),
            QueueStats {
                pending: 0,
                reserved: 1,
                dead: 0
            },
            "the second worker still holds the job"
        );
        assert!(
            driver.reserve(i64::MAX, ANY).await.unwrap().is_none(),
            "nobody else can take it"
        );
        assert!(driver.delete(&second).await.unwrap());
        assert_eq!(driver.stats().await.unwrap(), QueueStats::default());
    }

    /// Sweep W7-03: a payload over the limit is not read; its dead letter keeps the stored payload.
    async fn oversized_payloads_are_not_read(driver: &dyn Driver) {
        let big = format!("\"{}\"", "x".repeat(100));
        let id = driver.push("big", &big, 0, 0).await.unwrap();
        let job = driver.reserve(0, 50).await.unwrap().unwrap();
        assert_eq!(
            (job.id, job.payload.as_str(), job.oversized),
            (id, "", Some(102))
        );
        assert!(driver.dead_letter(&job, "too large", 1).await.unwrap());
        let dead = driver.dead_letters(1).await.unwrap();
        assert_eq!(
            (
                dead[0].job.as_str(),
                dead[0].payload.as_str(),
                dead[0].attempts
            ),
            ("big", big.as_str(), 1)
        );
        assert!(driver.take_dead(dead[0].id).await.unwrap().is_some());
        let small = driver.push("small", "{}", 0, 0).await.unwrap();
        let job = driver.reserve(0, 2).await.unwrap().unwrap();
        assert_eq!(
            (job.id, job.payload.as_str(), job.oversized),
            (small, "{}", None)
        );
        assert!(driver.delete(&job).await.unwrap());
    }

    #[tokio::test]
    async fn memory_driver_contract() {
        driver_contract(&MemoryDriver::default()).await;
    }

    #[tokio::test]
    async fn db_driver_contract_on_sqlite() {
        let db = smeltery_core::db::Db::connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrations::up(&smeltery_core::db::migration::Schema::new(&db))
            .await
            .unwrap();
        driver_contract(&db::DbDriver::new(db)).await;
    }

    async fn sqlite_driver() -> (smeltery_core::db::Db, db::DbDriver) {
        let db = smeltery_core::db::Db::connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrations::up(&smeltery_core::db::migration::Schema::new(&db))
            .await
            .unwrap();
        (db.clone(), db::DbDriver::new(db))
    }

    async fn count(db: &smeltery_core::db::Db, table: &str) -> i64 {
        use smeltery_core::db::prelude::sea_orm::{ConnectionTrait as _, DbBackend, Statement};
        let rows = db
            .conn()
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("SELECT COUNT(*) AS n FROM {table}"),
            ))
            .await
            .unwrap();
        rows[0].try_get::<i64>("", "n").unwrap()
    }

    /// S4-07: moving a job to the dead letters is one transaction. A failing delete used to leave the job both
    /// dead-lettered and queued (run again); now nothing changes.
    #[tokio::test]
    async fn dead_lettering_is_atomic() {
        let (db, driver) = sqlite_driver().await;
        let id = driver.push("a", "{}", 0, 0).await.unwrap();
        let job = driver.reserve(0, ANY).await.unwrap().unwrap();
        assert_eq!(job.id, id);
        db.execute(
            "CREATE TRIGGER no_delete BEFORE DELETE ON watchfire_jobs \
             BEGIN SELECT RAISE(ABORT, 'delete refused'); END",
        )
        .await
        .unwrap();
        assert!(driver.dead_letter(&job, "boom", 1).await.is_err());
        assert_eq!(count(&db, "watchfire_dead_letters").await, 0);
        assert_eq!(count(&db, "watchfire_jobs").await, 1);
        db.execute("DROP TRIGGER no_delete").await.unwrap();
        assert!(driver.dead_letter(&job, "boom", 1).await.unwrap());
        assert_eq!(count(&db, "watchfire_dead_letters").await, 1);
        assert_eq!(count(&db, "watchfire_jobs").await, 0);
    }

    /// S4-07: retrying a dead letter is one transaction. A failing insert used to lose the job (the dead letter was
    /// already deleted); now it stays a dead letter.
    #[tokio::test]
    async fn retrying_a_dead_letter_is_atomic() {
        let (db, driver) = sqlite_driver().await;
        driver.push("a", "{\"x\":1}", 0, 0).await.unwrap();
        let job = driver.reserve(0, ANY).await.unwrap().unwrap();
        assert!(driver.dead_letter(&job, "boom", 1).await.unwrap());
        let dead_id = driver.dead_letters(1).await.unwrap()[0].id;
        let queue = Queue::with_driver(
            Arc::new(driver),
            "database",
            Clock::new(),
            Duration::from_secs(5),
        );
        db.execute(
            "CREATE TRIGGER no_insert BEFORE INSERT ON watchfire_jobs \
             BEGIN SELECT RAISE(ABORT, 'insert refused'); END",
        )
        .await
        .unwrap();
        assert!(queue.retry_dead(dead_id).await.is_err());
        assert_eq!(count(&db, "watchfire_dead_letters").await, 1);
        db.execute("DROP TRIGGER no_insert").await.unwrap();
        let job_id = queue.retry_dead(dead_id).await.unwrap().unwrap();
        assert_eq!(count(&db, "watchfire_dead_letters").await, 0);
        let again = queue.driver.reserve(i64::MAX, ANY).await.unwrap().unwrap();
        assert_eq!(
            (again.id, again.payload.as_str(), again.attempts),
            (job_id, "{\"x\":1}", 1)
        );
        assert_eq!(queue.retry_dead(dead_id).await.unwrap(), None);
    }

    /// Concurrent requeues of dead letters on a SQLite file (each reads its row, then deletes and inserts) all
    /// succeed: the transaction takes the write lock at its start (`Db::begin_write`) instead of failing with
    /// "database is locked" at its first write.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_requeues_on_a_sqlite_file_all_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path()
                .join("queue.sqlite")
                .display()
                .to_string()
                .replace('\\', "/")
        );
        let db = smeltery_core::db::Db::connect(&url).await.unwrap();
        crate::migrations::up(&smeltery_core::db::migration::Schema::new(&db))
            .await
            .unwrap();
        let driver = Arc::new(db::DbDriver::new(db));
        for i in 0..16 {
            driver.push("a", &format!("{i}"), 0, 0).await.unwrap();
            let job = driver.reserve(0, ANY).await.unwrap().unwrap();
            assert!(driver.dead_letter(&job, "boom", 1).await.unwrap());
        }
        let dead = driver.dead_letters(u32::MAX).await.unwrap();
        assert_eq!(dead.len(), 16);
        let tasks: Vec<_> = dead
            .iter()
            .map(|d| {
                let driver = Arc::clone(&driver);
                let id = d.id;
                tokio::spawn(async move { driver.requeue_dead(id, 7).await })
            })
            .collect();
        for task in tasks {
            let requeued = task.await.unwrap();
            assert!(
                matches!(requeued, Ok(Some(_))),
                "{:?}",
                requeued.err().map(|e| e.to_string())
            );
        }
        assert!(driver.dead_letters(u32::MAX).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn memory_retry_requeues_and_dead_letters_are_bounded() {
        let driver = MemoryDriver::default();
        for i in 0..(MEMORY_DEAD_LETTERS + 5) {
            driver.push("a", &format!("{i}"), 0, 0).await.unwrap();
            let job = driver.reserve(0, ANY).await.unwrap().unwrap();
            assert!(driver.dead_letter(&job, "boom", 1).await.unwrap());
        }
        let dead = driver.dead_letters(u32::MAX).await.unwrap();
        assert_eq!(dead.len(), MEMORY_DEAD_LETTERS);
        assert_eq!(dead.last().unwrap().payload, "5", "the oldest went first");
        let id = dead[0].id;
        let job_id = driver.requeue_dead(id, 7).await.unwrap().unwrap();
        assert_eq!(driver.requeue_dead(id, 7).await.unwrap(), None);
        let job = driver.reserve(7, ANY).await.unwrap().unwrap();
        assert_eq!((job.id, job.attempts), (job_id, 1));
    }

    #[test]
    fn stored_errors_are_bounded_at_a_char_boundary() {
        assert_eq!(stored_error("short"), "short");
        let long = "é".repeat(40_000);
        let stored = stored_error(&long);
        assert!(stored.len() < MAX_STORED_ERROR + 64, "{}", stored.len());
        assert!(stored.ends_with("… (truncated, 80000 bytes)"), "{stored}");
        assert!(stored.starts_with("éé"));
    }

    #[tokio::test]
    async fn the_database_driver_stores_bounded_errors() {
        let (_db, driver) = sqlite_driver().await;
        driver.push("a", "{}", 0, 0).await.unwrap();
        let job = driver.reserve(0, ANY).await.unwrap().unwrap();
        driver
            .dead_letter(&job, &"x".repeat(70 * 1024), 1)
            .await
            .unwrap();
        let dead = driver.dead_letters(1).await.unwrap();
        assert!(dead[0].error.len() <= MAX_STORED_ERROR + 64);
        assert!(dead[0].error.contains("truncated"));
    }

    #[test]
    fn default_job_backoff_doubles_and_caps() {
        #[derive(Serialize, serde::Deserialize)]
        struct J;
        impl Job for J {
            const NAME: &'static str = "j";
            async fn handle(&self, _: JobCtx) -> Result<(), AgentError> {
                Ok(())
            }
        }
        assert_eq!(J.backoff(1), Duration::from_secs(5));
        assert_eq!(J.backoff(3), Duration::from_secs(20));
        assert_eq!(J.backoff(99), Duration::from_secs(600));
        assert_eq!(J.max_attempts(), 3);
    }
}
