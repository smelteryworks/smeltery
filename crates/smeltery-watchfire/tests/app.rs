//! The scheduler and the app integration: console commands, `work`, `serve`, `TestApp`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smeltery_core::config::Settings;
use smeltery_core::console::dispatch;
use smeltery_core::db::migration::{Migration, Schema};
use smeltery_core::testing::TestApp;
use smeltery_core::{AppBuilder, Result};
use smeltery_watchfire::prelude::*;
use smeltery_watchfire::testing::Harness;
use smeltery_watchfire::{Agents, Queue};

/// How a generated app's migration delegates.
struct WatchfireTables;

impl Migration for WatchfireTables {
    fn name(&self) -> &'static str {
        "2026_10_03_000000_create_watchfire_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        smeltery_watchfire::migrations::up(schema).await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        smeltery_watchfire::migrations::down(schema).await
    }
}

fn counting_call(
    w: &mut Watchfire,
    name: &str,
    busy: Duration,
    overlap: Overlap,
) -> Arc<AtomicU32> {
    let started = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&started);
    w.schedule()
        .call(name, move |ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                ctx.sleep(busy).await;
                Ok(())
            }
        })
        .every(10.secs())
        .overlap(overlap);
    started
}

#[tokio::test(start_paused = true)]
async fn overlap_modes() {
    let mut w = Watchfire::new();
    let skip = counting_call(&mut w, "skip", 25.secs(), Overlap::Skip);
    let queue = counting_call(&mut w, "queue", 25.secs(), Overlap::Queue);
    let allow = counting_call(&mut w, "allow", 25.secs(), Overlap::Allow);
    let mut h = Harness::from_watchfire(w);
    h.start().await.unwrap();
    assert_eq!(h.agents().names(), ["scheduler"]);
    h.advance(65.secs()).await;
    // Due at 10, 20, …, 60; each call takes 25 s.
    assert_eq!(skip.load(Ordering::SeqCst), 2, "10 and 40");
    assert_eq!(
        queue.load(Ordering::SeqCst),
        3,
        "10, 35 (queued at 20), 60 (queued at 40)"
    );
    assert_eq!(allow.load(Ordering::SeqCst), 6);
    h.shutdown().await;
    let runs = h.runs_of("scheduler").await;
    let calls: Vec<_> = runs.iter().filter(|r| r.job.is_some()).collect();
    assert_eq!(calls.len(), 11);
    assert!(calls.iter().all(|r| r.outcome != RunOutcome::Running));
    assert!(calls.iter().any(|r| r.outcome == RunOutcome::Completed));
    // Calls still running at shutdown stop with the scheduler.
    assert!(calls.iter().any(|r| r.outcome == RunOutcome::Stopped));
}

#[derive(Serialize, Deserialize, Default)]
struct Report {
    kind: String,
}

impl Job for Report {
    const NAME: &'static str = "report";

    async fn handle(&self, ctx: JobCtx) -> std::result::Result<(), AgentError> {
        ctx.counter(&self.kind).inc();
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn scheduled_jobs_and_agents() {
    let mut w = Watchfire::new();
    w.job::<Report>();
    w.schedule()
        .job(Report {
            kind: "daily".into(),
        })
        .every(1.mins());
    let starts = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&starts);
    w.run("sweeper", move |ctx| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            ctx.sleep(30.secs()).await;
            Ok(())
        }
    })
    .autostart(false);
    w.schedule().agent("sweeper").every(1.mins());
    let mut h = Harness::from_watchfire(w).workers(1);
    h.start().await.unwrap();
    assert_eq!(h.state_of("sweeper"), AgentState::Stopped);
    h.advance(3.mins() + 5.secs()).await;
    assert_eq!(
        starts.load(Ordering::SeqCst),
        3,
        "started at 1, 2 and 3 minutes"
    );
    let jobs: Vec<_> = h
        .runs_of("queue#0")
        .await
        .into_iter()
        .filter(|r| r.job.as_deref() == Some("report"))
        .collect();
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[0].counters.get("daily"), Some(&1));
    h.shutdown().await;
}

fn register(w: &mut Watchfire) {
    w.job::<Report>();
    w.schedule()
        .call("cleanup", |ctx| async move {
            ctx.log().info("cleaning");
            Ok(())
        })
        .every_minute();
    w.schedule()
        .call(
            "broken",
            |_ctx| async move { Err(AgentError::msg("no disk")) },
        )
        .every(10.secs());
    w.schedule()
        .job(Report {
            kind: "nightly".into(),
        })
        .every_minute()
        .name("nightly-report");
    w.schedule().agent("sweeper").daily_at("03:00");
    w.run("sweeper", |ctx| async move {
        ctx.cancelled().await;
        Ok(())
    });
}

fn sqlite_builder(dir: &tempfile::TempDir) -> AppBuilder {
    let mut settings = Settings::from_env();
    settings.database_url = format!("sqlite://{}", dir.path().join("app.sqlite").display());
    // `migrate` asks for `--force` in production, the default without `APP_ENV`.
    settings.env = "local".to_owned();
    AppBuilder::new(settings)
        .migrations(|m| {
            m.add(WatchfireTables);
        })
        .agents(register)
}

async fn run(builder: AppBuilder, args: &[&str]) -> (ExitCode, String) {
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let mut out = Vec::new();
    let code = dispatch(builder, &args, &mut out).await.unwrap();
    (code, String::from_utf8(out).unwrap())
}

#[tokio::test]
async fn schedule_commands_and_agent_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = run(sqlite_builder(&dir), &["migrate"]).await;
    assert_eq!(code, ExitCode::SUCCESS, "{out}");

    let (_, out) = run(sqlite_builder(&dir), &["schedule:list"]).await;
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("NAME"), "{out}");
    assert!(
        lines[1].starts_with("cleanup") && lines[1].contains("* * * * *"),
        "{out}"
    );
    assert!(lines[2].contains("every 10s"), "{out}");
    assert!(
        out.contains("agent:sweeper") && out.contains("0 3 * * *"),
        "{out}"
    );

    let (code, out) = run(sqlite_builder(&dir), &["schedule:run"]).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("Ran: cleanup"), "{out}");
    assert!(out.contains("Failed: broken: no disk"), "{out}");
    assert!(out.contains("Dispatched: nightly-report (job #1)"), "{out}");
    assert!(
        !out.contains("sweeper") || out.contains("Skipped: agent:sweeper"),
        "{out}"
    );

    // A second cron minute: its runs are recorded next to the first ones, not over them.
    let (code, _) = run(sqlite_builder(&dir), &["schedule:run"]).await;
    assert_eq!(code, ExitCode::SUCCESS);

    // The scheduled calls were recorded; agents:runs reads them without a running app.
    let (code, out) = run(sqlite_builder(&dir), &["agents:runs", "scheduler"]).await;
    assert_eq!(code, ExitCode::SUCCESS);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("RUN"), "{out}");
    assert_eq!(lines.len(), 5, "two runs per `schedule:run`: {out}");
    assert!(
        out.contains("completed") && out.contains("cleanup"),
        "{out}"
    );
    assert!(out.contains("failed") && out.contains("no disk"), "{out}");

    let (_, out) = run(sqlite_builder(&dir), &["agents:runs", "nobody"]).await;
    assert_eq!(out.trim(), "No runs recorded for `nobody`.");
    let mut out = Vec::new();
    let err = dispatch(sqlite_builder(&dir), &["agents:runs".to_owned()], &mut out)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("usage"), "{err}");

    // help lists the commands.
    let (_, out) = run(sqlite_builder(&dir), &["help"]).await;
    for name in ["work", "agents:runs", "schedule:list", "schedule:run"] {
        assert!(out.contains(name), "{name} missing from help:\n{out}");
    }
}

#[tokio::test]
async fn agents_runs_without_a_database_explains() {
    let builder = AppBuilder::new(Settings::from_env()).agents(|w| {
        w.run("a", |_ctx| async { Ok(()) });
    });
    let mut out = Vec::new();
    let err = dispatch(
        builder,
        &["agents:runs".to_owned(), "a".to_owned()],
        &mut out,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("DATABASE_URL"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn work_runs_agents_until_shutdown_and_exits_cleanly() {
    let ticks = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&ticks);
    let mut settings = Settings::from_env();
    settings.shutdown_timeout = Duration::from_secs(5);
    let builder = AppBuilder::new(settings).agents(move |w| {
        w.every(1.secs(), "ticker", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        w.run("stopper", |ctx| async move {
            ctx.sleep(10.secs()).await;
            ctx.app().shutdown();
            ctx.cancelled().await;
            Ok(())
        });
        w.run("stubborn", |_ctx| async move {
            std::future::pending::<()>().await;
            Ok(())
        })
        .shutdown_timeout(1.hours());
    });
    let started = tokio::time::Instant::now();
    let (code, _) = run(builder, &["work"]).await;
    assert_eq!(code, ExitCode::SUCCESS);
    // 10 s of work, then at most the 5 s budget.
    assert!(started.elapsed() <= 15.secs(), "{:?}", started.elapsed());
    assert!(ticks.load(Ordering::SeqCst) >= 10);
}

#[tokio::test]
async fn work_without_agents_says_so() {
    let (code, out) = run(AppBuilder::new(Settings::from_env()), &["work"]).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert!(out.contains("Nothing to run"), "{out}");
}

#[tokio::test]
async fn serve_runs_web_and_agents_on_one_budget() {
    let built = AppBuilder::new(Settings::from_env())
        .agents(|w| {
            w.run("pinger", |ctx| async move {
                ctx.sleep(Duration::from_millis(50)).await;
                ctx.counter("pings").inc();
                ctx.app().shutdown();
                ctx.cancelled().await;
                Ok(())
            });
        })
        .build()
        .await
        .unwrap();
    let app = built.app.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        smeltery_core::serve_on(built.app, built.router, listener),
    )
    .await
    .unwrap()
    .unwrap();
    let agents = app.service::<Agents>().expect("serve started Watchfire");
    let runs = agents.runs(Some("pinger"), 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, RunOutcome::Stopped);
    assert_eq!(runs[0].counters.get("pings"), Some(&1));
}

#[test]
fn test_app_keeps_watchfire_off_until_asked() {
    let build = |b: AppBuilder| {
        b.migrations(|m| {
            m.add(WatchfireTables);
        })
        .agents(|w| {
            w.job::<Report>();
        })
    };
    let t = TestApp::new(build);
    assert!(t.app().service::<Agents>().is_none());
    // Dispatching works: the job waits in the (database) queue.
    t.block_on(Report::default().dispatch(t.app())).unwrap();
    let queue = t.app().service::<Queue>().unwrap();
    assert_eq!(queue.driver(), "database");
    assert_eq!(t.block_on(queue.stats()).unwrap().pending, 1);

    let t = TestApp::new(build).with_agents();
    let agents = t
        .app()
        .service::<Agents>()
        .expect("with_agents starts Watchfire");
    assert_eq!(agents.names(), ["queue#0", "queue#1"]);
    t.block_on(Report::default().dispatch(t.app())).unwrap();
    let queue = t.app().service::<Queue>().unwrap();
    for _ in 0..100 {
        if t.block_on(queue.stats()).unwrap().pending == 0 {
            break;
        }
        t.block_on(async { tokio::time::sleep(Duration::from_millis(20)).await });
    }
    let stats = t.block_on(queue.stats()).unwrap();
    assert_eq!((stats.pending, stats.reserved), (0, 0));
}

/// Run `serve` (on a free port) with `args` until a boot-time timer shuts it down; whether the agent ran.
async fn serve_and_see_whether_agents_ran(args: &[&str], in_serve: bool) -> bool {
    let ran = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&ran);
    let mut b = AppBuilder::new(Settings::from_env());
    b.settings_mut().key = "watchfire-serve-key-0123456789abcdef".to_owned();
    b.settings_mut().host = "127.0.0.1".to_owned();
    b.settings_mut().port = 0;
    b.settings_mut().shutdown_timeout = Duration::from_secs(5);
    let b = b
        .agents(move |w| {
            w.run("marker", move |ctx| {
                let seen = Arc::clone(&seen);
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    ctx.cancelled().await;
                    Ok(())
                }
            });
        })
        .on_boot(move |app| async move {
            let mut settings = smeltery_watchfire::WatchfireSettings::from_env();
            settings.in_serve = in_serve;
            app.insert_service(settings);
            let stopper = app.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                stopper.shutdown();
            });
            Ok(())
        });
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let mut out = Vec::new();
    let code = tokio::time::timeout(Duration::from_secs(30), dispatch(b, &args, &mut out))
        .await
        .expect("serve stopped")
        .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    ran.load(Ordering::SeqCst) > 0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_runs_the_agents_unless_told_not_to() {
    assert!(serve_and_see_whether_agents_ran(&["serve"], true).await);
    assert!(!serve_and_see_whether_agents_ran(&["serve", "--no-agents"], true).await);
    assert!(!serve_and_see_whether_agents_ran(&["serve"], false).await);
}
