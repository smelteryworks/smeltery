//! Several Watchfire instances on one shared lock store stand for several processes (`serve` and `work`, or
//! several servers), on paused Tokio time with the in-process [`FakeLocks`].

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use smeltery_core::AppBuilder;
use smeltery_core::config::Settings;

use crate::app::{LaunchParts, WatchfireSettings, launch_with};
use crate::coord::{Coordinator, FakeLocks, FakeMode};
use crate::ctx::AgentCtx;
use crate::http::FakeTransport;
use crate::policy::Jitter;
use crate::prelude::*;
use crate::runtime::agent_lock;
use crate::store::{MemoryStore, Store};

/// How many runs are inside the agent at once, and the most ever seen.
#[derive(Clone, Default)]
struct Inside {
    now: Arc<AtomicUsize>,
    max: Arc<AtomicUsize>,
    runs: Arc<AtomicUsize>,
}

impl Inside {
    fn enter(&self) {
        let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
        self.runs.fetch_add(1, Ordering::SeqCst);
    }
    fn leave(&self) {
        self.now.fetch_sub(1, Ordering::SeqCst);
    }
    fn max(&self) -> usize {
        self.max.load(Ordering::SeqCst)
    }
    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

/// One "process": an app and Watchfire on `locks`, sharing `store` with the others.
async fn process(
    locks: &FakeLocks,
    store: &Arc<dyn Store>,
    register: impl FnOnce(&mut Watchfire),
) -> (smeltery_core::App, Agents) {
    process_on(locks, store, None, register).await
}

/// [`process`] with Watchfire's clock taken from `clock` (several processes' clocks agree under paused time).
async fn process_on(
    locks: &FakeLocks,
    store: &Arc<dyn Store>,
    clock: Option<crate::time::Clock>,
    register: impl FnOnce(&mut Watchfire),
) -> (smeltery_core::App, Agents) {
    let app = AppBuilder::new(Settings::from_env())
        .build()
        .await
        .unwrap()
        .app;
    if let Some(clock) = clock {
        app.insert_service(crate::queue::Queue::memory(clock, Duration::from_secs(5)));
    }
    let mut w = Watchfire::new();
    register(&mut w);
    let parts = LaunchParts {
        transport: Arc::new(FakeTransport::new()),
        store: Arc::clone(store),
        jitter: Jitter::seeded(7),
        health_interval: Duration::from_secs(1),
        coord: Some(Arc::new(Coordinator::new(
            Arc::new(locks.clone()),
            "fake",
            Duration::from_secs(30),
        ))),
    };
    let agents = launch_with(&app, w, &WatchfireSettings::from_env(), parts, |_| {})
        .await
        .unwrap();
    (app, agents)
}

fn singleton(inside: &Inside) -> impl FnOnce(&mut Watchfire) {
    let inside = inside.clone();
    move |w: &mut Watchfire| {
        w.run("single", move |ctx| {
            let inside = inside.clone();
            async move {
                inside.enter();
                ctx.cancelled().await;
                inside.leave();
                Ok(())
            }
        });
    }
}

fn state(agents: &Agents, name: &str) -> AgentState {
    agents.status(name).unwrap().state
}

#[tokio::test(start_paused = true)]
async fn a_singleton_runs_in_one_process_and_moves_when_that_one_stops() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let inside = Inside::default();
    let (_a_app, a) = process(&locks.view(0), &store, singleton(&inside)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_b_app, b) = process(&locks.view(0), &store, singleton(&inside)).await;
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(state(&a, "single"), AgentState::Running);
    assert_eq!(state(&b, "single"), AgentState::Standby);
    assert_eq!(inside.runs(), 1);
    // Commands in the standby process are refused; the holder obeys them and keeps the agent.
    assert!(matches!(
        b.stop("single").await,
        Err(crate::Error::Standby { .. })
    ));
    a.pause("single").await.unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(
        state(&b, "single"),
        AgentState::Standby,
        "paused stays paused"
    );
    assert_eq!(inside.runs(), 1);
    a.resume("single").await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The holder shuts down and releases: the other process takes over within its retry interval.
    a.shutdown().await;
    assert_eq!(locks.holder(&agent_lock("single")), None);
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert_eq!(state(&b, "single"), AgentState::Running);
    assert_eq!(inside.runs(), 3);
    assert_eq!(inside.max(), 1, "never two at once");
    b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_holder_cut_off_from_the_store_stops_before_another_process_takes_over() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let inside = Inside::default();
    let a_view = locks.view(0);
    let (_a_app, a) = process(&a_view, &store, singleton(&inside)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    // B's clock runs 8 s ahead of A's.
    let (_b_app, b) = process(&locks.view(8_000), &store, singleton(&inside)).await;
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(state(&a, "single"), AgentState::Running);

    // A can no longer reach the store (a crash looks the same to B: the lock is not renewed).
    a_view.set_failing(true);
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(
        state(&a, "single"),
        AgentState::Standby,
        "A gave the lease up"
    );
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(state(&b, "single"), AgentState::Running, "B took over");
    assert_eq!(inside.max(), 1, "never two at once");
    assert_eq!(inside.runs(), 2);

    // A comes back: B keeps it.
    a_view.set_failing(false);
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(state(&a, "single"), AgentState::Standby);
    assert_eq!(state(&b, "single"), AgentState::Running);
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn the_standby_process_leaves_the_holders_runs_alone() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let inside = Inside::default();
    let (_a_app, a) = process(&locks.view(0), &store, singleton(&inside)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_b_app, b) = process(&locks.view(0), &store, singleton(&inside)).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    // The run A is doing is still `running` in the shared history: B did not mark it interrupted.
    let runs = store.recent_runs(Some("single"), 5).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, RunOutcome::Running);
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn per_process_agents_and_pools() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let register = |w: &mut Watchfire| {
        w.run("everywhere", |ctx| async move {
            ctx.cancelled().await;
            Ok(())
        })
        .per_process();
        w.pool(2, "fetcher", |i| {
            agent_fn(format!("f{i}"), |ctx: AgentCtx| async move {
                ctx.cancelled().await;
                Ok(())
            })
        });
    };
    let (_a_app, a) = process(&locks.view(0), &store, register).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_b_app, b) = process(&locks.view(0), &store, register).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(state(&a, "everywhere"), AgentState::Running);
    assert_eq!(state(&b, "everywhere"), AgentState::Running);
    for member in ["fetcher#0", "fetcher#1"] {
        assert_eq!(state(&a, member), AgentState::Running);
        assert_eq!(state(&b, member), AgentState::Standby);
    }
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn each_scheduled_tick_runs_once_among_processes() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let shared_runs = Arc::new(AtomicUsize::new(0));
    let local_runs = Arc::new(AtomicUsize::new(0));
    let register = {
        let shared_runs = Arc::clone(&shared_runs);
        let local_runs = Arc::clone(&local_runs);
        move |w: &mut Watchfire| {
            let shared_runs = Arc::clone(&shared_runs);
            let local_runs = Arc::clone(&local_runs);
            w.schedule()
                .call("tick", move |_ctx| {
                    let runs = Arc::clone(&shared_runs);
                    async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                })
                .every(1.mins());
            w.schedule()
                .call("local", move |_ctx| {
                    let runs = Arc::clone(&local_runs);
                    async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                })
                .every(1.mins())
                .per_process();
        }
    };
    let (_a_app, a) = process(&locks.view(0), &store, register.clone()).await;
    // B starts 7 s later: with a shared store `every` runs on the same epoch-aligned ticks in both.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let (_b_app, b) = process(&locks.view(0), &store, register).await;
    tokio::time::sleep(Duration::from_secs(5 * 60 + 1)).await;
    let ran = shared_runs.load(Ordering::SeqCst);
    assert!((5..=6).contains(&ran), "{ran} runs of 5 or 6 ticks");
    assert_eq!(
        locks.claims().len(),
        ran,
        "one claim per tick, each run once"
    );
    // `per_process` runs in both.
    assert!(local_runs.load(Ordering::SeqCst) >= 2 * 5);
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_long_call_does_not_overlap_itself_across_processes() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let inside = Inside::default();
    let register = {
        let inside = inside.clone();
        move |w: &mut Watchfire| {
            let inside = inside.clone();
            w.schedule()
                .call("slow", move |_ctx| {
                    let inside = inside.clone();
                    async move {
                        inside.enter();
                        // Three ticks long.
                        tokio::time::sleep(Duration::from_secs(150)).await;
                        inside.leave();
                        Ok(())
                    }
                })
                .every(1.mins());
        }
    };
    let (_a_app, a) = process(&locks.view(0), &store, register.clone()).await;
    let (_b_app, b) = process(&locks.view(0), &store, register).await;
    tokio::time::sleep(Duration::from_secs(10 * 60)).await;
    assert_eq!(inside.max(), 1, "Overlap::Skip holds across processes");
    assert!(inside.runs() >= 3, "{}", inside.runs());
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_slow_then_silent_store_never_frees_the_lock_while_the_holder_still_runs() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let inside = Inside::default();
    let view = locks.view(0);
    // A refresh renews in the store but answers 4.9 s late, the next fails at once, then the store stops answering.
    view.script(&[
        FakeMode::Slow(Duration::from_millis(4_900)),
        FakeMode::Fail,
        FakeMode::Hang,
        FakeMode::Hang,
        FakeMode::Hang,
        FakeMode::Hang,
        FakeMode::Hang,
    ]);
    let slow = inside.clone();
    let (_app, a) = process(&view, &store, move |w: &mut Watchfire| {
        // Takes 9 s to stop once cancelled (its shutdown timeout is the default 10 s).
        w.run("single", move |ctx| {
            let inside = slow.clone();
            async move {
                inside.enter();
                ctx.cancelled().await;
                tokio::time::sleep(Duration::from_secs(9)).await;
                inside.leave();
                Ok(())
            }
        });
    })
    .await;
    let name = agent_lock("single");
    // Whenever another process could take the lock, the holder's run must be over.
    for _ in 0..1_200 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if locks.holder(&name).is_none() {
            assert_eq!(
                inside.now.load(Ordering::SeqCst),
                0,
                "the lock is free while the holder still runs"
            );
        }
    }
    // It gave the lease up, stopped, and took the free lock again (the store answers acquires).
    assert_eq!(inside.runs(), 2);
    a.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn per_process_agents_keep_their_own_runs_and_ended_processes_are_swept() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let register = |w: &mut Watchfire| {
        w.run("everywhere", |ctx| async move {
            ctx.cancelled().await;
            Ok(())
        })
        .per_process();
    };
    let (_a_app, a) = process(&locks.view(0), &store, register).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    // A process that died with a run in progress (it holds no process lease).
    let mut orphan = crate::status::RunRecord::started("everywhere", 7, None, 1);
    orphan.process = "gone:1-dead".into();
    store.upsert_run(&orphan).await.unwrap();
    let (_b_app, b) = process(&locks.view(0), &store, register).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let runs = store.recent_runs(Some("everywhere"), 10).await.unwrap();
    let running: Vec<&str> = runs
        .iter()
        .filter(|r| r.outcome == RunOutcome::Running)
        .map(|r| r.process.as_str())
        .collect();
    // B starting did not mark A's live run interrupted; both runs are recorded, each under its process.
    assert_eq!(running.len(), 2, "{runs:?}");
    assert!(running.iter().all(|p| !p.is_empty() && *p != "gone:1-dead"));
    assert_ne!(running[0], running[1]);
    // The dead process's run is swept.
    let swept = runs.iter().find(|r| r.process == "gone:1-dead").unwrap();
    assert_eq!(swept.outcome, RunOutcome::Interrupted);
    a.shutdown().await;
    b.shutdown().await;
}

/// A temporary SQLite file database with the Watchfire tables: its URL (the directory lives as long as the guard).
pub(crate) async fn sqlite_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path()
            .join("app.sqlite")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let db = smeltery_core::db::Db::connect(&url).await.unwrap();
    crate::migrations::up(&smeltery_core::db::migration::Schema::new(&db))
        .await
        .unwrap();
    (dir, url)
}

/// An app on the database `url` whose dashboard reads the shared tables through `locks` (a web process).
async fn web_process(
    url: &str,
    locks: &FakeLocks,
    register: impl FnOnce(&mut Watchfire),
) -> smeltery_core::App {
    let mut settings = Settings::from_env();
    settings.database_url = url.to_owned();
    let app = AppBuilder::new(settings).build().await.unwrap().app;
    let mut w = Watchfire::new();
    register(&mut w);
    app.insert_service(crate::remote::Registered::of(&w));
    let store = crate::store::DbStore::open(app.db().unwrap()).await;
    crate::remote::set_remote(
        &app,
        crate::remote::Remote {
            store,
            locks: Some(Arc::new(Coordinator::new(
                Arc::new(locks.clone()),
                "fake",
                Duration::from_secs(30),
            ))),
        },
    );
    app
}

/// A `work` process on the database `url`: Watchfire with the database store and the commands poller.
async fn work_process(
    url: &str,
    locks: &FakeLocks,
    register: impl FnOnce(&mut Watchfire),
) -> (smeltery_core::App, Agents, String) {
    let app = web_process(url, locks, |_| {}).await;
    let mut w = Watchfire::new();
    register(&mut w);
    let coord = Arc::new(Coordinator::new(
        Arc::new(locks.clone()),
        "fake",
        Duration::from_secs(30),
    ));
    let owner = coord.owner().to_owned();
    let parts = LaunchParts {
        transport: Arc::new(FakeTransport::new()),
        store: Arc::new(crate::store::DbStore::open(app.db().unwrap()).await),
        jitter: Jitter::seeded(7),
        health_interval: Duration::from_secs(1),
        coord: Some(coord),
    };
    let agents = launch_with(&app, w, &WatchfireSettings::from_env(), parts, |_| {})
        .await
        .unwrap();
    (app, agents, owner)
}

async fn wait_for(mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not met");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_web_process_shows_and_controls_the_agents_of_a_work_process() {
    let (_dir, url) = sqlite_file().await;
    web_process_controls_a_work_process(&url).await;
}

/// The body of `a_web_process_shows_and_controls_the_agents_of_a_work_process`, on the database at `url` (with fresh Watchfire tables).
pub(crate) async fn web_process_controls_a_work_process(url: &str) {
    use crate::remote::{Control, agent_view, control};
    let locks = FakeLocks::new();
    let register = |w: &mut Watchfire| {
        w.run("single", |ctx| async move {
            ctx.log().info("hello from single");
            ctx.cancelled().await;
            Ok(())
        });
    };
    let web = web_process(url, &locks.view(0), register).await;
    let (_work_app, work, owner) = work_process(url, &locks.view(0), register).await;
    wait_for(|| {
        work.status("single")
            .is_ok_and(|s| s.state == AgentState::Running)
    })
    .await;

    // The web process lists the agent from the shared tables, with the process that holds it.
    let view = agent_view(&web).await.unwrap();
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].state, AgentState::Running);
    assert_eq!(view[0].held_by.as_deref(), Some(owner.as_str()));

    // Its commands are carried out by that process, with the honest outcome.
    match control(&web, "single", "pause").await {
        Control::Remote(Ok(status)) => {
            assert_eq!(status.state, AgentState::Paused);
            assert_eq!(status.held_by.as_deref(), Some(owner.as_str()));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(work.status("single").unwrap().state, AgentState::Paused);
    match control(&web, "single", "pause").await {
        Control::Remote(Err((code, e))) => {
            assert_eq!(code, 409);
            assert!(e.contains("not running") || e.contains("paused"), "{e}")
        }
        // Pausing a paused agent answers its status (the runner keeps it paused).
        Control::Remote(Ok(status)) => assert_eq!(status.state, AgentState::Paused),
        other => panic!("{other:?}"),
    }
    match control(&web, "single", "resume").await {
        Control::Remote(Ok(_)) => {}
        other => panic!("{other:?}"),
    }
    match control(&web, "single", "start").await {
        Control::Remote(Err((code, e))) => {
            assert_eq!(code, 409, "the holder's refusal keeps its status");
            assert!(e.contains("already running"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        control(&web, "ghost", "stop").await,
        Control::Local(Err(crate::Error::UnknownAgent { .. }))
    ));
    assert!(matches!(
        control(&web, "single", "explode").await,
        Control::UnknownAction
    ));

    // Its log lines live in the holder's memory: read through the commands table.
    match crate::remote::logs_view(&web, "single").await {
        Some(Ok(lines)) => {
            let lines = lines.as_array().unwrap();
            assert!(
                lines
                    .iter()
                    .any(|l| l["message"] == "hello from single" && l["level"] == "info"),
                "{lines:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    let remote = crate::remote::remote_of(&web).await.unwrap();
    assert_eq!(
        remote.store.commands_with_action("logs").await,
        0,
        "the answer was deleted once read"
    );
    match crate::remote::logs_view(&web, "ghost").await {
        Some(Err((404, _))) => {}
        other => panic!("{other:?}"),
    }

    // Nobody holds it: the command waits, says so, and the next holder carries it out.
    work.shutdown().await;
    match control(&web, "single", "stop").await {
        Control::Queued => {}
        other => panic!("{other:?}"),
    }
    let (_next_app, next, _) = work_process(url, &locks.view(0), register).await;
    wait_for(|| {
        next.status("single")
            .is_ok_and(|s| s.state == AgentState::Stopped)
    })
    .await;
    next.shutdown().await;
}

/// `schedule:run` (system cron) as another process on `locks`: its output for the tasks due now.
async fn cron_run(
    locks: &FakeLocks,
    store: &Arc<dyn Store>,
    clock: crate::time::Clock,
    register: impl FnOnce(&mut Watchfire),
) -> String {
    let app = AppBuilder::new(Settings::from_env())
        .build()
        .await
        .unwrap()
        .app;
    app.insert_service(crate::queue::Queue::memory(clock, Duration::from_secs(5)));
    let mut w = Watchfire::new();
    register(&mut w);
    let parts = LaunchParts {
        transport: Arc::new(FakeTransport::new()),
        store: Arc::clone(store),
        jitter: Jitter::seeded(7),
        health_interval: Duration::from_secs(1),
        coord: Some(Arc::new(Coordinator::new(
            Arc::new(locks.clone()),
            "fake",
            Duration::from_secs(30),
        ))),
    };
    let shared = crate::app::build_shared(
        &app,
        &mut w,
        &WatchfireSettings::from_env(),
        parts,
        tokio_util::sync::CancellationToken::new(),
    );
    let ctx = AgentCtx::detached(
        app.clone(),
        shared,
        "scheduler",
        app.shutdown_token().child_token(),
    );
    let mut out = Vec::new();
    crate::schedule::run_due(&w.schedule, &ctx, &mut out)
        .await
        .unwrap();
    String::from_utf8(out).unwrap()
}

#[tokio::test(start_paused = true)]
async fn cron_schedule_run_next_to_a_scheduler_neither_repeats_a_tick_nor_overlaps_a_call() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let inside = Inside::default();
    let register = {
        let inside = inside.clone();
        move |w: &mut Watchfire| {
            let inside = inside.clone();
            w.schedule()
                .call("slow", move |_ctx| {
                    let inside = inside.clone();
                    async move {
                        inside.enter();
                        tokio::time::sleep(Duration::from_secs(150)).await;
                        inside.leave();
                        Ok(())
                    }
                })
                .every(1.mins());
            w.schedule()
                .call("quick", |_ctx| async move { Ok(()) })
                .every(1.mins());
        }
    };
    let clock = crate::time::Clock::new();
    let (_app, a) = process_on(&locks.view(0), &store, Some(clock), register.clone()).await;
    // To 1 s past A's second tick: A runs `slow` (holding its run lease) and claimed this minute's ticks.
    let now = clock.now_ms();
    let second = (now.div_euclid(60_000) + 2) * 60_000 + 1_000;
    tokio::time::sleep(Duration::from_millis(u64::try_from(second - now).unwrap())).await;
    assert_eq!(inside.now.load(Ordering::SeqCst), 1);
    let out = cron_run(&locks.view(0), &store, clock, register.clone()).await;
    assert!(
        out.contains("Skipped: slow (another process runs it)"),
        "{out}"
    );
    assert!(
        out.contains("Skipped: quick (another process runs it)"),
        "{out}"
    );
    a.shutdown().await;
    // Without A, the next minute: cron runs `quick`; `slow`'s run lease is free again.
    tokio::time::sleep(Duration::from_secs(60)).await;
    let out = cron_run(&locks.view(0), &store, clock, |w: &mut Watchfire| {
        w.schedule()
            .call("quick", |_ctx| async move { Ok(()) })
            .every(1.mins());
    })
    .await;
    assert!(out.contains("Ran: quick"), "{out}");
}

#[tokio::test(start_paused = true)]
async fn a_scheduled_call_stops_when_its_run_lease_is_lost() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let inside = Inside::default();
    let view = locks.view(0);
    let register = {
        let inside = inside.clone();
        move |w: &mut Watchfire| {
            let inside = inside.clone();
            w.schedule()
                .call("slow", move |ctx| {
                    let inside = inside.clone();
                    async move {
                        inside.enter();
                        ctx.cancelled().await;
                        inside.leave();
                        Ok(())
                    }
                })
                .every(1.mins());
        }
    };
    let (_app, a) = process(&view, &store, register).await;
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert_eq!(inside.now.load(Ordering::SeqCst), 1);
    view.set_failing(true);
    let name = crate::schedule::run_lock("slow");
    for _ in 0..600 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if locks.holder(&name).is_none() {
            assert_eq!(
                inside.now.load(Ordering::SeqCst),
                0,
                "the call outlived its lease"
            );
        }
    }
    assert_eq!(inside.now.load(Ordering::SeqCst), 0);
    let runs = store.recent_runs(Some("scheduler"), 5).await.unwrap();
    assert!(
        runs.iter()
            .any(|r| r.error.as_deref() == Some("stopped: the run lease was lost")),
        "{runs:?}"
    );
    a.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_silent_store_does_not_hold_the_scheduler_for_every_due_task() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let view = locks.view(0);
    let register = |w: &mut Watchfire| {
        for name in ["a", "b", "c", "d", "e", "f"] {
            w.schedule()
                .call(name, |_ctx| async move { Ok(()) })
                .every(1.mins());
        }
    };
    let clock = crate::time::Clock::new();
    view.hang_claims(true);
    let (_app, a) = process_on(&view, &store, Some(clock), register).await;
    let now = clock.now_ms();
    let due = (now.div_euclid(60_000) + 1) * 60_000;
    // 1 s into the pass of six due tasks, each claim of which would wait out the store timeout (5 s).
    tokio::time::sleep(Duration::from_millis(
        u64::try_from(due - now + 1_000).unwrap(),
    ))
    .await;
    let asked = tokio::time::Instant::now();
    a.shutdown().await;
    // The pass gave up after the first silent claim: the scheduler stopped promptly, not after six timeouts.
    let took = asked.elapsed();
    assert!(took <= Duration::from_secs(5), "{took:?}");
    let runs = store.recent_runs(Some("scheduler"), 5).await.unwrap();
    assert!(
        runs.iter().all(|r| r.outcome != RunOutcome::Killed),
        "{runs:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_call_killed_at_shutdown_frees_its_run_lease() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let register = |w: &mut Watchfire| {
        w.schedule()
            .call("stubborn", |_ctx| async move {
                // Ignores cancellation.
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(())
            })
            .every(1.mins());
    };
    let (_app, a) = process(&locks.view(0), &store, register).await;
    tokio::time::sleep(Duration::from_secs(61)).await;
    let name = crate::schedule::run_lock("stubborn");
    assert!(locks.holder(&name).is_some());
    a.shutdown().await;
    assert_eq!(locks.holder(&name), None, "released, not left to expire");
}

/// A store whose agent-status writes take `delay` (under the store timeout): a slow database.
struct SlowStore {
    inner: MemoryStore,
    delay: Duration,
    /// Slow only while this is set (always when `None`).
    gate: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl Store for SlowStore {
    fn upsert_agent<'a>(
        &'a self,
        status: &'a crate::status::AgentStatus,
    ) -> smeltery_core::BoxFuture<'a, Result<(), crate::StoreError>> {
        Box::pin(async move {
            if self.gate.as_ref().is_none_or(|g| g.load(Ordering::SeqCst)) {
                tokio::time::sleep(self.delay).await;
            }
            self.inner.upsert_agent(status).await
        })
    }
    fn load_agent<'a>(
        &'a self,
        name: &'a str,
    ) -> smeltery_core::BoxFuture<'a, Result<Option<crate::store::StoredAgent>, crate::StoreError>>
    {
        self.inner.load_agent(name)
    }
    fn upsert_run<'a>(
        &'a self,
        run: &'a crate::status::RunRecord,
    ) -> smeltery_core::BoxFuture<'a, Result<(), crate::StoreError>> {
        self.inner.upsert_run(run)
    }
    fn recent_runs<'a>(
        &'a self,
        agent: Option<&'a str>,
        limit: u32,
    ) -> smeltery_core::BoxFuture<'a, Result<Vec<crate::status::RunRecord>, crate::StoreError>>
    {
        self.inner.recent_runs(agent, limit)
    }
    fn mark_interrupted<'a>(
        &'a self,
        agent: &'a str,
        at_ms: i64,
    ) -> smeltery_core::BoxFuture<'a, Result<Vec<u64>, crate::StoreError>> {
        self.inner.mark_interrupted(agent, at_ms)
    }
    fn running_processes<'a>(
        &'a self,
        except: &'a str,
    ) -> smeltery_core::BoxFuture<'a, Result<Vec<String>, crate::StoreError>> {
        self.inner.running_processes(except)
    }
    fn mark_process_interrupted<'a>(
        &'a self,
        process: &'a str,
        at_ms: i64,
    ) -> smeltery_core::BoxFuture<'a, Result<u64, crate::StoreError>> {
        self.inner.mark_process_interrupted(process, at_ms)
    }
    fn restore_run<'a>(
        &'a self,
        run: &'a crate::status::RunRecord,
    ) -> smeltery_core::BoxFuture<'a, Result<bool, crate::StoreError>> {
        self.inner.restore_run(run)
    }
    fn load_checkpoint<'a>(
        &'a self,
        agent: &'a str,
    ) -> smeltery_core::BoxFuture<'a, Result<Option<String>, crate::StoreError>> {
        self.inner.load_checkpoint(agent)
    }
    fn save_checkpoint<'a>(
        &'a self,
        agent: &'a str,
        data: &'a str,
        at_ms: i64,
    ) -> smeltery_core::BoxFuture<'a, Result<(), crate::StoreError>> {
        self.inner.save_checkpoint(agent, data, at_ms)
    }
}

#[tokio::test(start_paused = true)]
async fn a_slow_status_write_does_not_delay_the_stop_past_the_skew_budget() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(SlowStore {
        inner: MemoryStore::default(),
        delay: Duration::from_millis(4_900),
        gate: None,
    });
    let inside = Inside::default();
    let view = locks.view(0);
    // Every renewal hangs: the lease is lost at its cut-off (15 s after it was taken).
    view.script(&[FakeMode::Hang; 16]);
    let watched = inside.clone();
    let (_app, a) = process(&view, &store, move |w: &mut Watchfire| {
        // Heartbeat-monitored: its status (the heartbeat) is written on most health checks.
        w.run("single", move |ctx| {
            let inside = watched.clone();
            async move {
                inside.enter();
                let mut ticker = ctx.interval(Duration::from_millis(100));
                while ticker.tick().await {}
                // 9 s to stop (the shutdown timeout is the default 10 s).
                tokio::time::sleep(Duration::from_secs(9)).await;
                inside.leave();
                Ok(())
            }
        })
        .heartbeat_timeout(Duration::from_secs(2));
    })
    .await;
    let name = agent_lock("single");
    // A process whose clock runs ttl / 6 - 1 s ahead must never see the lock free while the run is inside.
    for _ in 0..600 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if locks.holder_seen_by(&name, 4_000).is_none() {
            assert_eq!(
                inside.now.load(Ordering::SeqCst),
                0,
                "free (to a process 4 s ahead) while the holder still runs"
            );
        }
    }
    assert!(inside.runs() >= 1);
    a.shutdown().await;
}

/// The records of `run_id` of `agent`.
async fn runs_of(
    store: &Arc<dyn Store>,
    agent: &str,
    run_id: u64,
) -> Vec<crate::status::RunRecord> {
    let runs = store.recent_runs(Some(agent), 100).await.unwrap();
    runs.into_iter().filter(|r| r.run_id == run_id).collect()
}

/// A run that returns while its supervisor writes the agent's status (D-390) is recorded once, with its own
/// outcome, and the restart policy runs once.
#[tokio::test(start_paused = true)]
async fn a_run_that_ends_during_a_status_write_is_recorded_once_and_restarted_once() {
    let locks = FakeLocks::new();
    let slow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let store: Arc<dyn Store> = Arc::new(SlowStore {
        inner: MemoryStore::default(),
        delay: Duration::from_millis(4_900),
        gate: Some(Arc::clone(&slow)),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let (counted, gate) = (Arc::clone(&calls), Arc::clone(&slow));
    let (_app, a) = process(&locks.view(0), &store, move |w: &mut Watchfire| {
        w.run("ender", move |ctx| {
            let n = counted.fetch_add(1, Ordering::SeqCst);
            let gate = Arc::clone(&gate);
            async move {
                if n > 0 {
                    let mut ticker = ctx.interval(Duration::from_millis(100));
                    while ticker.tick().await {}
                    return Ok(());
                }
                // The next health check (in 500 ms) writes this heartbeat, slowly (4.9 s); the run returns
                // meanwhile.
                gate.store(true, Ordering::SeqCst);
                ctx.heartbeat();
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(())
            }
        })
        .heartbeat_timeout(Duration::from_secs(2))
        .restart(Restart::Always);
    })
    .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let first = runs_of(&store, "ender", 1).await;
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].outcome, RunOutcome::Completed, "{first:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one restart");
    assert_eq!(a.status("ender").unwrap().restarts, 1);
    assert_eq!(
        runs_of(&store, "ender", 2).await[0].outcome,
        RunOutcome::Running
    );
    a.shutdown().await;
}

/// A run that returns during a status write, after which the lease is lost before the write finishes, is recorded
/// with its own outcome (`completed`, not `stopped`), at once.
#[tokio::test(start_paused = true)]
async fn a_run_that_ended_before_its_lease_was_lost_is_recorded_as_completed() {
    let locks = FakeLocks::new();
    let slow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let store: Arc<dyn Store> = Arc::new(SlowStore {
        inner: MemoryStore::default(),
        delay: Duration::from_secs(60),
        gate: Some(Arc::clone(&slow)),
    });
    let view = locks.view(0);
    // Every renewal hangs: the lease is lost at its cut-off (15 s after it was taken).
    view.script(&[FakeMode::Hang; 16]);
    let gate = Arc::clone(&slow);
    let (_app, a) = process(&view, &store, move |w: &mut Watchfire| {
        w.run("ender", move |ctx| {
            let gate = Arc::clone(&gate);
            async move {
                let mut ticker = ctx.interval(Duration::from_millis(100));
                let until = tokio::time::Instant::now() + Duration::from_secs(11);
                while tokio::time::Instant::now() < until && ticker.tick().await {}
                // The next health check writes this heartbeat slowly (until the store timeout, 5 s); the run
                // returns at 13 s and the lease is lost at 15 s, both during that write.
                gate.store(true, Ordering::SeqCst);
                ctx.heartbeat();
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok(())
            }
        })
        .heartbeat_timeout(Duration::from_secs(2));
    })
    .await;
    tokio::time::sleep(Duration::from_millis(14_000)).await;
    // Later writes are quick again (the hanging one goes on).
    slow.store(false, Ordering::SeqCst);
    assert_eq!(
        runs_of(&store, "ender", 1).await[0].outcome,
        RunOutcome::Running,
        "the write still hangs"
    );
    // Recorded without waiting for the shutdown timeout (10 s).
    tokio::time::sleep(Duration::from_secs(3)).await;
    let first = runs_of(&store, "ender", 1).await;
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].outcome, RunOutcome::Completed, "{first:?}");
    a.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn runs_wrongly_marked_interrupted_during_an_outage_are_put_back() {
    let locks = FakeLocks::new();
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let register = |w: &mut Watchfire| {
        w.run("everywhere", |ctx| async move {
            ctx.cancelled().await;
            Ok(())
        })
        .per_process();
    };
    let a_view = locks.view(0);
    let (_a_app, a) = process(&a_view, &store, register).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_b_app, b) = process(&locks.view(0), &store, register).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    // A cannot reach the lock store for 70 s; its process lease expires and B's sweep marks A's run interrupted.
    a_view.set_failing(true);
    tokio::time::sleep(Duration::from_secs(70)).await;
    let runs = store.recent_runs(Some("everywhere"), 10).await.unwrap();
    assert!(
        runs.iter().any(|r| r.outcome == RunOutcome::Interrupted),
        "the outage was long enough for a sweep: {runs:?}"
    );
    // A comes back, takes its lease again and records its live run as running.
    a_view.set_failing(false);
    tokio::time::sleep(Duration::from_secs(10)).await;
    let runs = store.recent_runs(Some("everywhere"), 10).await.unwrap();
    assert_eq!(
        runs.iter()
            .filter(|r| r.outcome == RunOutcome::Running)
            .count(),
        2,
        "{runs:?}"
    );
    assert!(
        runs.iter().all(|r| r.outcome == RunOutcome::Running),
        "{runs:?}"
    );
    a.shutdown().await;
    b.shutdown().await;
}

/// One process's view of a shared [`MemoryStore`] whose run writes can fail (a database outage) or, for records
/// that say `running`, be slow (signalling `entered` when such a write starts).
#[derive(Clone, Default)]
struct GatedStore {
    inner: MemoryStore,
    fail_runs: Arc<std::sync::atomic::AtomicBool>,
    slow_running: Arc<std::sync::atomic::AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
}

impl GatedStore {
    async fn gate(&self, running: bool) -> Result<(), crate::StoreError> {
        if self.fail_runs.load(Ordering::SeqCst) {
            return Err(crate::StoreError::new("upsert_run", "fake outage"));
        }
        if running && self.slow_running.load(Ordering::SeqCst) {
            self.entered.notify_one();
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        Ok(())
    }
}

impl Store for GatedStore {
    fn upsert_agent<'a>(
        &'a self,
        status: &'a crate::status::AgentStatus,
    ) -> smeltery_core::BoxFuture<'a, Result<(), crate::StoreError>> {
        self.inner.upsert_agent(status)
    }
    fn load_agent<'a>(
        &'a self,
        name: &'a str,
    ) -> smeltery_core::BoxFuture<'a, Result<Option<crate::store::StoredAgent>, crate::StoreError>>
    {
        self.inner.load_agent(name)
    }
    fn upsert_run<'a>(
        &'a self,
        run: &'a crate::status::RunRecord,
    ) -> smeltery_core::BoxFuture<'a, Result<(), crate::StoreError>> {
        Box::pin(async move {
            self.gate(run.outcome == RunOutcome::Running).await?;
            self.inner.upsert_run(run).await
        })
    }
    fn recent_runs<'a>(
        &'a self,
        agent: Option<&'a str>,
        limit: u32,
    ) -> smeltery_core::BoxFuture<'a, Result<Vec<crate::status::RunRecord>, crate::StoreError>>
    {
        self.inner.recent_runs(agent, limit)
    }
    fn mark_interrupted<'a>(
        &'a self,
        agent: &'a str,
        at_ms: i64,
    ) -> smeltery_core::BoxFuture<'a, Result<Vec<u64>, crate::StoreError>> {
        self.inner.mark_interrupted(agent, at_ms)
    }
    fn running_processes<'a>(
        &'a self,
        except: &'a str,
    ) -> smeltery_core::BoxFuture<'a, Result<Vec<String>, crate::StoreError>> {
        self.inner.running_processes(except)
    }
    fn mark_process_interrupted<'a>(
        &'a self,
        process: &'a str,
        at_ms: i64,
    ) -> smeltery_core::BoxFuture<'a, Result<u64, crate::StoreError>> {
        self.inner.mark_process_interrupted(process, at_ms)
    }
    fn restore_run<'a>(
        &'a self,
        run: &'a crate::status::RunRecord,
    ) -> smeltery_core::BoxFuture<'a, Result<bool, crate::StoreError>> {
        Box::pin(async move {
            self.gate(true).await?;
            self.inner.restore_run(run).await
        })
    }
    fn load_checkpoint<'a>(
        &'a self,
        agent: &'a str,
    ) -> smeltery_core::BoxFuture<'a, Result<Option<String>, crate::StoreError>> {
        self.inner.load_checkpoint(agent)
    }
    fn save_checkpoint<'a>(
        &'a self,
        agent: &'a str,
        data: &'a str,
        at_ms: i64,
    ) -> smeltery_core::BoxFuture<'a, Result<(), crate::StoreError>> {
        self.inner.save_checkpoint(agent, data, at_ms)
    }
}

/// A per-process agent that runs until `end` is notified (or it is stopped), then ends `completed`.
fn ending(end: &Arc<tokio::sync::Notify>) -> impl FnOnce(&mut Watchfire) {
    let end = Arc::clone(end);
    move |w: &mut Watchfire| {
        w.run("ending", move |ctx| {
            let end = Arc::clone(&end);
            async move {
                tokio::select! {
                    () = ctx.cancelled() => {}
                    () = end.notified() => {}
                }
                Ok(())
            }
        })
        .per_process()
        .restart(Restart::Never);
    }
}

type Process = (smeltery_core::App, Agents);

/// Process A (on `a_store`, running [`ending`]) and process B (no agents, on the plain shared store). A is then cut
/// off from the lock store for 70 s, so B's sweep marks A's run interrupted. Returns A's view of the locks and both
/// processes.
async fn outage_setup(
    locks: &FakeLocks,
    a_store: &GatedStore,
    end: &Arc<tokio::sync::Notify>,
) -> (FakeLocks, Process, Process) {
    let a_view = locks.view(0);
    let shared: Arc<dyn Store> = Arc::new(a_store.inner.clone());
    let a_dyn: Arc<dyn Store> = Arc::new(a_store.clone());
    let a = process(&a_view, &a_dyn, ending(end)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let b = process(&locks.view(0), &shared, |_| {}).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    a_view.set_failing(true);
    tokio::time::sleep(Duration::from_secs(70)).await;
    let runs = shared.recent_runs(Some("ending"), 10).await.unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(
        runs[0].outcome,
        RunOutcome::Interrupted,
        "the outage was long enough for B's sweep"
    );
    (a_view, a, b)
}

#[tokio::test(start_paused = true)]
async fn a_run_that_ends_while_its_record_is_restored_keeps_its_outcome() {
    let locks = FakeLocks::new();
    let a_store = GatedStore::default();
    let end = Arc::new(tokio::sync::Notify::new());
    let (a_view, (_a_app, a), (_b_app, b)) = outage_setup(&locks, &a_store, &end).await;
    // A comes back; its restore of the run is slow, and the run ends while the restore is under way.
    a_store.slow_running.store(true, Ordering::SeqCst);
    a_view.set_failing(false);
    tokio::time::timeout(Duration::from_secs(30), a_store.entered.notified())
        .await
        .expect("A restores its run after the outage");
    end.notify_waiters();
    tokio::time::sleep(Duration::from_secs(10)).await;
    a_store.slow_running.store(false, Ordering::SeqCst);
    let runs = a_store.inner.recent_runs(Some("ending"), 10).await.unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(
        runs[0].outcome,
        RunOutcome::Completed,
        "the restore must not overwrite the final outcome: {runs:?}"
    );
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_final_run_record_that_failed_during_an_outage_is_written_after_it() {
    let locks = FakeLocks::new();
    let a_store = GatedStore::default();
    let end = Arc::new(tokio::sync::Notify::new());
    let (a_view, (_a_app, a), (_b_app, b)) = outage_setup(&locks, &a_store, &end).await;
    // The database is down for A too when the run ends: its final record cannot be written.
    a_store.fail_runs.store(true, Ordering::SeqCst);
    end.notify_waiters();
    tokio::time::sleep(Duration::from_secs(1)).await;
    a_store.fail_runs.store(false, Ordering::SeqCst);
    a_view.set_failing(false);
    tokio::time::sleep(Duration::from_secs(10)).await;
    let runs = a_store.inner.recent_runs(Some("ending"), 10).await.unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].outcome, RunOutcome::Completed, "{runs:?}");
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_final_run_record_that_failed_without_a_lease_lapse_is_written_at_the_next_sweep() {
    let locks = FakeLocks::new();
    let a_store = GatedStore::default();
    let a_dyn: Arc<dyn Store> = Arc::new(a_store.clone());
    let end = Arc::new(tokio::sync::Notify::new());
    let (_a_app, a) = process(&locks.view(0), &a_dyn, ending(&end)).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    // Only the database fails, briefly, as the run ends; the lock store answers throughout.
    a_store.fail_runs.store(true, Ordering::SeqCst);
    end.notify_waiters();
    tokio::time::sleep(Duration::from_secs(1)).await;
    a_store.fail_runs.store(false, Ordering::SeqCst);
    let runs = a_store.inner.recent_runs(Some("ending"), 10).await.unwrap();
    assert_eq!(runs[0].outcome, RunOutcome::Running, "not written yet");
    // The process keeper's next pass (every lease time to live, 30 s) writes it.
    tokio::time::sleep(Duration::from_secs(31)).await;
    let runs = a_store.inner.recent_runs(Some("ending"), 10).await.unwrap();
    assert_eq!(runs[0].outcome, RunOutcome::Completed, "{runs:?}");
    a.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_remote_stop_is_reported_as_being_carried_out_and_the_queue_is_bounded() {
    let (_dir, url) = sqlite_file().await;
    slow_remote_stop_and_bounded_queue(&url).await;
}

/// The body of `a_slow_remote_stop_is_reported_as_being_carried_out_and_the_queue_is_bounded`, on the database at `url` (with fresh Watchfire tables).
pub(crate) async fn slow_remote_stop_and_bounded_queue(url: &str) {
    use crate::remote::{Control, control};
    let locks = FakeLocks::new();
    let register = |w: &mut Watchfire| {
        // Needs 7 s to stop: longer than the 5 s a command waits.
        w.run("slow", |ctx| async move {
            ctx.cancelled().await;
            tokio::time::sleep(Duration::from_secs(7)).await;
            Ok(())
        });
    };
    let web = web_process(url, &locks.view(0), register).await;
    let (_work_app, work, owner) = work_process(url, &locks.view(0), register).await;
    wait_for(|| {
        work.status("slow")
            .is_ok_and(|s| s.state == AgentState::Running)
    })
    .await;
    match control(&web, "slow", "stop").await {
        Control::Taken(process) => assert_eq!(process, owner),
        other => panic!("{other:?}"),
    }
    wait_for(|| {
        work.status("slow")
            .is_ok_and(|s| s.state == AgentState::Stopped)
    })
    .await;
    work.shutdown().await;

    // Nobody holds it: 20 commands wait, the 21st is refused as a full queue (not as "pending").
    let remote = crate::remote::remote_of(&web).await.unwrap();
    let now = remote.store.db_now().await.unwrap();
    for _ in 0..20 {
        remote
            .store
            .command_request("slow", "start", now, 20)
            .await
            .unwrap()
            .unwrap();
    }
    match control(&web, "slow", "start").await {
        Control::QueueFull(why) => assert!(why.contains("20 commands"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_stop_does_not_hold_back_another_agents_command() {
    let (_dir, url) = sqlite_file().await;
    slow_stop_does_not_hold_back_others(&url).await;
}

/// The body of `a_slow_stop_does_not_hold_back_another_agents_command`, on the database at `url` (with fresh Watchfire tables).
pub(crate) async fn slow_stop_does_not_hold_back_others(url: &str) {
    use crate::remote::{Control, control};
    let locks = FakeLocks::new();
    let register = |w: &mut Watchfire| {
        // Needs 7 s to stop.
        w.run("slow", |ctx| async move {
            ctx.cancelled().await;
            tokio::time::sleep(Duration::from_secs(7)).await;
            Ok(())
        });
        w.run("quick", |ctx| async move {
            ctx.cancelled().await;
            Ok(())
        });
    };
    let web = web_process(url, &locks.view(0), register).await;
    let (_work_app, work, _owner) = work_process(url, &locks.view(0), register).await;
    wait_for(|| {
        ["slow", "quick"].iter().all(|name| {
            work.status(name)
                .is_ok_and(|s| s.state == AgentState::Running)
        })
    })
    .await;
    let slow = control(&web, "slow", "stop");
    let quick = async {
        // The holder is busy stopping `slow` by now.
        tokio::time::sleep(Duration::from_millis(2_500)).await;
        let asked = std::time::Instant::now();
        let answer = control(&web, "quick", "pause").await;
        (answer, asked.elapsed())
    };
    let (slow, (quick, took)) = tokio::join!(slow, quick);
    assert!(matches!(slow, Control::Taken(_)), "{slow:?}");
    match quick {
        Control::Remote(Ok(status)) => assert_eq!(status.state, AgentState::Paused),
        other => panic!("held back behind the slow stop: {other:?}"),
    }
    assert!(took < Duration::from_secs(4), "{took:?}");
    work.shutdown().await;
}

/// Polls command `id` until it has an outcome: when it was taken and when it finished, after `start`.
async fn watch_command(
    store: &crate::store::DbStore,
    id: i64,
    start: std::time::Instant,
) -> (Duration, Duration) {
    let mut taken = None;
    for _ in 0..600 {
        let state = store.command_state(id).await.unwrap();
        if taken.is_none() && state.taken_by.is_some() {
            taken = Some(start.elapsed());
        }
        if state.outcome.is_some() {
            return (taken.unwrap_or_default(), start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("command {id} never finished");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outcome_is_recorded_while_the_poller_waits_on_the_database() {
    let (_dir, url) = sqlite_file().await;
    let locks = FakeLocks::new();
    let register = |w: &mut Watchfire| {
        w.run("stopper", |ctx| async move {
            ctx.cancelled().await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(())
        });
        // Another agent held here: the poller keeps looking for its commands while `stopper`'s stop runs.
        w.run("idle", |ctx| async move {
            ctx.cancelled().await;
            Ok(())
        });
    };
    let (work_app, work, _owner) = work_process(&url, &locks.view(0), register).await;
    wait_for(|| {
        ["stopper", "idle"].iter().all(|name| {
            work.status(name)
                .is_ok_and(|s| s.state == AgentState::Running)
        })
    })
    .await;
    let remote = crate::remote::remote_of(&work_app).await.unwrap();
    // Every look for new commands takes 2.5 s: the poller is in its own database calls most of the time.
    remote.store.pending_delay_ms.store(2_500, Ordering::SeqCst);
    let now = remote.store.db_now().await.unwrap();
    let id = remote
        .store
        .command_request("stopper", "stop", now, 20)
        .await
        .unwrap()
        .unwrap();
    let (taken, done) = watch_command(&remote.store, id, std::time::Instant::now()).await;
    let outcome = remote
        .store
        .command_state(id)
        .await
        .unwrap()
        .outcome
        .unwrap();
    assert!(outcome.ok, "{}", outcome.result);
    assert!(
        done - taken < Duration::from_millis(1_500),
        "taken at {taken:?}, recorded at {done:?}: the outcome waited for the poller"
    );
    remote.store.pending_delay_ms.store(0, Ordering::SeqCst);
    work.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_cut_short_by_shutdown_are_closed_at_once() {
    let (_dir, url) = sqlite_file().await;
    let locks = FakeLocks::new();
    let register = |w: &mut Watchfire| {
        // Needs 7 s to stop.
        w.run("slow", |ctx| async move {
            ctx.cancelled().await;
            tokio::time::sleep(Duration::from_secs(7)).await;
            Ok(())
        });
    };
    let (work_app, work, _owner) = work_process(&url, &locks.view(0), register).await;
    wait_for(|| {
        work.status("slow")
            .is_ok_and(|s| s.state == AgentState::Running)
    })
    .await;
    let remote = crate::remote::remote_of(&work_app).await.unwrap();
    let now = remote.store.db_now().await.unwrap();
    let id = remote
        .store
        .command_request("slow", "stop", now, 20)
        .await
        .unwrap()
        .unwrap();
    for _ in 0..200 {
        if remote
            .store
            .command_state(id)
            .await
            .unwrap()
            .taken_by
            .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        remote
            .store
            .command_state(id)
            .await
            .unwrap()
            .taken_by
            .is_some()
    );
    // The process shuts down while it is stopping the agent for the command.
    work.shutdown().await;
    let outcome = remote
        .store
        .command_state(id)
        .await
        .unwrap()
        .outcome
        .expect("closed at shutdown, not left for the 11-minute grace");
    assert!(!outcome.ok);
    assert_eq!(outcome.code, Some(503));
    assert_eq!(outcome.result, crate::remote::SHUT_DOWN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_whose_outcome_is_being_written_at_shutdown_keeps_it() {
    let (_dir, url) = sqlite_file().await;
    let locks = FakeLocks::new();
    let register = |w: &mut Watchfire| {
        w.run("quick", |ctx| async move {
            ctx.cancelled().await;
            Ok(())
        });
    };
    let (work_app, work, _owner) = work_process(&url, &locks.view(0), register).await;
    wait_for(|| {
        work.status("quick")
            .is_ok_and(|s| s.state == AgentState::Running)
    })
    .await;
    let remote = crate::remote::remote_of(&work_app).await.unwrap();
    // Recording the outcome takes a moment: the shutdown below starts while it is under way.
    remote.store.finish_delay_ms.store(400, Ordering::SeqCst);
    let now = remote.store.db_now().await.unwrap();
    let id = remote
        .store
        .command_request("quick", "stop", now, 20)
        .await
        .unwrap()
        .unwrap();
    wait_for(|| {
        work.status("quick")
            .is_ok_and(|s| s.state == AgentState::Stopped)
    })
    .await;
    work.shutdown().await;
    let outcome = remote
        .store
        .command_state(id)
        .await
        .unwrap()
        .outcome
        .unwrap();
    assert!(outcome.ok, "{}", outcome.result);
    assert_eq!(outcome.result, "stopped");
}

#[tokio::test]
async fn the_shutdown_close_gives_up_on_a_silent_database_within_its_deadline() {
    let db = smeltery_core::db::Db::connect("sqlite::memory:")
        .await
        .unwrap();
    crate::migrations::up(&smeltery_core::db::migration::Schema::new(&db))
        .await
        .unwrap();
    let store = crate::store::DbStore::open(db).await;
    let now = store.db_now().await.unwrap();
    let mut ids = Vec::new();
    for _ in 0..8 {
        let id = store
            .command_request("a", "stop", now, 20)
            .await
            .unwrap()
            .unwrap();
        assert!(store.command_take(id, "host:1-p").await.unwrap());
        ids.push(id);
    }
    // The database stops answering.
    store.finish_delay_ms.store(60_000, Ordering::SeqCst);
    let started = std::time::Instant::now();
    assert!(!crate::remote::close_unfinished(&store, "host:1-p", &ids).await);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[test]
fn logs_answers_keep_the_newest_lines_that_fit() {
    let lines: Vec<crate::status::LogLine> = (0..200)
        .map(|i| crate::status::LogLine {
            at_ms: i,
            level: "info",
            run_id: 1,
            message: format!("line {i} {}", "x".repeat(400)),
        })
        .collect();
    let json = crate::remote::bounded_logs(&lines, 60_000);
    assert!(json.len() <= 60_000, "{}", json.len());
    let kept: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert!(kept.len() > 100, "{}", kept.len());
    assert_eq!(kept.last().unwrap()["at_ms"], 199, "the newest stays");
    assert!(crate::remote::bounded_logs(&lines[..2], 60_000).len() < 2_000);
    assert_eq!(crate::remote::bounded_logs(&[], 10), "[]");
}

#[tokio::test]
async fn commands_lapse_honestly_by_the_database_clock() {
    let db = smeltery_core::db::Db::connect("sqlite::memory:")
        .await
        .unwrap();
    crate::migrations::up(&smeltery_core::db::migration::Schema::new(&db))
        .await
        .unwrap();
    let store = crate::store::DbStore::open(db).await;
    let now = store.db_now().await.unwrap();
    assert!(
        (now - crate::time::system_ms()).abs() < 5_000,
        "the database clock"
    );
    let untaken = store
        .command_request("a", "stop", now - 61_000, 20)
        .await
        .unwrap()
        .unwrap();
    let taken = store
        .command_request("a", "stop", now - 61_000, 20)
        .await
        .unwrap()
        .unwrap();
    assert!(store.command_take(taken, "host:1-ab").await.unwrap());
    let ttl = 60_000;
    let grace = 600_000;
    store
        .commands_lapse(
            now - ttl,
            now - ttl - grace,
            now - 86_400_000,
            ("logs", now - ttl),
            now,
        )
        .await
        .unwrap();
    let lapsed = store.command_state(untaken).await.unwrap();
    let outcome = lapsed.outcome.unwrap();
    assert!(!outcome.ok);
    assert_eq!(outcome.code, Some(504));
    // Taken a minute ago: its taker may still be stopping the agent; not closed yet.
    let still = store.command_state(taken).await.unwrap();
    assert!(still.outcome.is_none());
    assert_eq!(still.taken_by.as_deref(), Some("host:1-ab"));
    // Well past the grace: closed as "outcome unknown", not as "not carried out".
    store
        .commands_lapse(now, now, now - 86_400_000, ("logs", now - ttl), now)
        .await
        .unwrap();
    let closed = store.command_state(taken).await.unwrap().outcome.unwrap();
    assert!(
        closed.result.contains("outcome was never recorded"),
        "{}",
        closed.result
    );
}

#[tokio::test(start_paused = true)]
async fn a_lease_too_short_for_scheduled_calls_is_refused_at_launch() {
    let app = AppBuilder::new(Settings::from_env())
        .build()
        .await
        .unwrap()
        .app;
    let mut w = Watchfire::new();
    w.schedule()
        .call("nightly", |_ctx| async move { Ok(()) })
        .every(1.mins());
    let parts = LaunchParts {
        transport: Arc::new(FakeTransport::new()),
        store: Arc::new(MemoryStore::default()),
        jitter: Jitter::seeded(7),
        health_interval: Duration::from_secs(1),
        coord: Some(Arc::new(Coordinator::new(
            Arc::new(FakeLocks::new()),
            "fake",
            Duration::from_secs(10),
        ))),
    };
    let err = launch_with(&app, w, &WatchfireSettings::from_env(), parts, |_| {})
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("nightly") && text.contains("at least 18"),
        "{text}"
    );
}

#[tokio::test(start_paused = true)]
async fn schedule_run_refuses_a_lease_too_short_for_its_calls_too() {
    let app = AppBuilder::new(Settings::from_env())
        .build()
        .await
        .unwrap()
        .app;
    let ran = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&ran);
    let mut w = Watchfire::new();
    w.schedule()
        .call("nightly", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .every(1.mins());
    let parts = LaunchParts {
        transport: Arc::new(FakeTransport::new()),
        store: Arc::new(MemoryStore::default()),
        jitter: Jitter::seeded(7),
        health_interval: Duration::from_secs(1),
        coord: Some(Arc::new(Coordinator::new(
            Arc::new(FakeLocks::new()),
            "fake",
            Duration::from_secs(10),
        ))),
    };
    let err = crate::app::schedule_run(&app, w, &WatchfireSettings::from_env(), parts)
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("nightly") && text.contains("at least 18"),
        "{text}"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}
