//! Wiring into a Smeltery app: [`AgentsExt::agents`], launch on `serve` / `work`, the console
//! commands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smeltery_core::config::env;
use smeltery_core::console::{Args, Command, Output};
use smeltery_core::{App, AppBuilder, Background};
use tokio_util::sync::CancellationToken;

use crate::config::AgentConfig;
use crate::coord::{CacheLocks, Coordinator};
use crate::ctx::AgentCtx;
use crate::error::Error;
use crate::http::{BrokenTransport, Http, HttpOptions, ReqwestTransport, Transport};
use crate::policy::{Jitter, Restart};
use crate::queue::{Queue, Worker};
use crate::registry::Watchfire;
use crate::runtime::{Agents, Shared, SharedParts};
use crate::schedule::{self, SchedulerAgent};
use crate::store::{DbStore, MemoryStore, Store};
use crate::time::{Clock, format_utc};

/// Watchfire's settings, from `.env` / the environment.
///
/// | Variable | Default | Meaning |
/// |---|---|---|
/// | `WATCHFIRE_MAX_CONCURRENT` | `0` | agents running at once (0 = no limit; Watchfire's own agents do not count) |
/// | `WATCHFIRE_WORKERS` | `2` | queue workers (`queue#0..n`) |
/// | `WATCHFIRE_JOB_TIMEOUT` | `60` | seconds a job may run; reservations older than twice this are released |
/// | `WATCHFIRE_HTTP_TIMEOUT` | `30` | seconds per `ctx.http()` request |
/// | `WATCHFIRE_HTTP_MAX_BODY` | `10485760` | the largest response body `ctx.http()` reads, in bytes |
/// | `WATCHFIRE_STORE_TIMEOUT` | `5` | seconds per store or queue call |
/// | `WATCHFIRE_MAX_PAYLOAD` | `1048576` | the largest job payload in bytes: a larger dispatch is refused, a larger stored job is dead-lettered without running |
/// | `QUEUE_DRIVER` | `database` with a database, else `memory` | where jobs wait: `database`, `redis` (the `redis` feature; the server of `REDIS_URL`) or `memory` |
/// | `QUEUE_PREFIX` | `<app name in snake case>_queue_` | the start of the `redis` queue's keys |
/// | `WATCHFIRE_ALERT_WEBHOOK` | empty | URL alerts are POSTed to (JSON) |
/// | `WATCHFIRE_ALERT_MAIL` | empty | comma-separated addresses alerts are mailed to (the `mail` feature and `.mail()`) |
/// | `WATCHFIRE_DASHBOARD` | `local` | `local` (signed-in users the gate admits; open in local development), `auth` (signed-in users the gate admits), `off` |
/// | `WATCHFIRE_API_ADDR` | empty | address `work` serves the API on (e.g. `127.0.0.1:8001`) |
/// | `WATCHFIRE_IN_SERVE` | `true` | whether `serve` runs the agents, queue workers and scheduler |
/// | `WATCHFIRE_LOCK_STORE` | `CACHE_STORE` | the cache store whose locks coordinate processes (`off`: none) |
/// | `WATCHFIRE_LEASE_TTL` | `30` | seconds a lease lasts without renewal (at least 12/7 of singleton shutdown timeouts) |
///
/// Its `Debug` output shows only the host of `WATCHFIRE_ALERT_WEBHOOK` (the rest of a webhook URL is often its
/// secret).
#[derive(Clone)]
#[non_exhaustive]
pub struct WatchfireSettings {
    /// `WATCHFIRE_MAX_CONCURRENT`.
    pub max_concurrent: usize,
    /// `WATCHFIRE_WORKERS`.
    pub workers: usize,
    /// `WATCHFIRE_JOB_TIMEOUT`.
    pub job_timeout: Duration,
    /// `WATCHFIRE_HTTP_TIMEOUT`.
    pub http_timeout: Duration,
    /// `WATCHFIRE_HTTP_MAX_BODY`: the largest response body `ctx.http()` reads, in bytes.
    pub http_max_body: usize,
    /// `WATCHFIRE_STORE_TIMEOUT`.
    pub store_timeout: Duration,
    /// `WATCHFIRE_MAX_PAYLOAD`: the largest job payload, in bytes.
    pub max_payload: u64,
    /// `QUEUE_DRIVER` (empty: decide from the database).
    pub queue_driver: String,
    /// `QUEUE_PREFIX`: the start of the `redis` queue's keys (`None`: `<app name in snake case>_queue_`).
    pub queue_prefix: Option<String>,
    /// `WATCHFIRE_ALERT_WEBHOOK`: where alerts are POSTed as JSON.
    pub alert_webhook: Option<String>,
    /// `WATCHFIRE_ALERT_MAIL`: who alerts are mailed to.
    pub alert_mail: Vec<String>,
    /// `WATCHFIRE_DASHBOARD`: who may open the dashboard and the API without a token.
    pub dashboard: crate::web::Access,
    /// `WATCHFIRE_API_ADDR`: where `work` serves the API (and where the console commands
    /// call it).
    pub api_addr: Option<String>,
    /// `WATCHFIRE_IN_SERVE`: `serve` runs the background work too (`false`: only `work` does).
    pub in_serve: bool,
    /// `WATCHFIRE_LOCK_STORE`: the cache store for cross-process locks (`None`: `CACHE_STORE`; `off`: none).
    pub lock_store: Option<String>,
    /// `WATCHFIRE_LEASE_TTL`: how long a singleton agent's lease lasts without renewal.
    pub lease_ttl: Duration,
}

impl std::fmt::Debug for WatchfireSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchfireSettings")
            .field("max_concurrent", &self.max_concurrent)
            .field("workers", &self.workers)
            .field("job_timeout", &self.job_timeout)
            .field("http_timeout", &self.http_timeout)
            .field("http_max_body", &self.http_max_body)
            .field("store_timeout", &self.store_timeout)
            .field("max_payload", &self.max_payload)
            .field("queue_driver", &self.queue_driver)
            .field("queue_prefix", &self.queue_prefix)
            .field(
                "alert_webhook",
                &self
                    .alert_webhook
                    .as_deref()
                    .map(|url| format!("<redacted, host {}>", crate::alert::webhook_host(url))),
            )
            .field("alert_mail", &self.alert_mail)
            .field("dashboard", &self.dashboard)
            .field("api_addr", &self.api_addr)
            .field("in_serve", &self.in_serve)
            .field("lock_store", &self.lock_store)
            .field("lease_ttl", &self.lease_ttl)
            .finish()
    }
}

impl WatchfireSettings {
    /// Read the settings.
    pub fn from_env() -> Self {
        Self {
            max_concurrent: env::<usize>("WATCHFIRE_MAX_CONCURRENT", 0),
            workers: env::<usize>("WATCHFIRE_WORKERS", 2),
            job_timeout: Duration::from_secs(env::<u64>("WATCHFIRE_JOB_TIMEOUT", 60).max(1)),
            http_timeout: Duration::from_secs(env::<u64>("WATCHFIRE_HTTP_TIMEOUT", 30).max(1)),
            http_max_body: env::<usize>("WATCHFIRE_HTTP_MAX_BODY", crate::http::DEFAULT_MAX_BODY)
                .max(1),
            store_timeout: Duration::from_secs(env::<u64>("WATCHFIRE_STORE_TIMEOUT", 5).max(1)),
            max_payload: env::<u64>("WATCHFIRE_MAX_PAYLOAD", crate::queue::DEFAULT_MAX_PAYLOAD)
                .max(1),
            queue_driver: env::<String>("QUEUE_DRIVER", "")
                .trim()
                .to_ascii_lowercase(),
            queue_prefix: env::<Option<String>>("QUEUE_PREFIX", None)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            alert_webhook: env::<Option<String>>("WATCHFIRE_ALERT_WEBHOOK", None)
                .filter(|s| !s.trim().is_empty()),
            alert_mail: env::<String>("WATCHFIRE_ALERT_MAIL", "")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            dashboard: crate::web::Access::parse(&env::<String>("WATCHFIRE_DASHBOARD", "local")),
            api_addr: env::<Option<String>>("WATCHFIRE_API_ADDR", None)
                .filter(|s| !s.trim().is_empty()),
            in_serve: env::<bool>("WATCHFIRE_IN_SERVE", true),
            lock_store: env::<Option<String>>("WATCHFIRE_LOCK_STORE", None)
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty()),
            lease_ttl: Duration::from_secs(env::<u64>("WATCHFIRE_LEASE_TTL", 30).max(3)),
        }
    }
}

/// `AppBuilder::agents`: run Watchfire in the app.
///
/// ```
/// use smeltery_core::AppBuilder;
/// use smeltery_watchfire::AgentsExt as _;
/// use smeltery_watchfire::prelude::*;
///
/// fn register(w: &mut Watchfire) {
///     w.every(30.secs(), "heartbeat", |ctx| async move {
///         ctx.log().info("tick");
///         Ok(())
///     });
/// }
///
/// fn build(app: AppBuilder) -> AppBuilder {
///     app.agents(register)
/// }
/// # let _ = build(AppBuilder::new(smeltery_core::config::Settings::from_env()));
/// ```
pub trait AgentsExt {
    /// Register agents, jobs and the schedule with `register` (`app/agents/mod.rs`). At boot
    /// the registration is checked and the queue is set up (so handlers can dispatch jobs);
    /// `serve` and `work` start the agents, queue workers and scheduler and stop them within
    /// the app's shutdown budget. Adds the `agents:runs`, `schedule:list` and `schedule:run`
    /// commands. Call it once.
    fn agents(self, register: impl FnOnce(&mut Watchfire)) -> Self;
}

impl AgentsExt for AppBuilder {
    fn agents(self, register: impl FnOnce(&mut Watchfire)) -> Self {
        let mut watchfire = Watchfire::new();
        register(&mut watchfire);
        // The dashboard gate and the registration serve web requests also in a process that does not run
        // Watchfire (the dashboard then shows the agents of other processes).
        let gate = crate::web::DashboardGate(watchfire.gate.take());
        let registered = crate::remote::Registered::of(&watchfire);
        let slot = Arc::new(Mutex::new(Some(watchfire)));
        let boot_slot = Arc::clone(&slot);
        let start_slot = Arc::clone(&slot);
        let list_slot = Arc::clone(&slot);
        crate::web::mount(self)
            .commands(move |c| {
                crate::console::register(c);
                c.add(RunsCommand);
                c.add(ScheduleList {
                    slot: Arc::clone(&list_slot),
                });
                c.add(ScheduleRun { slot: list_slot });
            })
            .on_boot(move |app| async move {
                {
                    let guard = boot_slot.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(w) = guard.as_ref() {
                        w.validate()?;
                    }
                }
                let settings = WatchfireSettings::from_env();
                let queue = make_queue(&app, &settings, Clock::new())?;
                app.insert_service(queue);
                app.insert_service(settings);
                app.insert_service(gate);
                app.insert_service(registered);
                app.insert_service(crate::remote::RemoteCell::default());
                // Live dashboard panels when the app also installed Sparks.
                crate::web::live::register(&app)?;
                crate::web::live::watch_shared(&app);
                Ok(())
            })
            .on_start(move |app| async move {
                let settings = crate::web::settings(&app);
                if app.serves_http() && !settings.in_serve {
                    // The background work runs in `work`: PUBSUB_DRIVER=auto then shares messages with it.
                    app.set_web_only();
                    tracing::info!(
                        "WATCHFIRE_IN_SERVE=false: this process serves HTTP only; run `work` for the agents"
                    );
                    return Ok(Background::new(async {}));
                }
                let watchfire = start_slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .ok_or_else(|| {
                        smeltery_core::Error::internal("Watchfire was already started")
                    })?;
                let agents = launch(&app, watchfire, &settings).await?;
                app.insert_service(agents.clone());
                crate::web::live::start(&app, &agents);
                // Headless `work`: the API on its own address when one is configured.
                if !app.serves_http()
                    && let Some(addr) = &settings.api_addr
                {
                    let server =
                        crate::web::serve_api(app.clone(), addr, agents.shutdown_token()).await?;
                    agents.spawn_owned(server);
                }
                Ok(Background::new(async move {
                    agents.run_until_shutdown().await;
                }))
            })
    }
}

/// The queue for the app: database, Redis or memory.
fn make_queue(app: &App, settings: &WatchfireSettings, clock: Clock) -> Result<Queue, Error> {
    Ok(pick_queue(app, settings, clock)?.with_max_payload(settings.max_payload))
}

fn pick_queue(app: &App, settings: &WatchfireSettings, clock: Clock) -> Result<Queue, Error> {
    let db = app.db().ok();
    match (settings.queue_driver.as_str(), db) {
        ("memory", _) | ("", None) => Ok(Queue::memory(clock, settings.store_timeout)),
        ("database" | "", Some(db)) => Ok(Queue::database(db, clock, settings.store_timeout)),
        ("database", None) => Err(Error::Config(
            "QUEUE_DRIVER=database needs a database: set DATABASE_URL".to_owned(),
        )),
        ("redis", _) => redis_queue(app, settings, clock),
        (other, _) => Err(Error::Config(format!(
            "QUEUE_DRIVER must be `database`, `redis` or `memory`, not `{other}`"
        ))),
    }
}

/// `QUEUE_PREFIX`, else `<app name in snake case>_queue_`.
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
fn queue_prefix(app_name: &str, configured: Option<&str>) -> String {
    configured.map_or_else(|| format!("{}_queue_", snake(app_name)), str::to_owned)
}

/// `My App` → `my_app`: lowercase ASCII letters and digits, everything else one `_` (as the default
/// `CACHE_PREFIX`).
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
fn snake(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_owned();
    if out.is_empty() {
        "smeltery".to_owned()
    } else {
        out
    }
}

/// `QUEUE_PREFIX` for the `redis` queue: no `{` / `}` (the keys' hash tag must be the queue's), and no overlap with
/// `CACHE_PREFIX` in either direction. `cache:clear` (of any store on Redis: `CACHE_STORE`, `cache:clear redis`, the
/// lock store) removes every key that starts with `CACHE_PREFIX`; it must never match a queue key, and the queue's
/// keys must never be cache keys.
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
fn check_queue_prefix(prefix: &str, cache_prefix: &str) -> Result<(), Error> {
    if prefix.contains(['{', '}']) {
        return Err(Error::Config(format!(
            "QUEUE_PREFIX `{prefix}` contains `{{` or `}}`; use letters, digits, `_`, `-` or `:`"
        )));
    }
    let base = crate::queue::redis_key_base(prefix, crate::queue::REDIS_QUEUE);
    if base.starts_with(cache_prefix) || cache_prefix.starts_with(&base) {
        return Err(Error::Config(format!(
            "QUEUE_PREFIX `{prefix}` and CACHE_PREFIX `{cache_prefix}` overlap: `cache:clear` on Redis would remove \
             the queued jobs; choose prefixes where neither starts with the other"
        )));
    }
    Ok(())
}

#[cfg(feature = "redis")]
fn redis_queue(app: &App, settings: &WatchfireSettings, clock: Clock) -> Result<Queue, Error> {
    let s = app.settings();
    let prefix = queue_prefix(&s.name, settings.queue_prefix.as_deref());
    check_queue_prefix(&prefix, &s.cache_prefix)?;
    if let Some(host) = plain_remote_redis(&s.redis_url) {
        tracing::warn!(
            host = %host,
            "REDIS_URL sends the Redis password and the queued jobs unencrypted to another machine; use rediss:// \
             (TLS) unless the network between them is private"
        );
    }
    Queue::redis(&s.redis_url, &prefix, clock, settings.store_timeout)
}

/// The host of a plain-text (`redis://` / `valkey://`) URL with a password to a host that is not this machine, for
/// a warning; `None` for TLS, a local host, a Unix socket, no password or an unreadable URL. The password itself is
/// never returned.
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
fn plain_remote_redis(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "redis" | "valkey") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let (userinfo, host_port) = authority.rsplit_once('@')?;
    if !userinfo.split_once(':').is_some_and(|(_, p)| !p.is_empty()) {
        return None;
    }
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => host_port.split(':').next()?,
    };
    if host.is_empty() {
        return None;
    }
    let local = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    (!local).then(|| host.to_owned())
}

#[cfg(not(feature = "redis"))]
fn redis_queue(app: &App, settings: &WatchfireSettings, _clock: Clock) -> Result<Queue, Error> {
    let _ = (app, settings);
    Err(Error::Config(
        "QUEUE_DRIVER=redis needs the `redis` feature of smeltery".to_owned(),
    ))
}

/// How the lock store answered at launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StoreCheck {
    /// `WATCHFIRE_LOCK_STORE=off`.
    Off,
    /// A store that lives in one process (`memory`, `array`, `null`).
    Unshared,
    /// The store cannot be opened (unknown, its feature off, the database store without a database).
    OpenFailed(String),
    /// It opened but a probe lock failed (server down, tables missing).
    ProbeFailed(String),
    /// It takes and frees locks.
    Works,
}

/// Whether this process coordinates through the lock store. A store named in `WATCHFIRE_LOCK_STORE` that cannot
/// work is an error. A store that does not answer at launch is coordinated through anyway outside `local` /
/// `testing` (singletons wait in standby and ticks are skipped until it answers): running everything here could
/// double what other processes run. Only in local development and tests does a silent store mean "run it all
/// here", so an app whose migrations have not run still works.
pub(crate) fn decide(
    explicit: bool,
    env: &str,
    name: &str,
    check: &StoreCheck,
) -> Result<bool, Error> {
    let development = matches!(env, "local" | "testing");
    match check {
        StoreCheck::Off => Ok(false),
        StoreCheck::Unshared if explicit => Err(Error::Config(format!(
            "WATCHFIRE_LOCK_STORE=`{name}` is not shared between processes; use one of {} or `off`",
            crate::coord::SHARED_STORES.join(", ")
        ))),
        StoreCheck::Unshared => {
            tracing::debug!(store = %name, "the cache store is not shared; no cross-process locks");
            Ok(false)
        }
        StoreCheck::OpenFailed(e) if explicit => {
            Err(Error::Config(format!("WATCHFIRE_LOCK_STORE: {e}")))
        }
        StoreCheck::OpenFailed(e) => {
            tracing::warn!(store = %name, error = %e, "cannot open the cache store for Watchfire's locks; this process runs every agent and scheduled task itself");
            Ok(false)
        }
        StoreCheck::ProbeFailed(e) if development && explicit => Err(Error::Config(format!(
            "WATCHFIRE_LOCK_STORE=`{name}` does not work: {e}"
        ))),
        StoreCheck::ProbeFailed(e) if development => {
            tracing::warn!(store = %name, error = %e, "the cache store's locks do not work (run the migrations?); in local development this process runs every agent and scheduled task itself");
            Ok(false)
        }
        StoreCheck::ProbeFailed(e) => {
            tracing::warn!(store = %name, error = %e, "the cache store's locks do not answer; agents wait in standby and scheduled ticks are skipped until they do");
            Ok(true)
        }
        StoreCheck::Works => {
            tracing::debug!(store = %name, "Watchfire coordinates processes through the cache's locks");
            Ok(true)
        }
    }
}

/// The lock store processes coordinate through: `WATCHFIRE_LOCK_STORE`, else the app's `CACHE_STORE`, when it is
/// one every process shares; see [`decide`].
pub(crate) async fn make_coordinator(
    app: &App,
    settings: &WatchfireSettings,
) -> Result<Option<Arc<Coordinator>>, Error> {
    let explicit = settings.lock_store.is_some();
    let name = settings
        .lock_store
        .clone()
        .unwrap_or_else(|| app.settings().cache_store.to_ascii_lowercase());
    let env = app.settings().env.clone();
    if matches!(name.as_str(), "off" | "none" | "false") {
        decide(explicit, &env, &name, &StoreCheck::Off)?;
        return Ok(None);
    }
    if !crate::coord::SHARED_STORES.contains(&name.as_str()) {
        decide(explicit, &env, &name, &StoreCheck::Unshared)?;
        return Ok(None);
    }
    let cache = match app.cache().store(&name) {
        Ok(cache) => cache,
        Err(e) => {
            decide(
                explicit,
                &env,
                &name,
                &StoreCheck::OpenFailed(e.to_string()),
            )?;
            return Ok(None);
        }
    };
    let coord = Arc::new(Coordinator::new(
        Arc::new(CacheLocks(cache)),
        &name,
        settings.lease_ttl,
    ));
    let check = match coord.probe().await {
        Ok(()) => StoreCheck::Works,
        Err(e) => StoreCheck::ProbeFailed(e),
    };
    Ok(decide(explicit, &env, &name, &check)?.then_some(coord))
}

/// The lock store as a view of who holds what (no probe): for reading lease holders in any process.
pub(crate) fn lock_view(app: &App, settings: &WatchfireSettings) -> Option<Arc<Coordinator>> {
    let name = settings
        .lock_store
        .clone()
        .unwrap_or_else(|| app.settings().cache_store.to_ascii_lowercase());
    if !crate::coord::SHARED_STORES.contains(&name.as_str()) {
        return None;
    }
    let cache = app.cache().store(&name).ok()?;
    Some(Arc::new(Coordinator::new(
        Arc::new(CacheLocks(cache)),
        &name,
        settings.lease_ttl,
    )))
}

/// Hold this process's own lease while it runs, so the others can tell it is alive, and mark the runs of ended
/// processes interrupted: at launch and every lease time to live.
pub(crate) async fn process_keeper(shared: Arc<Shared>, coord: Arc<Coordinator>) {
    let name = crate::coord::process_lock(coord.owner());
    let mut lease: Option<crate::coord::Lease> = None;
    // Whether the lease was lost since this process last held it (others may have marked its runs interrupted).
    let mut lapsed = false;
    let retry = (coord.ttl() / 10).max(Duration::from_millis(100));
    let mut next_sweep = tokio::time::Instant::now();
    loop {
        if lease.as_ref().is_none_or(|l| l.lost().is_cancelled()) {
            if lease.take().is_some() {
                lapsed = true;
                tracing::warn!("this process's lease was lost; taking it again");
            }
            let spawner = Arc::clone(&shared);
            match coord
                .lease(&name, Duration::ZERO, |task| spawner.spawn_task(task))
                .await
            {
                Ok(Some(taken)) => {
                    lease = Some(taken);
                    if std::mem::take(&mut lapsed) {
                        // Put this process's runs back to `running` where a sweep marked them interrupted.
                        shared.reassert_runs().await;
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "cannot take this process's lease; retrying"),
            }
        }
        if tokio::time::Instant::now() >= next_sweep {
            // Final run records that failed to write (a database outage) are written again.
            shared.flush_unwritten().await;
            sweep(&shared, &coord).await;
            next_sweep = tokio::time::Instant::now() + coord.ttl();
        }
        let lost = lease
            .as_ref()
            .map_or_else(CancellationToken::new, |l| l.lost().clone());
        let wait = if lease.is_some() {
            next_sweep.saturating_duration_since(tokio::time::Instant::now())
        } else {
            retry
        };
        tokio::select! {
            biased;
            () = shared.shutdown.cancelled() => break,
            () = lost.cancelled() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
    if let Some(lease) = lease {
        lease.release().await;
    }
}

/// Mark the runs still recorded as running by processes that hold no process lease any more as interrupted.
pub(crate) async fn sweep(shared: &Shared, coord: &Coordinator) {
    let processes = match shared
        .timed(
            "running_processes",
            shared.store.running_processes(coord.owner()),
        )
        .await
    {
        Ok(processes) => processes,
        Err(e) => {
            tracing::debug!(error = %e, "cannot list the processes with running runs");
            return;
        }
    };
    for process in processes {
        if let Ok(None) = coord.holder(&crate::coord::process_lock(&process)).await {
            let now = shared.clock.now_ms();
            if let Err(e) = shared
                .timed(
                    "mark_process_interrupted",
                    shared.store.mark_process_interrupted(&process, now),
                )
                .await
            {
                tracing::warn!(error = %e, process, "cannot mark an ended process's runs interrupted");
            }
        }
    }
}

/// The store: the database when the Watchfire tables exist, else memory.
async fn make_store(app: &App) -> Arc<dyn Store> {
    if let Ok(db) = app.db() {
        if DbStore::ready(&db).await {
            return Arc::new(DbStore::open(db).await);
        }
        tracing::warn!(
            "the Watchfire tables are missing (run the migrations); agent history is kept in memory"
        );
    }
    Arc::new(MemoryStore::default())
}

fn real_transport() -> Arc<dyn Transport> {
    match ReqwestTransport::new() {
        Ok(t) => Arc::new(t),
        Err(e) => {
            tracing::error!(error = %e, "cannot set up the HTTP client; ctx.http() requests will fail");
            Arc::new(BrokenTransport(e.to_string()))
        }
    }
}

/// How the shared state is put together; tests swap the transport, clock, store and jitter.
pub(crate) struct LaunchParts {
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) store: Arc<dyn Store>,
    pub(crate) jitter: Jitter,
    pub(crate) health_interval: Duration,
    /// Cross-process locks (`None`: this process coordinates with nobody).
    pub(crate) coord: Option<Arc<Coordinator>>,
}

pub(crate) fn build_shared(
    app: &App,
    watchfire: &mut Watchfire,
    settings: &WatchfireSettings,
    parts: LaunchParts,
    shutdown: CancellationToken,
) -> Arc<Shared> {
    let queue = app.service::<Queue>().map(|q| (*q).clone());
    let clock = queue.as_ref().map_or_else(Clock::new, |q| q.clock);
    let options = HttpOptions {
        timeout: settings.http_timeout,
        max_body: settings.http_max_body,
        user_agent: format!("{} (smeltery-watchfire)", app.settings().name),
        ..HttpOptions::default()
    };
    let http = Http::from_arc(parts.transport, options, &watchfire.rates);
    Shared::new(SharedParts {
        app: app.clone(),
        store: parts.store,
        store_timeout: settings.store_timeout,
        clock,
        jitter: parts.jitter,
        http,
        queue,
        jobs: std::mem::take(&mut watchfire.jobs),
        job_timeout: settings.job_timeout,
        health_interval: parts.health_interval,
        groups: watchfire
            .groups
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect::<HashMap<_, _>>(),
        max_concurrent: settings.max_concurrent,
        shutdown,
        budget: app.settings().shutdown_timeout,
        coord: parts.coord,
    })
}

/// Start everything registered; returns the handle.
pub(crate) async fn launch(
    app: &App,
    watchfire: Watchfire,
    settings: &WatchfireSettings,
) -> Result<Agents, Error> {
    let parts = LaunchParts {
        transport: real_transport(),
        store: make_store(app).await,
        jitter: Jitter::from_os(),
        health_interval: Duration::from_secs(5),
        coord: make_coordinator(app, settings).await?,
    };
    launch_with(app, watchfire, settings, parts, |_| {}).await
}

pub(crate) async fn launch_with(
    app: &App,
    mut watchfire: Watchfire,
    settings: &WatchfireSettings,
    parts: LaunchParts,
    before_spawn: impl FnOnce(&Agents),
) -> Result<Agents, Error> {
    #[cfg(feature = "mail")]
    crate::mail::prepare(app, &mut watchfire, &settings.alert_mail);
    #[cfg(not(feature = "mail"))]
    if !settings.alert_mail.is_empty() {
        tracing::warn!(
            "WATCHFIRE_ALERT_MAIL needs the `mail` feature of smeltery-watchfire; alerts are not mailed"
        );
    }
    watchfire.validate()?;
    let shared = build_shared(
        app,
        &mut watchfire,
        settings,
        parts,
        app.shutdown_token().child_token(),
    );
    check_call_leases(shared.coord.as_deref(), &watchfire.schedule)?;
    let agents = Agents::new(Arc::clone(&shared));
    before_spawn(&agents);
    let has_jobs = !shared.jobs.is_empty();
    for reg in watchfire.agents {
        shared
            .spawn_agent(reg.name, reg.config, reg.agent, None, false)
            .await?;
    }
    let framework = AgentConfig::default()
        .restart(Restart::Always)
        .backoff(Duration::from_secs(1)..=Duration::from_secs(30));
    if has_jobs && shared.queue.is_some() {
        for i in 0..settings.workers.max(1) {
            let name = format!("queue#{i}");
            shared
                .spawn_agent(
                    name.clone(),
                    framework.clone(),
                    Box::new(Worker { name }),
                    None,
                    true,
                )
                .await?;
        }
    }
    shared.start_alerts(
        std::mem::take(&mut watchfire.alert_hooks),
        settings.alert_webhook.clone(),
    );
    let entries = Arc::new(std::mem::take(&mut watchfire.schedule));
    let _ = shared.schedule.set(Arc::clone(&entries));
    if !entries.is_empty() {
        shared
            .spawn_agent(
                "scheduler".to_owned(),
                framework,
                Box::new(SchedulerAgent { entries }),
                None,
                true,
            )
            .await?;
    }
    if let Some(coord) = shared.coord.clone() {
        shared.spawn_task(process_keeper(Arc::clone(&shared), coord));
        // Commands from other processes for the agents this one holds.
        if let Some(remote) = crate::remote::remote_of(app).await {
            shared.spawn_task(crate::remote::poll_commands(
                Arc::clone(&shared),
                remote.store,
            ));
        }
    }
    tracing::info!(agents = agents.names().len(), process = %shared.process, "Watchfire launched");
    Ok(agents)
}

/// `agents:runs <name> [--limit N]`: the latest runs from the database.
struct RunsCommand;

impl Command for RunsCommand {
    fn name(&self) -> &'static str {
        "agents:runs"
    }

    fn about(&self) -> &'static str {
        "Show an agent's latest runs from the database (--limit N, default 20)"
    }

    async fn run(&self, app: &App, args: Args) -> smeltery_core::Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(
        &self,
        app: &App,
        args: Args,
        out: Output,
    ) -> smeltery_core::Result<()> {
        let name = args.get(0).ok_or_else(|| {
            smeltery_core::Error::internal("usage: agents:runs <name> [--limit N]")
        })?;
        let limit = match args.value("limit") {
            Some(raw) => raw.parse::<u32>().map_err(|_| {
                smeltery_core::Error::internal(format!("--limit must be a number, not `{raw}`"))
            })?,
            None => 20,
        };
        let db = app.db().map_err(|_| {
            smeltery_core::Error::internal(
                "agents:runs reads the database: set DATABASE_URL (without one, run history lives in the running app's memory)",
            )
        })?;
        if !DbStore::ready(&db).await {
            return Err(smeltery_core::Error::internal(
                "the Watchfire tables are missing: run `migrate`",
            ));
        }
        let store = DbStore::open(db).await;
        let timeout = WatchfireSettings::from_env().store_timeout;
        let runs = tokio::time::timeout(timeout, store.recent_runs(Some(name), limit))
            .await
            .map_err(|_| smeltery_core::Error::internal("the database timed out"))?
            .map_err(Error::from)?;
        if runs.is_empty() {
            out.line(format!("No runs recorded for `{name}`."));
            return Ok(());
        }
        out.line(format!(
            "{:<6} {:<19}  {:>9}  {:<11} {:<16} ERROR",
            "RUN", "STARTED (UTC)", "DURATION", "OUTCOME", "JOB"
        ));
        for run in runs {
            let duration = run.ended_at_ms.map_or_else(
                || "-".to_owned(),
                |end| format_duration(end - run.started_at_ms),
            );
            let counters = if run.counters.is_empty() {
                String::new()
            } else {
                format!(
                    " [{}]",
                    run.counters
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            };
            let line = format!(
                "{:<6} {:<19}  {:>9}  {:<11} {:<16} {}{}",
                run.run_id,
                format_utc(run.started_at_ms),
                duration,
                run.outcome.as_str(),
                run.job.unwrap_or_default(),
                run.error.unwrap_or_default(),
                counters
            );
            out.line(line.trim_end());
        }
        Ok(())
    }
}

fn format_duration(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

type Slot = Arc<Mutex<Option<Watchfire>>>;

/// `schedule:list`.
struct ScheduleList {
    slot: Slot,
}

impl Command for ScheduleList {
    fn name(&self) -> &'static str {
        "schedule:list"
    }

    fn about(&self) -> &'static str {
        "List the scheduled tasks and their next run (UTC)"
    }

    async fn run(&self, app: &App, args: Args) -> smeltery_core::Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(
        &self,
        _app: &App,
        _args: Args,
        out: Output,
    ) -> smeltery_core::Result<()> {
        let infos = {
            let guard = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .as_ref()
                .map(|w| schedule::infos(&w.schedule, crate::time::system_ms()))
                .unwrap_or_default()
        };
        if infos.is_empty() {
            out.line("No scheduled tasks.");
            return Ok(());
        }
        let width = infos.iter().map(|i| i.name.len()).max().unwrap_or(4).max(4);
        let expr = infos
            .iter()
            .map(|i| i.expression.len())
            .max()
            .unwrap_or(10)
            .max(10);
        out.line(format!(
            "{:<width$}  {:<5}  {:<expr$}  NEXT RUN (UTC)",
            "NAME", "KIND", "EXPRESSION"
        ));
        for info in infos {
            let next = info
                .next_run_ms
                .map_or_else(|| "never".to_owned(), format_utc);
            out.line(format!(
                "{:<width$}  {:<5}  {:<expr$}  {next}",
                info.name, info.kind, info.expression
            ));
        }
        Ok(())
    }
}

/// Scheduled calls that must not overlap hold a lease while they run, which must cover their stop time: refused at
/// launch (and by `schedule:run`), like singleton agents, rather than skipped at every tick.
fn check_call_leases(
    coord: Option<&Coordinator>,
    schedule: &[schedule::Entry],
) -> Result<(), Error> {
    if let Some(coord) = coord
        && let Some(entry) = schedule.iter().find(|e| e.needs_run_lease())
        && let Err(needed) = coord.cutoff(schedule::CALL_STOP)
    {
        return Err(Error::Config(format!(
            "scheduled call `{}` (stopped within {:?} when its lease is lost): {}",
            entry.name,
            schedule::CALL_STOP,
            coord.too_short(schedule::CALL_STOP, needed)
        )));
    }
    Ok(())
}

/// `schedule:run` on `parts`: the output lines for the tasks due this minute, and whether they ran.
pub(crate) async fn schedule_run(
    app: &App,
    mut watchfire: Watchfire,
    settings: &WatchfireSettings,
    parts: LaunchParts,
) -> Result<(String, smeltery_core::Result<()>), Error> {
    let shared = build_shared(
        app,
        &mut watchfire,
        settings,
        parts,
        CancellationToken::new(),
    );
    // Refused like at launch: with a lease too short for a call's stop, every tick would be skipped.
    check_call_leases(shared.coord.as_deref(), &watchfire.schedule)?;
    // While it runs, this process holds its own lease, so other processes' sweeps leave its runs alone.
    let process_lease = match shared.coord.clone() {
        Some(coord) => {
            let spawner = Arc::clone(&shared);
            coord
                .lease(
                    &crate::coord::process_lock(coord.owner()),
                    Duration::ZERO,
                    |task| spawner.spawn_task(task),
                )
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    let ctx = AgentCtx::detached(
        app.clone(),
        shared,
        "scheduler",
        app.shutdown_token().child_token(),
    );
    let mut buffer = Vec::new();
    let result = schedule::run_due(&watchfire.schedule, &ctx, &mut buffer).await;
    if let Some(lease) = process_lease {
        lease.release().await;
    }
    let out = String::from_utf8_lossy(&buffer).trim_end().to_owned();
    Ok((out, result))
}

/// `schedule:run`.
struct ScheduleRun {
    slot: Slot,
}

impl Command for ScheduleRun {
    fn name(&self) -> &'static str {
        "schedule:run"
    }

    fn about(&self) -> &'static str {
        "Run the scheduled tasks due this minute once (for system cron)"
    }

    async fn run(&self, app: &App, args: Args) -> smeltery_core::Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(
        &self,
        app: &App,
        _args: Args,
        out: Output,
    ) -> smeltery_core::Result<()> {
        let watchfire = self.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(watchfire) = watchfire else {
            out.line("No scheduled tasks.");
            return Ok(());
        };
        let settings = WatchfireSettings::from_env();
        let parts = LaunchParts {
            transport: real_transport(),
            store: make_store(app).await,
            jitter: Jitter::from_os(),
            health_interval: Duration::from_secs(5),
            coord: make_coordinator(app, &settings).await?,
        };
        let (text, result) = schedule_run(app, watchfire, &settings, parts).await?;
        out.line(text);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::FakeTransport;
    use crate::status::AgentState;
    use crate::store::failing::FailingStore;
    use crate::time::DurationExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn settings_debug_hides_the_webhook_url() {
        let mut settings = WatchfireSettings::from_env();
        settings.alert_webhook =
            Some("https://hooks.example.com/services/T000/B000/WEBHOOKSECRET?token=T".to_owned());
        let debug = format!("{settings:?}");
        assert!(!debug.contains("WEBHOOKSECRET"), "{debug}");
        assert!(!debug.contains("token=T"), "{debug}");
        assert!(debug.contains("hooks.example.com"), "{debug}");
    }

    async fn bare_app() -> App {
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.env = "testing".to_owned();
        settings.database_url = String::new();
        settings.name = "My Shop".to_owned();
        settings.cache_store = "memory".to_owned();
        settings.redis_url = "redis://:hunter2@127.0.0.1:6379".to_owned();
        settings.cache_prefix = "my_shop_cache_".to_owned();
        AppBuilder::new(settings).build().await.unwrap().app
    }

    #[tokio::test]
    async fn the_queue_driver_is_chosen_by_queue_driver() {
        let app = bare_app().await;
        // Every setting the test depends on is set here, never read from the environment.
        let mut settings = WatchfireSettings::from_env();
        settings.queue_driver = String::new();
        settings.queue_prefix = None;
        assert_eq!(
            make_queue(&app, &settings, Clock::new()).unwrap().driver(),
            "memory"
        );
        settings.queue_driver = "database".to_owned();
        assert!(make_queue(&app, &settings, Clock::new()).is_err());
        settings.queue_driver = "sqs".to_owned();
        let err = make_queue(&app, &settings, Clock::new())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("`database`, `redis` or `memory`, not `sqs`"),
            "{err}"
        );
        settings.queue_driver = "redis".to_owned();
        let made = make_queue(&app, &settings, Clock::new());
        #[cfg(feature = "redis")]
        {
            let queue = made.unwrap();
            assert_eq!(queue.driver(), "redis");
            assert!(!format!("{queue:?}").contains("hunter2"));
        }
        #[cfg(not(feature = "redis"))]
        assert!(
            made.unwrap_err()
                .to_string()
                .contains("needs the `redis` feature"),
        );
    }

    /// Pure: no app, no environment (it once read both, and failed in full runs only).
    #[test]
    fn the_queue_prefix_defaults_to_the_app_name_and_avoids_the_cache_prefix() {
        assert_eq!(queue_prefix("My Shop", None), "my_shop_queue_");
        assert_eq!(queue_prefix("My Shop", Some("other_")), "other_");
        assert_eq!(snake("  Été Shop 2! "), "t_shop_2");
        assert_eq!(snake("!!"), "smeltery");
        // `cache:clear` on Redis removes keys under CACHE_PREFIX, whatever CACHE_STORE says (`cache:clear redis`,
        // the lock store): the prefixes must not overlap in either direction.
        let err = check_queue_prefix("app_cache_queue_", "app_cache_")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("would remove the queued jobs; choose"),
            "{err}"
        );
        assert!(check_queue_prefix("app_queue_", "app_cache_").is_ok());
        assert!(check_queue_prefix("app_", "app_{default}:job:").is_err());
        assert!(check_queue_prefix("app_", "app_{def").is_err());
        assert!(check_queue_prefix("app_", "app_{default}:x").is_err());
        assert!(check_queue_prefix("app_", "app_cache_").is_ok());
        assert!(check_queue_prefix("anything", "").is_err());
        // The hash tag must be the queue's own.
        assert!(check_queue_prefix("a{b}_", "app_cache_").is_err());
        assert!(check_queue_prefix("a}_", "app_cache_").is_err());
    }

    /// Sweep W7-09: a plain Redis URL with a password to another machine is warned about; TLS, local hosts and
    /// URLs without a password are not. The warning names the host only.
    #[test]
    fn plain_remote_redis_urls_are_found() {
        assert_eq!(
            plain_remote_redis("redis://:hunter2@cache.internal:6379/0").as_deref(),
            Some("cache.internal")
        );
        assert_eq!(
            plain_remote_redis("redis://user:hunter2@10.0.0.5").as_deref(),
            Some("10.0.0.5")
        );
        for quiet in [
            "rediss://:hunter2@cache.internal:6379",
            "redis://:hunter2@127.0.0.1:6379",
            "redis://:hunter2@localhost",
            "redis://:hunter2@[::1]:6379",
            "redis://cache.internal:6379",
            "not a url",
            "",
        ] {
            assert_eq!(plain_remote_redis(quiet), None, "{quiet}");
        }
    }

    /// Sweep W7-03 / W7-09: `WATCHFIRE_MAX_PAYLOAD` reaches the queue; `QUEUE_DRIVER` is trimmed.
    #[tokio::test]
    async fn the_queue_gets_the_payload_limit() {
        let app = bare_app().await;
        let mut settings = WatchfireSettings::from_env();
        settings.queue_driver = "memory".to_owned();
        settings.max_payload = 10;
        let queue = make_queue(&app, &settings, Clock::new()).unwrap();
        assert_eq!(queue.max_payload, 10);
        let err = queue
            .push_raw("j", "01234567890", Duration::ZERO)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("WATCHFIRE_MAX_PAYLOAD"), "{err}");
        assert_eq!(queue.stats().await.unwrap().pending, 0, "nothing queued");
        queue
            .push_raw("j", "0123456789", Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(queue.stats().await.unwrap().pending, 1);
    }

    #[test]
    fn whether_a_process_coordinates() {
        let probe = StoreCheck::ProbeFailed("down".into());
        let open = StoreCheck::OpenFailed("no database".into());
        // A store named explicitly must be usable.
        assert!(decide(true, "production", "memory", &StoreCheck::Unshared).is_err());
        assert!(decide(true, "production", "redis", &open).is_err());
        assert!(decide(true, "local", "redis", &probe).is_err());
        // The default store: memory and friends do not coordinate; one that works does.
        assert!(!decide(false, "production", "memory", &StoreCheck::Unshared).unwrap());
        assert!(decide(false, "production", "database", &StoreCheck::Works).unwrap());
        assert!(!decide(false, "production", "x", &StoreCheck::Off).unwrap());
        // A silent store: run everything here only in local development and tests.
        assert!(!decide(false, "local", "database", &probe).unwrap());
        assert!(!decide(false, "testing", "database", &probe).unwrap());
        assert!(decide(false, "production", "database", &probe).unwrap());
        assert!(decide(false, "staging", "database", &probe).unwrap());
        assert!(decide(true, "production", "redis", &probe).unwrap());
        // A store that cannot even be opened is a configuration no process can coordinate through.
        assert!(!decide(false, "production", "database", &open).unwrap());
    }

    #[tokio::test]
    async fn make_coordinator_reads_the_settings() {
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.env = "production".into();
        settings.cache_store = "memory".into();
        settings.database_url = String::new();
        let app = AppBuilder::new(settings).build().await.unwrap().app;
        let mut wf = WatchfireSettings::from_env();
        wf.lock_store = None;
        assert!(make_coordinator(&app, &wf).await.unwrap().is_none());
        wf.lock_store = Some("array".into());
        assert!(make_coordinator(&app, &wf).await.is_err());
        // The database store without a database: explicit is an error, the default runs uncoordinated.
        wf.lock_store = Some("database".into());
        assert!(make_coordinator(&app, &wf).await.is_err());
        // The file store works anywhere.
        let dir = tempfile::tempdir().unwrap();
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.env = "production".into();
        settings.cache_store = "file".into();
        settings.cache_path = dir.path().to_path_buf();
        settings.database_url = String::new();
        let app = AppBuilder::new(settings).build().await.unwrap().app;
        wf.lock_store = None;
        assert!(make_coordinator(&app, &wf).await.unwrap().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_store_never_kills_an_agent() {
        let app = AppBuilder::new(smeltery_core::config::Settings::from_env())
            .build()
            .await
            .unwrap()
            .app;
        let ticks = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&ticks);
        let mut w = Watchfire::new();
        w.run("steady", move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                // Checkpoints report the store failure to the agent, which carries on.
                assert!(ctx.checkpoint(&1).await.is_err());
                let mut ticker = ctx.interval(1.secs());
                while ticker.tick().await {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            }
        });
        let parts = LaunchParts {
            transport: Arc::new(FakeTransport::new()),
            store: Arc::new(FailingStore),
            jitter: Jitter::seeded(1),
            health_interval: Duration::from_secs(1),
            coord: None,
        };
        let agents = launch_with(&app, w, &WatchfireSettings::from_env(), parts, |_| {})
            .await
            .unwrap();
        tokio::time::sleep(10.secs()).await;
        assert_eq!(agents.status("steady").unwrap().state, AgentState::Running);
        assert!(ticks.load(Ordering::SeqCst) >= 10);
        assert!(agents.runs(Some("steady"), 5).await.is_err());
        agents.stop("steady").await.unwrap();
        assert_eq!(agents.status("steady").unwrap().state, AgentState::Stopped);
        agents.shutdown().await;
    }
}
