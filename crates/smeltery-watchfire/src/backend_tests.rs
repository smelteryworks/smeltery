//! Watchfire's SQL against real databases, one shared suite per backend: SQLite here, PostgreSQL and MySQL when
//! built with the test-only feature `backend-tests` and `DATABASE_URL_PG` / `DATABASE_URL_MYSQL` point at a server
//! (`cargo test -p smeltery-watchfire --features backend-tests --lib backend_tests -- --ignored`).
//!
//! The suite covers what only a type-strict backend can prove: the migrations (`up`, and `up_multi_process` on a
//! database an older `up` created, re-run included), the database clock, the commands table's whole lifecycle,
//! process-keyed run writes, the sweep and its conditional undo, the agents table read by other processes, commands
//! carried between two processes, and leases and claims through the `database` cache store.
//!
//! The Watchfire tables have fixed names, so each run drops and recreates them (and the cache tables), and every
//! backend runs the whole suite in ONE test: the steps never share tables with another test at the same time. A
//! fresh database and one used by an earlier run both work.

use smeltery_core::cache::Cache;
use smeltery_core::db::migration::Schema;
use smeltery_core::db::{Backend, Db};

use crate::migrations;
use crate::policy::Restart;
use crate::status::{AgentState, AgentStatus, RunRecord};
use crate::store::{COMMANDS, DbStore, RUNS, Store};

/// Drop every Watchfire table and create them again.
async fn fresh_tables(db: &Db) {
    let schema = Schema::new(db);
    migrations::down(&schema).await.unwrap();
    migrations::up(&schema).await.unwrap();
}

/// `up_multi_process` on the runs table as `up` created it before several-process support.
async fn upgrade_from_the_old_schema(db: &Db) {
    let schema = Schema::new(db);
    migrations::down(&schema).await.unwrap();
    schema
        .create(RUNS, |t| {
            t.id();
            t.string("agent").index();
            t.big_integer("run_id");
            t.string("job").nullable();
            t.big_integer("started_at").index();
            t.big_integer("ended_at").nullable();
            t.string_len("outcome", 32).index();
            t.text("error").nullable();
            t.text("counters").nullable();
        })
        .await
        .unwrap();
    let old = DbStore::open(db.clone()).await;
    assert!(!old.processes());
    old.upsert_run(&RunRecord::started("a", 1, None, 1))
        .await
        .unwrap();
    assert_eq!(old.recent_runs(Some("a"), 5).await.unwrap().len(), 1);

    if db.backend() == Backend::MySql {
        // MySQL commits DDL at once: a run that added the column and then failed is re-run, and finds the column.
        schema
            .table(RUNS, |t| {
                t.string("process").default("");
            })
            .await
            .unwrap();
    }
    migrations::up_multi_process(&schema).await.unwrap();
    // Run again (a second app migration, or a re-run): nothing to do.
    migrations::up_multi_process(&schema).await.unwrap();
    assert!(schema.has_table(COMMANDS).await.unwrap());

    let new = DbStore::open(db.clone()).await;
    assert!(new.processes());
    let mut run = RunRecord::started("a", 1, None, 2);
    run.process = "host:1-p".into();
    new.upsert_run(&run).await.unwrap();
    let runs = new.recent_runs(Some("a"), 5).await.unwrap();
    assert_eq!(runs.len(), 2, "the old row and the new one: {runs:?}");
    assert!(runs.iter().any(|r| r.process.is_empty()), "{runs:?}");
    assert!(runs.iter().any(|r| r.process == "host:1-p"), "{runs:?}");

    migrations::down_multi_process(&schema).await.unwrap();
    assert!(!schema.has_table(COMMANDS).await.unwrap());
    assert!(!DbStore::open(db.clone()).await.processes());
    migrations::down(&schema).await.unwrap();
}

/// The database clock, Unix milliseconds, agrees with this machine's.
async fn clock(store: &DbStore) {
    let now = store.db_now().await.unwrap();
    let here = crate::time::system_ms();
    assert!((now - here).abs() < 5_000, "database {now}, here {here}");
    let later = store.db_now().await.unwrap();
    assert!(later >= now, "{later} < {now}");
}

/// Request, count, list, take, finish, both lapse paths and the clean-up of finished commands.
async fn commands(store: &DbStore) {
    let now = store.db_now().await.unwrap();
    let mut ids = Vec::new();
    for action in ["stop", "pause", "resume"] {
        ids.push(
            store
                .command_request("a", action, now, 3)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 3, "three rows, three ids: {ids:?}");
    let full = store.command_request("a", "stop", now, 3).await.unwrap();
    assert!(
        full.as_ref()
            .is_err_and(|why| why.contains("3 commands for `a`")),
        "{full:?}"
    );
    // Another agent has its own limit.
    let other = store
        .command_request("b", "start", now, 3)
        .await
        .unwrap()
        .unwrap();

    let pending = store
        .commands_pending(&["a".to_owned(), "b".to_owned()], now - 60_000)
        .await
        .unwrap();
    assert_eq!(pending.len(), 4, "{pending:?}");
    assert!(
        pending.windows(2).all(|w| w[0].0 < w[1].0),
        "oldest first: {pending:?}"
    );
    let (first, _, _) = pending[0].clone();
    assert!(pending.contains(&(other, "b".into(), "start".into())));

    // Taken once: a compare-and-set.
    assert!(store.command_take(first, "host:1-p").await.unwrap());
    assert!(!store.command_take(first, "host:2-q").await.unwrap());
    let state = store.command_state(first).await.unwrap();
    assert_eq!(state.taken_by.as_deref(), Some("host:1-p"));
    assert!(state.outcome.is_none());

    // A refusal keeps its HTTP status; a second finish does not overwrite the first.
    let done = store.db_now().await.unwrap();
    store
        .command_finish(first, false, Some(409), "`a` is already running", done)
        .await
        .unwrap();
    store
        .command_finish(first, true, None, "running", done)
        .await
        .unwrap();
    let outcome = store.command_state(first).await.unwrap().outcome.unwrap();
    assert!(!outcome.ok);
    assert_eq!(outcome.code, Some(409));
    assert_eq!(outcome.result, "`a` is already running");

    let second = ids.iter().copied().find(|id| *id != first).unwrap();
    assert!(store.command_take(second, "host:1-p").await.unwrap());
    store
        .command_finish(second, true, None, "stopped", done)
        .await
        .unwrap();
    let outcome = store.command_state(second).await.unwrap().outcome.unwrap();
    assert!(outcome.ok);
    assert_eq!(outcome.code, None);
    assert_eq!(outcome.result, "stopped");

    // Taken and finished rows are no longer pending.
    let pending = store
        .commands_pending(&["a".to_owned()], now - 60_000)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    // Requested before `since`: not pending either.
    assert!(
        store
            .commands_pending(&["a".to_owned()], now + 60_000)
            .await
            .unwrap()
            .is_empty()
    );

    // Lapsing, by the database clock: an untaken row after a minute, a taken one only after the grace too.
    let old = now - 61_000;
    let untaken = store
        .command_request("c", "stop", old, 20)
        .await
        .unwrap()
        .unwrap();
    let taken = store
        .command_request("c", "stop", old, 20)
        .await
        .unwrap()
        .unwrap();
    assert!(store.command_take(taken, "host:1-p").await.unwrap());
    let now = store.db_now().await.unwrap();
    store
        .commands_lapse(
            now - 60_000,
            now - 660_000,
            now - 86_400_000,
            ("logs", now - 60_000),
            now,
        )
        .await
        .unwrap();
    let lapsed = store.command_state(untaken).await.unwrap().outcome.unwrap();
    assert!(!lapsed.ok);
    assert_eq!(lapsed.code, Some(504));
    assert!(store.command_state(taken).await.unwrap().outcome.is_none());
    store
        .commands_lapse(
            now - 60_000,
            now,
            now - 86_400_000,
            ("logs", now - 60_000),
            now,
        )
        .await
        .unwrap();
    let closed = store.command_state(taken).await.unwrap().outcome.unwrap();
    assert!(!closed.ok);
    assert_eq!(closed.code, None);
    assert!(
        closed.result.contains("never recorded"),
        "{}",
        closed.result
    );

    // Finished `logs` answers go after a minute; other finished rows stay for the day.
    let logs = store
        .command_request("d", "logs", now - 120_000, 20)
        .await
        .unwrap()
        .unwrap();
    assert!(store.command_take(logs, "host:1-p").await.unwrap());
    store
        .command_finish(logs, true, None, "[]", now - 90_000)
        .await
        .unwrap();
    store
        .commands_lapse(
            now - 60_000,
            now - 660_000,
            now - 86_400_000,
            ("logs", now - 60_000),
            now,
        )
        .await
        .unwrap();
    assert!(
        store.command_state(logs).await.is_err(),
        "the log lines are gone"
    );
    assert!(store.command_state(first).await.is_ok(), "kept for the day");

    // The shutdown close: only rows this process took and did not finish, in one statement.
    let mine = store
        .command_request("e", "stop", now, 20)
        .await
        .unwrap()
        .unwrap();
    let theirs = store
        .command_request("e", "stop", now, 20)
        .await
        .unwrap()
        .unwrap();
    let nobodys = store
        .command_request("e", "stop", now, 20)
        .await
        .unwrap()
        .unwrap();
    assert!(store.command_take(mine, "host:1-p").await.unwrap());
    assert!(store.command_take(theirs, "host:2-q").await.unwrap());
    assert!(
        crate::remote::close_unfinished(store, "host:1-p", &[mine, theirs, nobodys, first]).await
    );
    let closed = store.command_state(mine).await.unwrap().outcome.unwrap();
    assert_eq!(closed.code, Some(503));
    assert_eq!(closed.result, crate::remote::SHUT_DOWN);
    assert!(store.command_state(theirs).await.unwrap().outcome.is_none());
    assert!(
        store
            .command_state(nobodys)
            .await
            .unwrap()
            .outcome
            .is_none()
    );
    assert_eq!(
        store
            .command_state(first)
            .await
            .unwrap()
            .outcome
            .unwrap()
            .code,
        Some(409),
        "a finished row keeps its outcome"
    );
    store
        .command_finish(theirs, true, None, "stopped", now)
        .await
        .unwrap();
    store
        .command_finish(nobodys, true, None, "stopped", now)
        .await
        .unwrap();

    // Finished rows older than the cut-off are deleted; unfinished ones stay.
    store
        .commands_lapse(
            now - 60_000,
            now - 660_000,
            now + 1,
            ("logs", now - 60_000),
            now,
        )
        .await
        .unwrap();
    for id in [first, second, untaken, taken] {
        assert!(store.command_state(id).await.is_err(), "row {id} is gone");
    }
    assert!(store.command_state(other).await.unwrap().outcome.is_none());
}

/// An agent's row as another process reads it.
async fn agent_rows(store: &DbStore) {
    let mut status = AgentStatus::new("reader", Restart::Always, Some("g".into()), 1_000);
    status.state = AgentState::Paused;
    status.restarts = 3;
    status.runs = 9;
    status.started_at_ms = Some(900);
    status.last_heartbeat_ms = Some(950);
    status.last_error = Some("boom \"quoted\" ✓".into());
    store.upsert_agent(&status).await.unwrap();
    let mut never = AgentStatus::new("fresh", Restart::Never, None, 2_000);
    never.state = AgentState::Stopped;
    store.upsert_agent(&never).await.unwrap();
    let rows = store.agent_rows().await.unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert!(names.windows(2).all(|w| w[0] <= w[1]), "by name: {names:?}");
    let row = rows.iter().find(|r| r.name == "reader").unwrap();
    assert_eq!(row.state, "paused");
    assert_eq!((row.restarts, row.runs), (3, 9));
    assert_eq!(row.started_at_ms, Some(900));
    assert_eq!(row.last_heartbeat_ms, Some(950));
    assert_eq!(row.last_error.as_deref(), Some("boom \"quoted\" ✓"));
    assert_eq!(row.updated_at_ms, 1_000);
    let row = rows.iter().find(|r| r.name == "fresh").unwrap();
    assert_eq!((row.started_at_ms, row.last_heartbeat_ms), (None, None));
    assert_eq!(row.last_error, None);
}

/// Leases and claims through the `database` cache store on `db`.
async fn cache_locks(db: Db) {
    let schema = Schema::new(&db);
    smeltery_core::cache::migrations::down(&schema)
        .await
        .unwrap();
    smeltery_core::cache::migrations::up(&schema).await.unwrap();
    let mut settings = smeltery_core::config::Settings::from_env();
    settings.cache_prefix = format!("wf_{}_{}_", std::process::id(), crate::time::system_ms());
    let cache = Cache::open("database", &settings, Some(db)).unwrap();
    crate::coord::tests::real_store(cache).await;
}

/// The whole suite on the database at `url`.
async fn suite(url: &str) {
    let db = Db::connect(url).await.unwrap();
    upgrade_from_the_old_schema(&db).await;

    fresh_tables(&db).await;
    let store = DbStore::open(db.clone()).await;
    assert!(store.processes());
    clock(&store).await;
    commands(&store).await;
    agent_rows(&store).await;
    crate::store::tests::round_trip(&store).await;
    crate::store::tests::per_process_runs(&store).await;

    // Two processes on this database: the dashboard of one carries commands out in the other.
    fresh_tables(&db).await;
    crate::coord_tests::web_process_controls_a_work_process(url).await;
    fresh_tables(&db).await;
    crate::coord_tests::slow_remote_stop_and_bounded_queue(url).await;
    fresh_tables(&db).await;
    crate::coord_tests::slow_stop_does_not_hold_back_others(url).await;

    cache_locks(db).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backend_suite_on_sqlite() {
    let (_dir, url) = crate::coord_tests::sqlite_file().await;
    suite(&url).await;
}

#[cfg(feature = "backend-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL_PG"]
async fn backend_suite_on_postgres() {
    let url = std::env::var("DATABASE_URL_PG").expect("DATABASE_URL_PG");
    assert_eq!(
        Db::connect(&url).await.unwrap().backend(),
        Backend::Postgres
    );
    suite(&url).await;
}

#[cfg(feature = "backend-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL_MYSQL"]
async fn backend_suite_on_mysql() {
    let url = std::env::var("DATABASE_URL_MYSQL").expect("DATABASE_URL_MYSQL");
    assert_eq!(Db::connect(&url).await.unwrap().backend(), Backend::MySql);
    suite(&url).await;
}
