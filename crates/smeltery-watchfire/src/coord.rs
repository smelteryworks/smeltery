//! Cross-process coordination over the cache's atomic locks: singleton agents hold a lease (a lock renewed by a
//! keeper task), scheduled tasks claim each due tick once (`Cache::add`), scheduled calls that must not overlap hold
//! a lease while they run, and each process holds a lease on its own identity (so others can tell it is alive).
//!
//! A lease is a lock with a time to live `ttl` (`WATCHFIRE_LEASE_TTL`). Its holder needs `stop` to stop what the
//! lease guards (an agent's shutdown timeout). The lease counts as lost at a hard deadline, `cutoff = ttl - stop -
//! ttl / 6` after the last successful renewal was SENT (so the store renewed it no earlier), whatever store call is
//! in flight then; a renewal that answers that another owner holds it loses it at once. Renewals go out every
//! `cutoff / 3` (retried every `cutoff / 10` after an error). So the holder has stopped by `ttl - ttl / 6` after the
//! store's last renewal, and another process, which sees the lock free `ttl` after it by its own clock, never runs
//! the same thing while clocks differ by less than `ttl / 6`. A process that ends releases its leases; a crashed one
//! leaves them to expire.

use std::sync::Arc;
use std::time::Duration;

use smeltery_core::BoxFuture;
use smeltery_core::cache::Cache;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Lock operations Watchfire needs; errors are the store's message.
pub(crate) trait LockBackend: Send + Sync + 'static {
    /// Take lock `name` for `owner` when it is free (or expired).
    fn acquire<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, String>>;
    /// Hold lock `name` for `ttl` from now when `owner` holds it.
    fn refresh<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, String>>;
    /// Free lock `name` when `owner` holds it.
    fn release<'a>(&'a self, name: &'a str, owner: &'a str) -> BoxFuture<'a, Result<bool, String>>;
    /// Store `key` (for `ttl`) only when it is absent: `true` for the first caller.
    fn claim<'a>(
        &'a self,
        key: &'a str,
        owner: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, String>>;
    /// Who holds lock `name` now.
    fn owner<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>, String>>;
}

/// The real backend: a cache store (`Cache::lock`, `Lock::refresh`, `Cache::add`).
pub(crate) struct CacheLocks(pub(crate) Cache);

impl LockBackend for CacheLocks {
    fn acquire<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            self.0
                .restore_lock(name, owner, ttl)
                .get()
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn refresh<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            self.0
                .restore_lock(name, owner, ttl)
                .refresh()
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn release<'a>(&'a self, name: &'a str, owner: &'a str) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            self.0
                .restore_lock(name, owner, Duration::ZERO)
                .release()
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn claim<'a>(
        &'a self,
        key: &'a str,
        owner: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move { self.0.add(key, owner, ttl).await.map_err(|e| e.to_string()) })
    }

    fn owner<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            self.0
                .restore_lock(name, "", Duration::ZERO)
                .current_owner()
                .await
                .map_err(|e| e.to_string())
        })
    }
}

/// The lease name of a process (Watchfire's identity of it), held while it runs.
pub(crate) fn process_lock(owner: &str) -> String {
    format!("watchfire:process:{owner}")
}

/// What a lease keeps back for clock skew between processes: `ttl / 6`.
pub(crate) fn reserve(ttl: Duration) -> Duration {
    ttl / 6
}

/// The hard cut-off of a lease after its last renewal was sent: `ttl - stop - ttl / 6`; `Err` with the smallest
/// time to live (whole seconds) that works when that leaves less than `ttl / 4` (too little to renew a few times).
pub(crate) fn cutoff(ttl: Duration, stop: Duration) -> Result<Duration, Duration> {
    let cut = ttl.saturating_sub(stop).saturating_sub(reserve(ttl));
    if cut >= ttl / 4 && !cut.is_zero() {
        return Ok(cut);
    }
    // ttl - stop - ttl / 6 >= ttl / 4  <=>  ttl >= stop * 12 / 7
    let needed_ms = stop.as_millis().saturating_mul(12).div_ceil(7);
    let secs = u64::try_from(needed_ms.div_ceil(1000)).unwrap_or(u64::MAX);
    let mut needed = Duration::from_secs(secs.max(3));
    // Rounding: step up until it fits.
    while cutoff_fits(needed, stop).is_none() && needed < Duration::from_secs(u64::MAX / 2) {
        needed += Duration::from_secs(1);
    }
    Err(needed)
}

fn cutoff_fits(ttl: Duration, stop: Duration) -> Option<Duration> {
    let cut = ttl.saturating_sub(stop).saturating_sub(reserve(ttl));
    (cut >= ttl / 4 && !cut.is_zero()).then_some(cut)
}

/// The cache stores every process of an app can share (the others live in one process's memory).
pub(crate) const SHARED_STORES: &[&str] = &["database", "redis", "memcached", "file"];

/// This process's view of the shared lock store.
pub(crate) struct Coordinator {
    backend: Arc<dyn LockBackend>,
    /// Identifies this process as the holder of its locks.
    owner: String,
    ttl: Duration,
    store: String,
}

impl std::fmt::Debug for Coordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coordinator")
            .field("store", &self.store)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl Coordinator {
    pub(crate) fn new(backend: Arc<dyn LockBackend>, store: &str, ttl: Duration) -> Self {
        let random = crate::policy::Jitter::from_os().next_u64();
        Self {
            backend,
            owner: process_name(random),
            ttl: ttl.max(Duration::from_secs(1)),
            store: store.to_owned(),
        }
    }

    /// This process's identity in the store (the owner of its locks).
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    /// The lease time to live.
    pub(crate) fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The cut-off of a lease whose holder needs `stop` to stop ([`cutoff`]).
    pub(crate) fn cutoff(&self, stop: Duration) -> Result<Duration, Duration> {
        cutoff(self.ttl, stop)
    }

    /// Who holds lock `name` now.
    pub(crate) async fn holder(&self, name: &str) -> Result<Option<String>, String> {
        self.call("owner", self.backend.owner(name)).await
    }

    /// The store's name, for logs.
    pub(crate) fn store(&self) -> &str {
        &self.store
    }

    /// How often a standby process tries to take a lease.
    pub(crate) fn retry(&self) -> Duration {
        self.ttl / 3
    }

    /// The limit for one store call.
    fn call_limit(&self) -> Duration {
        (self.ttl / 6).max(Duration::from_millis(100))
    }

    async fn call<T>(
        &self,
        op: &str,
        fut: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let limit = self.call_limit();
        tokio::time::timeout(limit, fut)
            .await
            .map_err(|_| format!("the lock store did not answer `{op}` within {limit:?}"))?
    }

    /// Check that the store takes and frees locks (its tables exist, its server answers).
    pub(crate) async fn probe(&self) -> Result<(), String> {
        let name = format!("watchfire:probe:{}", self.owner);
        if !self
            .call(
                "acquire",
                self.backend.acquire(&name, &self.owner, self.ttl),
            )
            .await?
        {
            return Err("a fresh probe lock was refused".to_owned());
        }
        self.call("release", self.backend.release(&name, &self.owner))
            .await
            .map(|_| ())
    }

    /// The message for a stop time the lease cannot cover.
    pub(crate) fn too_short(&self, stop: Duration, needed: Duration) -> String {
        format!(
            "WATCHFIRE_LEASE_TTL={} cannot cover a stop time of {stop:?}: set it to at least {}",
            self.ttl.as_secs(),
            needed.as_secs()
        )
    }

    /// Claim `key` once among every process (a scheduled tick); `true` for the first claimant.
    pub(crate) async fn claim(&self, key: &str, ttl: Duration) -> Result<bool, String> {
        self.call("claim", self.backend.claim(key, &self.owner, ttl))
            .await
    }

    /// Take the lease `name` when it is free, for a holder that needs `stop` to stop: `Ok(Some(lease))` with its
    /// keeper spawned by `spawn` (which answers `false` when it cannot run tasks any more; the lock is then freed
    /// again). A `stop` too long for the time to live is an error (see [`cutoff`]). A lock this process already
    /// holds (an acquire whose answer was lost) is adopted.
    pub(crate) async fn lease(
        self: &Arc<Self>,
        name: &str,
        stop: Duration,
        spawn: impl FnOnce(BoxFuture<'static, ()>) -> bool,
    ) -> Result<Option<Lease>, String> {
        let cut = self
            .cutoff(stop)
            .map_err(|needed| self.too_short(stop, needed))?;
        let sent = Instant::now();
        let taken = self
            .call("acquire", self.backend.acquire(name, &self.owner, self.ttl))
            .await?;
        let sent = if taken {
            sent
        } else {
            // Ours already (an earlier acquire took it but its answer was lost)? Renew and keep it.
            if self.holder(name).await?.as_deref() != Some(self.owner.as_str()) {
                return Ok(None);
            }
            let again = Instant::now();
            if !self
                .call("refresh", self.backend.refresh(name, &self.owner, self.ttl))
                .await?
            {
                return Ok(None);
            }
            debug!(lock = %name, "adopted a lease this process already held");
            again
        };
        let lease = Lease {
            coord: Arc::clone(self),
            name: name.to_owned(),
            lost: CancellationToken::new(),
            done: CancellationToken::new(),
        };
        let keeper = keep(
            Arc::clone(self),
            name.to_owned(),
            Timing { cutoff: cut, sent },
            lease.lost.clone(),
            lease.done.clone(),
        );
        if spawn(Box::pin(keeper)) {
            Ok(Some(lease))
        } else {
            lease.release().await;
            Ok(None)
        }
    }
}

/// A held lease. Dropping it stops the renewals (the lock then expires); [`Lease::release`] frees it at once.
pub(crate) struct Lease {
    coord: Arc<Coordinator>,
    name: String,
    lost: CancellationToken,
    done: CancellationToken,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("name", &self.name)
            .field("lost", &self.lost.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl Lease {
    /// Cancelled when the lease is lost: the holder must stop what it guards.
    pub(crate) fn lost(&self) -> &CancellationToken {
        &self.lost
    }

    /// Stop renewing and free the lock (a failure is logged; the lock then expires).
    pub(crate) async fn release(self) {
        self.done.cancel();
        if self.lost.is_cancelled() {
            return;
        }
        let coord = Arc::clone(&self.coord);
        match coord
            .call("release", coord.backend.release(&self.name, &coord.owner))
            .await
        {
            Ok(_) => debug!(lock = %self.name, "lease released"),
            Err(e) => {
                warn!(lock = %self.name, error = %e, "cannot release a lease; it expires on its own");
            }
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.done.cancel();
    }
}

/// When a lease was last renewed (sent) and how long that holds.
struct Timing {
    cutoff: Duration,
    sent: Instant,
}

/// Renew the lease every `cutoff / 3` until `done`; cancel `lost` when it is taken over, or at the hard deadline
/// `cutoff` after the last successful renewal was sent, whatever call is in flight then.
async fn keep(
    coord: Arc<Coordinator>,
    name: String,
    timing: Timing,
    lost: CancellationToken,
    done: CancellationToken,
) {
    let every = timing.cutoff / 3;
    let retry = (timing.cutoff / 10).max(Duration::from_millis(50));
    let mut deadline = timing.sent + timing.cutoff;
    let mut next = timing.sent + every;
    let give_up = |lost: &CancellationToken| {
        warn!(lock = %name, cutoff = ?timing.cutoff, "lease given up: no renewal reached the lock store in time");
        lost.cancel();
    };
    loop {
        tokio::select! {
            biased;
            () = done.cancelled() => return,
            () = tokio::time::sleep_until(deadline) => return give_up(&lost),
            () = tokio::time::sleep_until(next) => {}
        }
        let sent = Instant::now();
        let answer = tokio::select! {
            biased;
            () = done.cancelled() => return,
            () = tokio::time::sleep_until(deadline) => return give_up(&lost),
            answer = coord.call("refresh", coord.backend.refresh(&name, &coord.owner, coord.ttl)) => answer,
        };
        match answer {
            Ok(true) => {
                deadline = sent + timing.cutoff;
                next = sent + every;
            }
            Ok(false) => {
                warn!(lock = %name, "lease lost: another process holds it now");
                lost.cancel();
                return;
            }
            Err(e) => {
                warn!(lock = %name, error = %e, "cannot renew a lease; retrying");
                next = Instant::now() + retry;
            }
        }
    }
}

/// A fresh process name for a process that does not coordinate.
pub(crate) fn local_process_name() -> String {
    process_name(crate::policy::Jitter::from_os().next_u64())
}

/// A process name: host, process id and a random part (unique per launch).
fn process_name(random: u64) -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_default();
    let host: String = host
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.')
        .take(40)
        .collect();
    let random = random & 0xffff_ffff;
    if host.is_empty() {
        format!("{}-{random:08x}", std::process::id())
    } else {
        format!("{host}:{}-{random:08x}", std::process::id())
    }
}

/// An in-process lock store for tests, on Watchfire's clock (paused Tokio time moves it). Views made with
/// [`FakeLocks::view`] share the locks and stand for processes whose clocks are off by `skew_ms`; each view can be
/// made to fail like an unreachable store.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct FakeLocks {
    state: Arc<std::sync::Mutex<FakeState>>,
    clock: crate::time::Clock,
    skew_ms: i64,
    failing: Arc<std::sync::atomic::AtomicBool>,
    /// How the next refresh calls of this view behave (then normally).
    script: Arc<std::sync::Mutex<std::collections::VecDeque<FakeMode>>>,
    /// Claims never answer.
    claims_hang: Arc<std::sync::atomic::AtomicBool>,
}

/// A scripted behaviour of one refresh call of a [`FakeLocks`] view.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum FakeMode {
    /// Renew in the store at once, answer after this delay.
    Slow(Duration),
    /// Answer an error at once.
    Fail,
    /// Never answer.
    Hang,
}

#[cfg(test)]
#[derive(Default)]
struct FakeState {
    /// name → (owner, expires at in the writer's clock)
    locks: std::collections::HashMap<String, (String, i64)>,
    claims: std::collections::HashMap<String, (String, i64)>,
}

#[cfg(test)]
impl FakeLocks {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::default(),
            clock: crate::time::Clock::starting_at(1_000_000),
            skew_ms: 0,
            failing: Arc::default(),
            script: Arc::default(),
            claims_hang: Arc::default(),
        }
    }

    /// Make the claims of this view never answer (or answer again).
    pub(crate) fn hang_claims(&self, hang: bool) {
        self.claims_hang
            .store(hang, std::sync::atomic::Ordering::SeqCst);
    }

    /// Script the next refresh calls of this view.
    pub(crate) fn script(&self, modes: &[FakeMode]) {
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(modes.iter().copied());
    }

    /// Another process on the same store, its clock `skew_ms` ahead.
    pub(crate) fn view(&self, skew_ms: i64) -> Self {
        Self {
            state: Arc::clone(&self.state),
            clock: self.clock,
            skew_ms,
            failing: Arc::default(),
            script: Arc::default(),
            claims_hang: Arc::default(),
        }
    }

    /// Make every call of this view fail (or work again).
    pub(crate) fn set_failing(&self, failing: bool) {
        self.failing
            .store(failing, std::sync::atomic::Ordering::SeqCst);
    }

    /// Who holds `name` now (by the unskewed clock).
    pub(crate) fn holder(&self, name: &str) -> Option<String> {
        let now = self.clock.now_ms();
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .locks
            .get(name)
            .filter(|(_, until)| *until > now)
            .map(|(owner, _)| owner.clone())
    }

    /// Who holds `name` as a process whose clock runs `skew_ms` ahead sees it.
    pub(crate) fn holder_seen_by(&self, name: &str, skew_ms: i64) -> Option<String> {
        let now = self.clock.now_ms() + skew_ms;
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .locks
            .get(name)
            .filter(|(_, until)| *until > now)
            .map(|(owner, _)| owner.clone())
    }

    /// The claimed keys.
    pub(crate) fn claims(&self) -> Vec<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut keys: Vec<String> = state.claims.keys().cloned().collect();
        keys.sort();
        keys
    }

    fn now(&self) -> i64 {
        self.clock.now_ms().saturating_add(self.skew_ms)
    }

    fn check(&self) -> Result<(), String> {
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            Err("the fake lock store is down".to_owned())
        } else {
            Ok(())
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut FakeState, i64) -> T) -> Result<T, String> {
        self.check()?;
        let now = self.now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&mut state, now))
    }
}

#[cfg(test)]
impl LockBackend for FakeLocks {
    fn acquire<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, String>> {
        let ttl = crate::time::duration_ms(ttl);
        Box::pin(async move {
            self.with(|s, now| {
                if s.locks.get(name).is_some_and(|(_, until)| *until > now) {
                    return false;
                }
                s.locks
                    .insert(name.to_owned(), (owner.to_owned(), now + ttl));
                true
            })
        })
    }

    fn refresh<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, String>> {
        let ttl = crate::time::duration_ms(ttl);
        let mode = self
            .script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();
        Box::pin(async move {
            match mode {
                Some(FakeMode::Fail) => return Err("the fake lock store failed".to_owned()),
                Some(FakeMode::Hang) => std::future::pending::<()>().await,
                _ => {}
            }
            let answer = self.with(|s, now| match s.locks.get_mut(name) {
                Some((o, until)) if o == owner && *until > now => {
                    *until = now + ttl;
                    true
                }
                _ => false,
            });
            if let Some(FakeMode::Slow(delay)) = mode {
                tokio::time::sleep(delay).await;
            }
            answer
        })
    }

    fn release<'a>(&'a self, name: &'a str, owner: &'a str) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            self.with(|s, now| {
                let held = s
                    .locks
                    .get(name)
                    .is_some_and(|(o, until)| o == owner && *until > now);
                if held {
                    s.locks.remove(name);
                }
                held
            })
        })
    }

    fn owner<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            self.with(|s, now| {
                s.locks
                    .get(name)
                    .filter(|(_, until)| *until > now)
                    .map(|(owner, _)| owner.clone())
            })
        })
    }

    fn claim<'a>(
        &'a self,
        key: &'a str,
        owner: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, String>> {
        let ttl = crate::time::duration_ms(ttl);
        let hang = self.claims_hang.load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            if hang {
                std::future::pending::<()>().await;
            }
            self.with(|s, now| {
                if s.claims.get(key).is_some_and(|(_, until)| *until > now) {
                    return false;
                }
                s.claims
                    .insert(key.to_owned(), (owner.to_owned(), now + ttl));
                true
            })
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::time::DurationExt;

    /// The default agent stop time: with a 30 s lease the cut-off is 30 - 10 - 5 = 15 s.
    const STOP: Duration = Duration::from_secs(10);

    #[test]
    fn the_cutoff_leaves_the_stop_time_and_a_skew_reserve() {
        assert_eq!(cutoff(30.secs(), 10.secs()), Ok(15.secs()));
        assert_eq!(cutoff(30.secs(), Duration::ZERO), Ok(25.secs()));
        // A stop time the lease cannot cover names the smallest time to live that can.
        assert_eq!(cutoff(30.secs(), 20.secs()), Err(35.secs()));
        assert!(cutoff(35.secs(), 20.secs()).is_ok());
        assert_eq!(cutoff(6.secs(), 10.secs()), Err(18.secs()));
        assert!(cutoff(18.secs(), 10.secs()).is_ok());
        assert!(cutoff(17.secs(), 10.secs()).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_time_longer_than_the_lease_allows_is_refused() {
        let a = Arc::new(Coordinator::new(
            Arc::new(FakeLocks::new()),
            "fake",
            30.secs(),
        ));
        let err = a.lease("x", 20.secs(), spawn_ok()).await.unwrap_err();
        assert!(err.contains("at least 35"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_acquire_whose_answer_was_lost_is_adopted() {
        let store = FakeLocks::new();
        let a = Arc::new(Coordinator::new(Arc::new(store.view(0)), "fake", 30.secs()));
        // The store took it for A, but A never heard back.
        assert!(store.acquire("x", &a.owner, 30.secs()).await.unwrap());
        let lease = a.lease("x", STOP, spawn_ok()).await.unwrap();
        assert!(lease.is_some(), "the hold is A's: adopted, not waited out");
    }

    fn spawn_ok() -> impl FnOnce(BoxFuture<'static, ()>) -> bool {
        |fut| {
            tokio::spawn(fut);
            true
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_lease_is_renewed_and_excludes_others_until_released() {
        let store = FakeLocks::new();
        let a = Arc::new(Coordinator::new(Arc::new(store.view(0)), "fake", 30.secs()));
        let b = Arc::new(Coordinator::new(Arc::new(store.view(0)), "fake", 30.secs()));
        let lease = a.lease("x", STOP, spawn_ok()).await.unwrap().unwrap();
        assert!(b.lease("x", STOP, spawn_ok()).await.unwrap().is_none());
        // Far beyond the time to live: the keeper renews it.
        tokio::time::sleep(5.mins()).await;
        assert!(!lease.lost().is_cancelled());
        assert_eq!(store.holder("x").as_deref(), Some(a.owner.as_str()));
        assert!(b.lease("x", STOP, spawn_ok()).await.unwrap().is_none());
        lease.release().await;
        assert!(b.lease("x", STOP, spawn_ok()).await.unwrap().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_lease_expires_after_its_time_to_live() {
        let store = FakeLocks::new();
        let a = Arc::new(Coordinator::new(Arc::new(store.view(0)), "fake", 30.secs()));
        let b = Arc::new(Coordinator::new(Arc::new(store.view(0)), "fake", 30.secs()));
        // A crash: the keeper stops, nobody releases.
        drop(a.lease("x", STOP, spawn_ok()).await.unwrap().unwrap());
        tokio::time::sleep(29.secs()).await;
        assert!(b.lease("x", STOP, spawn_ok()).await.unwrap().is_none());
        tokio::time::sleep(2.secs()).await;
        assert!(b.lease("x", STOP, spawn_ok()).await.unwrap().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreachable_store_loses_the_lease_before_others_can_take_it() {
        let store = FakeLocks::new();
        let a_view = store.view(0);
        // B's clock runs 4 s ahead (within the ttl / 6 reserve): it sees A's lock expire 4 s early.
        let b_view = store.view(4_000);
        let a = Arc::new(Coordinator::new(
            Arc::new(a_view.clone()),
            "fake",
            30.secs(),
        ));
        let b = Arc::new(Coordinator::new(Arc::new(b_view), "fake", 30.secs()));
        let lease = a.lease("x", STOP, spawn_ok()).await.unwrap().unwrap();
        tokio::time::sleep(1.secs()).await;
        a_view.set_failing(true);
        let started = Instant::now();
        let lost = lease.lost().clone();
        // A gives up at the cut-off (15 s) after the last renewal it sent (the acquire, 1 s before).
        tokio::time::timeout(30.secs(), lost.cancelled())
            .await
            .unwrap();
        let gave_up = started.elapsed();
        assert!(gave_up <= 14.secs(), "{gave_up:?}");
        // With its stop time (10 s) A is done at 25 s; B, 4 s ahead, sees the lock free at 26 s.
        assert!(b.lease("x", STOP, spawn_ok()).await.unwrap().is_none());
        tokio::time::sleep(10.secs()).await;
        assert!(b.lease("x", STOP, spawn_ok()).await.unwrap().is_none());
        tokio::time::sleep(2.secs()).await;
        assert!(b.lease("x", STOP, spawn_ok()).await.unwrap().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_lease_taken_over_is_lost_at_the_next_renewal() {
        let store = FakeLocks::new();
        let a = Arc::new(Coordinator::new(Arc::new(store.view(0)), "fake", 30.secs()));
        let lease = a.lease("x", STOP, spawn_ok()).await.unwrap().unwrap();
        // Someone forces the lock (e.g. `cache:clear` and another process took it).
        store
            .state
            .lock()
            .unwrap()
            .locks
            .insert("x".into(), ("other".into(), i64::MAX));
        tokio::time::timeout(6.secs(), lease.lost().cancelled())
            .await
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn claims_are_first_come_and_probe_checks_the_store() {
        let store = FakeLocks::new();
        let a = Coordinator::new(Arc::new(store.view(0)), "fake", 30.secs());
        let b_view = store.view(0);
        let b = Coordinator::new(Arc::new(b_view.clone()), "fake", 30.secs());
        assert!(a.claim("tick:1", 1.mins()).await.unwrap());
        assert!(!b.claim("tick:1", 1.mins()).await.unwrap());
        assert!(b.claim("tick:2", 1.mins()).await.unwrap());
        a.probe().await.unwrap();
        b_view.set_failing(true);
        assert!(b.probe().await.is_err());
        assert!(b.claim("tick:3", 1.mins()).await.is_err());
        assert_eq!(store.claims(), ["tick:1", "tick:2"]);
    }

    /// Two processes on a real cache store: exclusion, renewal past the time to live, release, takeover after a
    /// crash (expiry) and claims. Real time with an 8 s lease: its renewals then have a 1.3 s call limit and a 6.7 s
    /// cut-off, so a store write that stalls for a second on a loaded machine does not lose the lease.
    pub(crate) async fn real_store(cache: smeltery_core::cache::Cache) {
        let ttl = Duration::from_secs(8);
        let a = Arc::new(Coordinator::new(
            Arc::new(CacheLocks(cache.clone())),
            "real",
            ttl,
        ));
        let b = Arc::new(Coordinator::new(Arc::new(CacheLocks(cache)), "real", ttl));
        a.probe().await.unwrap();
        let lease = a
            .lease("x", Duration::ZERO, spawn_ok())
            .await
            .unwrap()
            .unwrap();
        assert!(
            b.lease("x", Duration::ZERO, spawn_ok())
                .await
                .unwrap()
                .is_none()
        );
        // Renewed: still A's after one and a half times the time to live.
        tokio::time::sleep(ttl + ttl / 2).await;
        assert!(!lease.lost().is_cancelled());
        assert_eq!(b.holder("x").await.unwrap().as_deref(), Some(a.owner()));
        assert!(
            b.lease("x", Duration::ZERO, spawn_ok())
                .await
                .unwrap()
                .is_none()
        );
        lease.release().await;
        let taken = b
            .lease("x", Duration::ZERO, spawn_ok())
            .await
            .unwrap()
            .unwrap();
        // B crashes: nothing renews it; A takes over once it expired.
        drop(taken);
        assert!(
            a.lease("x", Duration::ZERO, spawn_ok())
                .await
                .unwrap()
                .is_none()
        );
        tokio::time::sleep(ttl + Duration::from_millis(500)).await;
        assert!(
            a.lease("x", Duration::ZERO, spawn_ok())
                .await
                .unwrap()
                .is_some()
        );
        assert!(a.claim("tick:1", Duration::from_secs(60)).await.unwrap());
        assert!(!b.claim("tick:1", Duration::from_secs(60)).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cache_locks_on_the_database_store() {
        let db = smeltery_core::db::Db::connect("sqlite::memory:")
            .await
            .unwrap();
        smeltery_core::cache::migrations::up(&smeltery_core::db::migration::Schema::new(&db))
            .await
            .unwrap();
        let settings = smeltery_core::config::Settings::from_env();
        let cache = smeltery_core::cache::Cache::open("database", &settings, Some(db)).unwrap();
        real_store(cache).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cache_locks_on_the_file_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = smeltery_core::config::Settings::from_env();
        settings.cache_path = dir.path().to_path_buf();
        let cache = smeltery_core::cache::Cache::open("file", &settings, None).unwrap();
        real_store(cache).await;
    }
}
