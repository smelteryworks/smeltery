//! The framework tables Watchfire keeps in the app's database.
//!
//! A generated app's migration `database/migrations/m<stamp>_create_watchfire_tables.rs`
//! delegates here:
//!
//! ```
//! use smeltery_core::Result;
//! use smeltery_core::db::migration::{Migration, Schema};
//!
//! pub struct CreateWatchfireTables;
//!
//! impl Migration for CreateWatchfireTables {
//!     fn name(&self) -> &'static str {
//!         "2026_10_03_000000_create_watchfire_tables"
//!     }
//!
//!     async fn up(&self, schema: &Schema) -> Result<()> {
//!         smeltery_watchfire::migrations::up(schema).await
//!     }
//!
//!     async fn down(&self, schema: &Schema) -> Result<()> {
//!         smeltery_watchfire::migrations::down(schema).await
//!     }
//! }
//! ```
//!
//! | Table | Holds |
//! |---|---|
//! | `watchfire_agents` | one row per agent: state, restarts, runs, last heartbeat, last error |
//! | `watchfire_runs` | one row per run: agent, run id, the process that ran it, job, start, end, outcome, error, counters (JSON) |
//! | `watchfire_checkpoints` | one row per agent: the last checkpoint (JSON) |
//! | `watchfire_jobs` | the database queue: job name, payload (JSON), attempts, available / reserved times |
//! | `watchfire_dead_letters` | jobs that failed for good: name, payload, error, attempts |
//! | `watchfire_commands` | commands for agents that run in another process: agent, action, who took it, outcome |
//!
//! Times are Unix milliseconds in `BIGINT` columns.
//!
//! An app whose Watchfire migration ran before `watchfire_commands` and the `process` column existed adds them
//! with a second migration that calls [`up_multi_process`] / [`down_multi_process`] (it does nothing when they
//! exist already):
//!
//! ```
//! use smeltery_core::Result;
//! use smeltery_core::db::migration::{Migration, Schema};
//!
//! pub struct WatchfireMultiProcess;
//!
//! impl Migration for WatchfireMultiProcess {
//!     fn name(&self) -> &'static str {
//!         "2026_10_05_000000_watchfire_multi_process"
//!     }
//!
//!     async fn up(&self, schema: &Schema) -> Result<()> {
//!         smeltery_watchfire::migrations::up_multi_process(schema).await
//!     }
//!
//!     async fn down(&self, schema: &Schema) -> Result<()> {
//!         smeltery_watchfire::migrations::down_multi_process(schema).await
//!     }
//! }
//! ```

use smeltery_core::Result;
use smeltery_core::db::migration::Schema;

use crate::queue::db::{DEAD_LETTERS, JOBS};
use crate::store::{AGENTS, CHECKPOINTS, COMMANDS, RUNS};

/// Create the Watchfire tables.
///
/// # Errors
/// A table already exists or the database fails.
pub async fn up(schema: &Schema) -> Result<()> {
    schema
        .create(AGENTS, |t| {
            t.id();
            t.string("name").unique();
            t.string_len("state", 32);
            t.big_integer("restarts").default(0);
            t.big_integer("runs").default(0);
            t.big_integer("started_at").nullable();
            t.big_integer("last_heartbeat_at").nullable();
            t.text("last_error").nullable();
            t.big_integer("updated_at");
        })
        .await?;
    schema
        .create(RUNS, |t| {
            t.id();
            t.string("agent").index();
            t.big_integer("run_id");
            t.string("process").default("").index();
            t.string("job").nullable();
            t.big_integer("started_at").index();
            t.big_integer("ended_at").nullable();
            t.string_len("outcome", 32).index();
            t.text("error").nullable();
            t.text("counters").nullable();
        })
        .await?;
    schema
        .create(CHECKPOINTS, |t| {
            t.id();
            t.string("agent").unique();
            t.text("data");
            t.big_integer("updated_at");
        })
        .await?;
    schema
        .create(JOBS, |t| {
            t.id();
            t.string("job");
            t.text("payload");
            t.big_integer("attempts").default(0);
            t.big_integer("available_at").index();
            t.big_integer("reserved_at").nullable().index();
            t.big_integer("created_at");
        })
        .await?;
    schema
        .create(DEAD_LETTERS, |t| {
            t.id();
            t.string("job");
            t.text("payload");
            t.text("error");
            t.big_integer("attempts");
            t.big_integer("failed_at").index();
        })
        .await?;
    create_commands(schema).await
}

async fn create_commands(schema: &Schema) -> Result<()> {
    schema
        .create(COMMANDS, |t| {
            t.id();
            t.string_len("token", 64).unique();
            t.string("agent").index();
            t.string_len("action", 16);
            t.big_integer("requested_at").index();
            t.string("taken_by").nullable();
            t.big_integer("done_at").nullable().index();
            t.boolean("ok").nullable();
            t.integer("code").nullable();
            t.text("result").nullable();
        })
        .await
}

/// Add what several processes need to tables an older [`up`] created: the `process` column of `watchfire_runs`
/// and the `watchfire_commands` table. Does nothing when `watchfire_commands` exists (a database [`up`] created).
///
/// # Errors
/// The database fails.
pub async fn up_multi_process(schema: &Schema) -> Result<()> {
    if schema.has_table(COMMANDS).await? {
        return Ok(());
    }
    let added = schema
        .table(RUNS, |t| {
            t.string("process").default("");
        })
        .await;
    match added {
        Ok(()) => {}
        // MySQL commits DDL at once: a run that added the column and then failed to create the table is re-run
        // here, and finds the column (error 1060). SQLite and PostgreSQL roll the whole migration back.
        Err(e)
            if schema.backend() == smeltery_core::db::Backend::MySql
                && e.to_string().contains("Duplicate column") => {}
        Err(e) => return Err(e),
    }
    create_commands(schema).await
}

/// Undo [`up_multi_process`]: drop `watchfire_commands` and the `process` column.
///
/// # Errors
/// The database fails.
pub async fn down_multi_process(schema: &Schema) -> Result<()> {
    if !schema.has_table(COMMANDS).await? {
        return Ok(());
    }
    schema.drop_if_exists(COMMANDS).await?;
    schema
        .raw(&format!("ALTER TABLE {RUNS} DROP COLUMN process"))
        .await
}

/// Drop the Watchfire tables.
///
/// # Errors
/// The database fails.
pub async fn down(schema: &Schema) -> Result<()> {
    for table in [COMMANDS, DEAD_LETTERS, JOBS, CHECKPOINTS, RUNS, AGENTS] {
        schema.drop_if_exists(table).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use smeltery_core::db::Db;
    use smeltery_core::db::migration::Schema;

    use super::*;
    use crate::status::RunRecord;
    use crate::store::{DbStore, Store};

    /// The tables as `up` created them before several-process support.
    async fn old_up(schema: &Schema) {
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
    }

    #[tokio::test]
    async fn older_tables_are_upgraded_and_the_upgrade_is_a_no_op_on_new_ones() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let schema = Schema::new(&db);
        old_up(&schema).await;
        let old = DbStore::open(db.clone()).await;
        assert!(!old.processes());
        // The old schema still records runs.
        old.upsert_run(&RunRecord::started("a", 1, None, 1))
            .await
            .unwrap();
        up_multi_process(&schema).await.unwrap();
        let new = DbStore::open(db.clone()).await;
        assert!(new.processes());
        let mut run = RunRecord::started("a", 1, None, 2);
        run.process = "p".into();
        new.upsert_run(&run).await.unwrap();
        assert_eq!(new.recent_runs(Some("a"), 5).await.unwrap().len(), 2);
        down_multi_process(&schema).await.unwrap();
        assert!(!schema.has_table(COMMANDS).await.unwrap());
        assert!(!DbStore::open(db.clone()).await.processes());

        let fresh = Db::connect("sqlite::memory:").await.unwrap();
        let schema = Schema::new(&fresh);
        up(&schema).await.unwrap();
        up_multi_process(&schema).await.unwrap();
        assert!(DbStore::open(fresh).await.processes());
    }
}
