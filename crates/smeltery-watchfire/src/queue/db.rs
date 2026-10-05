//! The database queue driver over `watchfire_jobs` and `watchfire_dead_letters`.
//!
//! Reservation is one atomic statement per candidate:
//! `UPDATE watchfire_jobs SET reserved_at = now, attempts = attempts + 1 WHERE id = ? AND
//! reserved_at IS NULL`; only the worker whose update changed the row owns the job, so
//! concurrent workers (in one process or several) never hold a job at the same time. A
//! reservation older than twice the job timeout is released as stale and the job can be reserved
//! again (it may then run a second time); the earlier holder's delete / retry / release / dead
//! letter carries its reservation (`reserved_at` + `attempts`) and changes nothing once another
//! worker holds the job. Moving a job to the dead letters and back are transactions: the job is in
//! exactly one of the two tables.

use smeltery_core::BoxFuture;
use smeltery_core::db::prelude::sea_orm::sea_query::{
    Alias, ConditionalStatement, Expr, ExprTrait, Func, Order, Query,
};
use smeltery_core::db::prelude::sea_orm::{
    ConnectionTrait, DatabaseTransaction, QueryResult, StatementBuilder,
};
use smeltery_core::db::{Backend, Db};

use super::{DeadLetter, Driver, JobId, QResult, QueueStats, Reserved};
use crate::error::StoreError;

pub(crate) const JOBS: &str = "watchfire_jobs";
pub(crate) const DEAD_LETTERS: &str = "watchfire_dead_letters";

/// Candidates read per reservation attempt; another worker may win some of them.
const CANDIDATES: u64 = 8;

fn col(name: &str) -> Alias {
    Alias::new(name)
}

#[derive(Clone, Debug)]
pub(crate) struct DbDriver {
    db: Db,
}

impl DbDriver {
    pub(crate) fn new(db: Db) -> Self {
        Self { db }
    }

    async fn exec<S: StatementBuilder>(&self, op: &'static str, stmt: &S) -> QResult<u64> {
        let conn = self.db.conn();
        conn.execute_raw(conn.get_database_backend().build(stmt))
            .await
            .map(|r| r.rows_affected())
            .map_err(|e| StoreError::new(op, e))
    }

    async fn begin(&self, op: &'static str) -> QResult<DatabaseTransaction> {
        // A write transaction (SQLite `BEGIN IMMEDIATE`): `requeue_dead` reads before it writes.
        self.db
            .begin_write()
            .await
            .map_err(|e| StoreError::new(op, e))
    }

    async fn query<S: StatementBuilder>(
        &self,
        op: &'static str,
        stmt: &S,
    ) -> QResult<Vec<QueryResult>> {
        let conn = self.db.conn();
        conn.query_all_raw(conn.get_database_backend().build(stmt))
            .await
            .map_err(|e| StoreError::new(op, e))
    }
}

fn get<T: smeltery_core::db::prelude::sea_orm::TryGetable>(
    row: &QueryResult,
    op: &'static str,
    name: &str,
) -> QResult<T> {
    row.try_get("", name).map_err(|e| StoreError::new(op, e))
}

fn u32_of(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(0)
}

/// `WHERE id = ? AND reserved_at = ? AND attempts = ?`: `job`'s reservation is still the one held.
fn held<S: ConditionalStatement>(stmt: &mut S, job: &Reserved) {
    stmt.and_where(Expr::col(col("id")).eq(job.id))
        .and_where(Expr::col(col("reserved_at")).eq(job.reserved_at))
        .and_where(Expr::col(col("attempts")).eq(i64::from(job.attempts)));
}

/// The payload's length in bytes, per backend.
fn payload_bytes(backend: Backend) -> &'static str {
    match backend {
        Backend::MySql => "LENGTH(`payload`)",
        Backend::Postgres => "OCTET_LENGTH(\"payload\")",
        _ => "LENGTH(CAST(\"payload\" AS BLOB))",
    }
}

async fn exec_on<C: ConnectionTrait, S: StatementBuilder>(
    conn: &C,
    op: &'static str,
    stmt: &S,
) -> QResult<u64> {
    conn.execute_raw(conn.get_database_backend().build(stmt))
        .await
        .map(|r| r.rows_affected())
        .map_err(|e| StoreError::new(op, e))
}

/// Insert a job row on `conn` (the pool or a transaction) and return its id.
async fn insert_job<C: ConnectionTrait>(
    conn: &C,
    backend: Backend,
    op: &'static str,
    job: &str,
    payload: &str,
    available_at: i64,
    now: i64,
) -> QResult<JobId> {
    let mut insert = Query::insert();
    insert
        .into_table(col(JOBS))
        .columns([
            col("job"),
            col("payload"),
            col("attempts"),
            col("available_at"),
            col("created_at"),
        ])
        .values([
            Expr::val(job),
            Expr::val(payload),
            Expr::val(0_i64),
            Expr::val(available_at),
            Expr::val(now),
        ])
        .map_err(|e| StoreError::new(op, e.to_string()))?;
    if backend == Backend::MySql {
        // MySQL has no RETURNING; the driver reports the id.
        let result = conn
            .execute_raw(conn.get_database_backend().build(&insert))
            .await
            .map_err(|e| StoreError::new(op, e))?;
        return Ok(i64::try_from(result.last_insert_id()).unwrap_or(i64::MAX));
    }
    insert.returning_col(col("id"));
    let rows = conn
        .query_all_raw(conn.get_database_backend().build(&insert))
        .await
        .map_err(|e| StoreError::new(op, e))?;
    match rows.first() {
        Some(row) => get(row, op, "id"),
        None => Err(StoreError::new(op, "the insert returned no id")),
    }
}

fn dead_letter_of(row: &QueryResult, op: &'static str) -> QResult<DeadLetter> {
    Ok(DeadLetter {
        id: get(row, op, "id")?,
        job: get(row, op, "job")?,
        payload: get(row, op, "payload")?,
        error: get(row, op, "error")?,
        attempts: u32_of(get(row, op, "attempts")?),
        failed_at_ms: get(row, op, "failed_at")?,
    })
}

impl Driver for DbDriver {
    fn push<'a>(
        &'a self,
        job: &'a str,
        payload: &'a str,
        available_at: i64,
        now: i64,
    ) -> BoxFuture<'a, QResult<JobId>> {
        Box::pin(async move {
            insert_job(
                self.db.conn(),
                self.db.backend(),
                "push",
                job,
                payload,
                available_at,
                now,
            )
            .await
        })
    }

    fn reserve(&self, now: i64, max_payload: u64) -> BoxFuture<'_, QResult<Option<Reserved>>> {
        Box::pin(async move {
            let select = Query::select()
                .column(col("id"))
                .from(col(JOBS))
                .and_where(Expr::col(col("reserved_at")).is_null())
                .and_where(Expr::col(col("available_at")).lte(now))
                .order_by(col("available_at"), Order::Asc)
                .order_by(col("id"), Order::Asc)
                .limit(CANDIDATES)
                .to_owned();
            for row in self.query("reserve", &select).await? {
                let id: i64 = get(&row, "reserve", "id")?;
                let update = Query::update()
                    .table(col(JOBS))
                    .values([
                        (col("reserved_at"), Expr::val(now)),
                        (col("attempts"), Expr::col(col("attempts")).add(1)),
                    ])
                    .and_where(Expr::col(col("id")).eq(id))
                    .and_where(Expr::col(col("reserved_at")).is_null())
                    .to_owned();
                if self.exec("reserve", &update).await? != 1 {
                    continue; // another worker won this one
                }
                // The size first: a payload over the limit is never read.
                let read = Query::select()
                    .columns([col("job"), col("attempts")])
                    .expr_as(
                        Expr::cust(payload_bytes(self.db.backend())),
                        col("payload_bytes"),
                    )
                    .from(col(JOBS))
                    .and_where(Expr::col(col("id")).eq(id))
                    .to_owned();
                let rows = self.query("reserve", &read).await?;
                let Some(row) = rows.first() else { continue };
                let size = u64::try_from(get::<i64>(row, "reserve", "payload_bytes")?).unwrap_or(0);
                let oversized = (size > max_payload).then_some(size);
                let payload = if oversized.is_some() {
                    String::new()
                } else {
                    let read = Query::select()
                        .column(col("payload"))
                        .from(col(JOBS))
                        .and_where(Expr::col(col("id")).eq(id))
                        .to_owned();
                    let rows = self.query("reserve", &read).await?;
                    let Some(payload_row) = rows.first() else {
                        continue;
                    };
                    get(payload_row, "reserve", "payload")?
                };
                return Ok(Some(Reserved {
                    id,
                    job: get(row, "reserve", "job")?,
                    payload,
                    attempts: u32_of(get(row, "reserve", "attempts")?),
                    reserved_at: now,
                    oversized,
                }));
            }
            Ok(None)
        })
    }

    fn delete<'a>(&'a self, job: &'a Reserved) -> BoxFuture<'a, QResult<bool>> {
        Box::pin(async move {
            let mut delete = Query::delete().from_table(col(JOBS)).to_owned();
            held(&mut delete, job);
            Ok(self.exec("delete", &delete).await? == 1)
        })
    }

    fn retry<'a>(&'a self, job: &'a Reserved, available_at: i64) -> BoxFuture<'a, QResult<bool>> {
        Box::pin(async move {
            let mut update = Query::update()
                .table(col(JOBS))
                .values([
                    (col("reserved_at"), Expr::val(None::<i64>)),
                    (col("available_at"), Expr::val(available_at)),
                ])
                .to_owned();
            held(&mut update, job);
            Ok(self.exec("retry", &update).await? == 1)
        })
    }

    fn release<'a>(&'a self, job: &'a Reserved) -> BoxFuture<'a, QResult<bool>> {
        Box::pin(async move {
            let mut update = Query::update()
                .table(col(JOBS))
                .values([
                    (col("reserved_at"), Expr::val(None::<i64>)),
                    (col("attempts"), Expr::col(col("attempts")).sub(1)),
                ])
                .to_owned();
            held(&mut update, job);
            Ok(self.exec("release", &update).await? == 1)
        })
    }

    fn dead_letter<'a>(
        &'a self,
        job: &'a Reserved,
        error: &'a str,
        now: i64,
    ) -> BoxFuture<'a, QResult<bool>> {
        Box::pin(async move {
            // The dead letter is copied from the job's row (its stored payload, also one too large to read), and
            // only while `job`'s reservation is the one held.
            let mut source = Query::select()
                .columns([col("job"), col("payload")])
                .expr(Expr::val(super::stored_error(error)))
                .column(col("attempts"))
                .expr(Expr::val(now))
                .from(col(JOBS))
                .to_owned();
            held(&mut source, job);
            let mut insert = Query::insert();
            insert
                .into_table(col(DEAD_LETTERS))
                .columns([
                    col("job"),
                    col("payload"),
                    col("error"),
                    col("attempts"),
                    col("failed_at"),
                ])
                .select_from(source)
                .map_err(|e| StoreError::new("dead_letter", e.to_string()))?;
            let mut delete = Query::delete().from_table(col(JOBS)).to_owned();
            held(&mut delete, job);
            // One transaction: a failed insert leaves the job queued, a failed delete leaves no dead letter.
            let txn = self.begin("dead_letter").await?;
            if exec_on(&txn, "dead_letter", &insert).await? != 1
                || exec_on(&txn, "dead_letter", &delete).await? != 1
            {
                txn.rollback()
                    .await
                    .map_err(|e| StoreError::new("dead_letter", e))?;
                return Ok(false);
            }
            txn.commit()
                .await
                .map_err(|e| StoreError::new("dead_letter", e))?;
            Ok(true)
        })
    }

    fn release_stale(&self, before: i64) -> BoxFuture<'_, QResult<u64>> {
        Box::pin(async move {
            let update = Query::update()
                .table(col(JOBS))
                .values([(col("reserved_at"), Expr::val(None::<i64>))])
                .and_where(Expr::col(col("reserved_at")).lt(before))
                .to_owned();
            self.exec("release_stale", &update).await
        })
    }

    fn stats(&self) -> BoxFuture<'_, QResult<QueueStats>> {
        Box::pin(async move {
            let count = |table: &str, reserved: Option<bool>| {
                let mut select = Query::select();
                select
                    .expr_as(Func::count(Expr::col(col("id"))), col("n"))
                    .from(col(table));
                match reserved {
                    Some(true) => {
                        select.and_where(Expr::col(col("reserved_at")).is_not_null());
                    }
                    Some(false) => {
                        select.and_where(Expr::col(col("reserved_at")).is_null());
                    }
                    None => {}
                }
                select
            };
            let mut numbers = [0_u64; 3];
            for (slot, select) in numbers.iter_mut().zip([
                count(JOBS, Some(false)),
                count(JOBS, Some(true)),
                count(DEAD_LETTERS, None),
            ]) {
                let rows = self.query("stats", &select).await?;
                let n: i64 = match rows.first() {
                    Some(row) => get(row, "stats", "n")?,
                    None => 0,
                };
                *slot = u64::try_from(n).unwrap_or(0);
            }
            let [pending, reserved, dead] = numbers;
            Ok(QueueStats {
                pending,
                reserved,
                dead,
            })
        })
    }

    fn dead_letters(&self, limit: u32) -> BoxFuture<'_, QResult<Vec<DeadLetter>>> {
        Box::pin(async move {
            let select = Query::select()
                .columns([
                    col("id"),
                    col("job"),
                    col("payload"),
                    col("error"),
                    col("attempts"),
                    col("failed_at"),
                ])
                .from(col(DEAD_LETTERS))
                .order_by(col("id"), Order::Desc)
                .limit(u64::from(limit))
                .to_owned();
            let rows = self.query("dead_letters", &select).await?;
            rows.iter()
                .map(|row| dead_letter_of(row, "dead_letters"))
                .collect()
        })
    }

    fn take_dead(&self, id: i64) -> BoxFuture<'_, QResult<Option<DeadLetter>>> {
        Box::pin(async move {
            let select = Query::select()
                .columns([
                    col("id"),
                    col("job"),
                    col("payload"),
                    col("error"),
                    col("attempts"),
                    col("failed_at"),
                ])
                .from(col(DEAD_LETTERS))
                .and_where(Expr::col(col("id")).eq(id))
                .to_owned();
            let rows = self.query("take_dead", &select).await?;
            let Some(row) = rows.first() else {
                return Ok(None);
            };
            let dead = dead_letter_of(row, "take_dead")?;
            let delete = Query::delete()
                .from_table(col(DEAD_LETTERS))
                .and_where(Expr::col(col("id")).eq(id))
                .to_owned();
            // Only the caller whose delete removed the row owns it.
            if self.exec("take_dead", &delete).await? == 1 {
                Ok(Some(dead))
            } else {
                Ok(None)
            }
        })
    }

    fn requeue_dead(&self, id: i64, now: i64) -> BoxFuture<'_, QResult<Option<JobId>>> {
        Box::pin(async move {
            let select = Query::select()
                .columns([col("job"), col("payload")])
                .from(col(DEAD_LETTERS))
                .and_where(Expr::col(col("id")).eq(id))
                .to_owned();
            let delete = Query::delete()
                .from_table(col(DEAD_LETTERS))
                .and_where(Expr::col(col("id")).eq(id))
                .to_owned();
            // One transaction: the dead letter goes only when its job is queued again.
            let txn = self.begin("requeue_dead").await?;
            let rows = txn
                .query_all_raw(txn.get_database_backend().build(&select))
                .await
                .map_err(|e| StoreError::new("requeue_dead", e))?;
            let Some(row) = rows.first() else {
                return Ok(None);
            };
            let job: String = get(row, "requeue_dead", "job")?;
            let payload: String = get(row, "requeue_dead", "payload")?;
            // Only the caller whose delete removed the row owns it.
            if exec_on(&txn, "requeue_dead", &delete).await? != 1 {
                return Ok(None);
            }
            let job_id = insert_job(
                &txn,
                self.db.backend(),
                "requeue_dead",
                &job,
                &payload,
                now,
                now,
            )
            .await?;
            txn.commit()
                .await
                .map_err(|e| StoreError::new("requeue_dead", e))?;
            Ok(Some(job_id))
        })
    }
}
