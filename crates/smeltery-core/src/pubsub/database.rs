//! The `database` driver: one row per message in `pubsub_messages`, read by every process that subscribes.
//!
//! Commit order is not id order on PostgreSQL and MySQL (a row with a lower id can commit after a higher one), so a
//! poll does not read "ids after the last one": it reads the rows created since the newest `created_at` it has seen
//! minus [`OVERLAP_MS`], and skips ids it has already delivered (a bounded set). `created_at` comes from the
//! database's clock, so the poll window does not depend on the processes' clocks (the sealed send time does: see
//! `MAX_MESSAGE_AGE`). Inserts are single statements (autocommit), so the gap
//! between a row's `created_at` and its commit is the statement's own time.
//!
//! The cursor never runs ahead of the database's clock: a row dated in the future (written by hand, or before the
//! database's clock stepped back) would otherwise move it past every row inserted later, and delivery would stop
//! without a sign. Each poll reads the clock; a cursor ahead of it is put back to it (logged), and rows dated more
//! than `MAX_MESSAGE_AGE` ahead are pruned. A payload longer than a sealed message is not read: the poll returns it
//! empty, and it is counted as unreadable.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{
    DRIVER_TIMEOUT, MAX_MESSAGE_AGE, MAX_SEALED_BYTES, Outage, RareLog, Shared, TABLE, Transport,
    backoff,
};
use crate::app::BoxFuture;
use crate::db::{Backend, Db};
use crate::error::{Error, Result};

/// How far before the newest `created_at` seen a poll reads again (late commits).
pub(super) const OVERLAP_MS: i64 = 2_000;
/// Rows read per query.
const PAGE: u64 = 200;
/// Queries per poll at most; what is left is read at the next poll.
const MAX_PAGES: usize = 10;
/// The most delivered ids remembered.
const MAX_SEEN: usize = 50_000;
/// Rows older than this are deleted.
pub(super) const RETENTION_MS: i64 = 60_000;
/// Deletes run at most this often per process.
const PRUNE_EVERY: Duration = Duration::from_secs(10);
/// Rows one delete statement removes at most.
const PRUNE_BATCH: u64 = 500;
/// Delete statements per prune at most (500,000 rows; the time budget usually ends a prune first).
const PRUNE_BATCHES: usize = 1_000;

/// The database's clock in Unix milliseconds, as an SQL expression (the functions Watchfire's `db_now` uses).
fn now_sql(backend: Backend) -> &'static str {
    match backend {
        Backend::Postgres => "CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)",
        // UTC_TIMESTAMP is UTC whatever the session time zone.
        Backend::MySql => {
            "CAST(TIMESTAMPDIFF(MICROSECOND, '1970-01-01 00:00:00', UTC_TIMESTAMP(6)) DIV 1000 AS SIGNED)"
        }
        _ => "CAST(ROUND((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)",
    }
}

/// Placeholder `n` (1-based) of `backend`.
fn ph(backend: Backend, n: usize) -> String {
    if backend == Backend::Postgres {
        format!("${n}")
    } else {
        "?".to_owned()
    }
}

pub(super) fn insert_sql(backend: Backend) -> String {
    format!(
        "INSERT INTO {TABLE} (payload, created_at) VALUES ({}, {})",
        ph(backend, 1),
        now_sql(backend)
    )
}

/// The rows after a position. A payload longer than any sealed message comes back empty (it is not transferred, and
/// it is counted as unreadable).
pub(super) fn poll_sql(backend: Backend) -> String {
    format!(
        "SELECT id, CASE WHEN LENGTH(payload) <= {MAX_SEALED_BYTES} THEN payload ELSE '' END AS payload, created_at \
         FROM {TABLE} WHERE created_at >= {} AND id > {} ORDER BY id LIMIT {PAGE}",
        ph(backend, 1),
        ph(backend, 2)
    )
}

/// One bounded delete of rows created before the first bound value or after the second (dated in the future). MySQL
/// has `DELETE … LIMIT` (and refuses `LIMIT` in an `IN` sub-select of the same table); PostgreSQL and SQLite delete
/// the ids of a limited sub-select.
pub(super) fn prune_sql(backend: Backend) -> String {
    if backend == Backend::MySql {
        format!("DELETE FROM {TABLE} WHERE created_at < ? OR created_at > ? LIMIT {PRUNE_BATCH}")
    } else {
        format!(
            "DELETE FROM {TABLE} WHERE id IN (SELECT id FROM {TABLE} WHERE created_at < {} OR created_at > {} \
             LIMIT {PRUNE_BATCH})",
            ph(backend, 1),
            ph(backend, 2)
        )
    }
}

/// How far ahead of the database's clock a row may be dated before it is pruned (a sender's message is refused that
/// far ahead anyway).
fn ahead_ms() -> i64 {
    i64::try_from(MAX_MESSAGE_AGE.as_millis()).unwrap_or(i64::MAX)
}

/// The cursor was ahead of the database's clock (logged at most once a minute).
static CLOCK_LOG: RareLog = RareLog::new();

async fn db_now(db: &Db) -> Result<i64> {
    let rows = db
        .query_with(&format!("SELECT {} AS n", now_sql(db.backend())), [])
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| Error::internal("the database clock query returned no row"))?;
    Ok(row.try_get::<i64>("", "n")?)
}

/// Sends by inserting a row.
pub(super) struct DatabaseTransport {
    db: Db,
}

impl DatabaseTransport {
    pub(super) fn new(db: Db) -> Self {
        Self { db }
    }
}

impl Transport for DatabaseTransport {
    fn send<'a>(&'a self, sealed: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.db
                .execute_with(&insert_sql(self.db.backend()), [sealed.into()])
                .await?;
            Ok(())
        })
    }
}

/// Delete rows older than [`RETENTION_MS`] or dated more than `MAX_MESSAGE_AGE` ahead, [`PRUNE_BATCH`] per
/// statement, until a statement deletes fewer (at most [`PRUNE_BATCHES`] statements); the rows deleted. The caller
/// bounds it in time ([`DRIVER_TIMEOUT`]).
pub(super) async fn prune(db: &Db) -> Result<u64> {
    let now = db_now(db).await?;
    let cutoff = now - RETENTION_MS;
    let ahead = now.saturating_add(ahead_ms());
    let sql = prune_sql(db.backend());
    let mut removed = 0;
    for _ in 0..PRUNE_BATCHES {
        let n = db.execute_with(&sql, [cutoff.into(), ahead.into()]).await?;
        removed += n;
        if n < PRUNE_BATCH {
            break;
        }
    }
    Ok(removed)
}

/// Every [`PRUNE_EVERY`] until shutdown, delete old rows within [`DRIVER_TIMEOUT`]: every process of the `database`
/// driver does, whether it publishes or not, so rows never outlive [`RETENTION_MS`] by more than a round while a
/// process runs. Failures are logged once per outage.
pub(super) async fn prune_loop(db: Db, token: CancellationToken) {
    let mut outage = Outage::new("pubsub: deleting old messages");
    loop {
        tokio::select! {
            biased;
            () = token.cancelled() => return,
            () = tokio::time::sleep(PRUNE_EVERY) => {}
        }
        let pruned = tokio::select! {
            biased;
            () = token.cancelled() => return,
            pruned = tokio::time::timeout(DRIVER_TIMEOUT, prune(&db)) => pruned,
        };
        match pruned {
            Ok(Ok(_)) => outage.ok(),
            Ok(Err(e)) => outage.fail(&e),
            // Partial progress is kept; the rest goes at the next round.
            Err(_) => outage.fail(&format!("not finished within {DRIVER_TIMEOUT:?}")),
        }
    }
}

/// The ids a poller has delivered, oldest first, bounded.
#[derive(Default)]
pub(super) struct Seen {
    order: VecDeque<(i64, i64)>,
    ids: HashSet<i64>,
}

impl Seen {
    /// Remember `id` (created at `created_at`); `false` when it was delivered before.
    pub(super) fn insert(&mut self, id: i64, created_at: i64) -> bool {
        if !self.ids.insert(id) {
            return false;
        }
        self.order.push_back((id, created_at));
        while self.order.len() > MAX_SEEN {
            if let Some((old, _)) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        true
    }

    /// Forget ids created before `floor` (a poll never reads them again).
    fn forget_before(&mut self, floor: i64) {
        while let Some(&(id, created_at)) = self.order.front() {
            if created_at >= floor {
                break;
            }
            self.order.pop_front();
            self.ids.remove(&id);
        }
    }

    fn clear(&mut self) {
        self.order.clear();
        self.ids.clear();
    }

    #[cfg(all(test, feature = "sqlite"))]
    pub(super) fn len(&self) -> usize {
        self.ids.len()
    }
}

/// Where a poller is: the newest `created_at` it has seen (`None` after an idle spell: start from "now"), and where
/// an unfinished scan goes on.
#[derive(Default)]
pub(super) struct Cursor {
    pub(super) since: Option<i64>,
    pub(super) seen: Seen,
    /// A scan that stopped at [`MAX_PAGES`]: its floor and the last id it read. The next poll goes on from there
    /// instead of reading the same rows again (more than `MAX_PAGES` pages of rows in one overlap window would
    /// otherwise stall the poller on them).
    /// While a scan resumes, a row committed late with an id below its position is found only by the next full scan
    /// (from 2 s before the newest `created_at`): one burst spanning more than 2 s can lose such a row, within the
    /// at-most-once contract.
    pub(super) resume: Option<(i64, i64)>,
}

/// One poll: deliver the rows created since the cursor (minus the overlap) that were not delivered yet.
pub(super) async fn poll_once(db: &Db, shared: &Shared, cursor: &mut Cursor) -> Result<usize> {
    let now = db_now(db).await?;
    let Some(mut since) = cursor.since else {
        // The first poll after starting or an idle spell takes the database's now as its starting point; the next
        // one reads from [`OVERLAP_MS`] before it, so only messages of the last two seconds can arrive from earlier.
        cursor.since = Some(now);
        cursor.resume = None;
        return Ok(0);
    };
    if since > now + OVERLAP_MS {
        // The database's clock stepped back (or a row dated in the future moved the cursor): rows inserted from now
        // on would lie below the cursor and never be read. Read from the clock again.
        if CLOCK_LOG.due() {
            tracing::warn!(
                ahead_ms = since - now,
                "pubsub: the poll position was ahead of the database's clock (its clock stepped back, or a row is \
                 dated in the future); reading from the database's clock again"
            );
        }
        since = now;
        cursor.resume = None;
    }
    // A full scan of the overlap window, or the rest of a scan that hit the page limit.
    let (floor, mut after) = cursor.resume.take().unwrap_or((since - OVERLAP_MS, 0));
    let sql = poll_sql(db.backend());
    let mut newest = since;
    let mut delivered = 0;
    let mut finished = false;
    for _ in 0..MAX_PAGES {
        let rows = db.query_with(&sql, [floor.into(), after.into()]).await?;
        let count = rows.len();
        for row in rows {
            let id: i64 = row.try_get("", "id")?;
            let created_at: i64 = row.try_get("", "created_at")?;
            after = after.max(id);
            newest = newest.max(created_at);
            if cursor.seen.insert(id, created_at) {
                let payload: String = row.try_get("", "payload")?;
                shared.receive(&payload);
                delivered += 1;
            }
        }
        if u64::try_from(count).unwrap_or(u64::MAX) < PAGE {
            finished = true;
            break;
        }
    }
    if !finished {
        cursor.resume = Some((floor, after));
    }
    // Never ahead of the database's clock: a row dated in the future must not move the cursor past later rows.
    let newest = newest.min(now);
    cursor.since = Some(newest);
    cursor.seen.forget_before(newest - OVERLAP_MS - 1_000);
    Ok(delivered)
}

/// Poll every `interval` while something in this process subscribes, until shutdown. Errors back off (1 s to 30 s)
/// and are logged once per outage.
pub(super) async fn poll_loop(
    shared: Arc<Shared>,
    db: Db,
    interval: Duration,
    token: CancellationToken,
) {
    let mut cursor = Cursor::default();
    let mut failures = 0_u32;
    let mut outage = Outage::new("pubsub: reading the messages of the other processes");
    loop {
        let wait = if failures == 0 {
            interval
        } else {
            backoff(failures - 1)
        };
        tokio::select! {
            biased;
            () = token.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        if shared.subscribers() == 0 {
            cursor.since = None;
            cursor.seen.clear();
            cursor.resume = None;
            failures = 0;
            continue;
        }
        let polled = tokio::select! {
            biased;
            () = token.cancelled() => return,
            polled = tokio::time::timeout(DRIVER_TIMEOUT, poll_once(&db, &shared, &mut cursor)) => polled,
        };
        match polled {
            Ok(Ok(_)) => {
                outage.ok();
                failures = 0;
            }
            Ok(Err(e)) => {
                outage.fail(&e);
                failures = failures.saturating_add(1);
            }
            Err(_) => {
                outage.fail(&format!("no answer within {DRIVER_TIMEOUT:?}"));
                failures = failures.saturating_add(1);
            }
        }
    }
}
