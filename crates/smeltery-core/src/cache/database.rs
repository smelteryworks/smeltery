//! The `database` store: the `cache` table (`key`, `value`, `expiration` in Unix ms or
//! `NULL` for never) and the `cache_locks` table (`key`, `owner`, `expiration`), through the
//! app's [`Db`].
//!
//! Atomic operations use statements every backend runs atomically: `add` and lock acquiring
//! insert (a unique-key conflict means "taken") and otherwise update only an expired row;
//! `increment` is a compare-and-set loop (`UPDATE … WHERE value = <read value>`).

use std::time::Duration;

use sea_orm::sea_query::{Alias, Condition, Expr, ExprTrait, Func, OnConflict, Query, SimpleExpr};
use sea_orm::{ConnectionTrait, SqlErr, Statement, Value};

use super::{CacheStore, add_to, expires_at, now_ms};
use crate::app::BoxFuture;
use crate::db::Db;
use crate::error::{Error, Result};

/// How often `increment` retries when another writer changed the counter between its read and
/// its write.
const CAS_ATTEMPTS: usize = 1000;

/// The most a prune may add to a write (it is cut off after this, or after the store timeout if shorter).
const PRUNE_BUDGET: Duration = Duration::from_millis(500);

/// Rows one pruning statement deletes at most: a statement the budget cuts off keeps running on a PostgreSQL or
/// MySQL server, and holds the row (and, on InnoDB, gap) locks of at most this many rows meanwhile.
const PRUNE_BATCH: u64 = 500;

/// Pruning statements per table per prune; what is left goes at a later prune.
const PRUNE_BATCHES: usize = 20;

pub(crate) struct DatabaseStore {
    db: Db,
    table: String,
    locks: String,
    timeout: Duration,
    /// `CACHE_MAX_VALUE_BYTES`: a larger value is never fetched.
    max_value: usize,
    /// Tests: prune on every write (this store only).
    #[cfg(test)]
    clean_always: std::sync::atomic::AtomicBool,
    /// Tests: a slow prune (milliseconds it sleeps first).
    #[cfg(test)]
    prune_delay_ms: std::sync::atomic::AtomicU64,
}

/// Keys (and lock names) up to this many characters are stored as they are; the `key` columns are
/// `varchar(255)`.
const MAX_PLAIN_KEY: usize = 191;
/// How many leading characters of a longer key are kept in front of its hash, so `flush` by prefix still finds it.
const KEPT_HEAD: usize = 120;

/// The `key` column's value for `key`: the key itself up to [`MAX_PLAIN_KEY`] characters, else its first
/// [`KEPT_HEAD`] characters, `~sha256:` and the SHA-256 of the whole key (192 characters). A key built from user
/// input can be of any length; stored as it is, a long one failed the statement on PostgreSQL and strict MySQL,
/// and non-strict MySQL cut it, so different keys met. Plain keys are never 192 characters long, so the two
/// forms cannot meet.
fn stored_key(key: &str) -> std::borrow::Cow<'_, str> {
    if key.chars().count() <= MAX_PLAIN_KEY {
        return std::borrow::Cow::Borrowed(key);
    }
    let head: String = key.chars().take(KEPT_HEAD).collect();
    std::borrow::Cow::Owned(format!("{head}~sha256:{}", crate::crypto::sha256_hex(key)))
}

fn ms(value: Option<u64>) -> Value {
    Value::from(value.map(|v| i64::try_from(v).unwrap_or(i64::MAX)))
}

fn col(name: &str) -> Alias {
    Alias::new(name)
}

/// `expiration IS NULL OR expiration > now`.
fn live_cond() -> Condition {
    Condition::any()
        .add(Expr::col(col("expiration")).is_null())
        .add(Expr::col(col("expiration")).gt(ms(Some(now_ms()))))
}

/// `expiration IS NOT NULL AND expiration <= now`.
fn expired_cond() -> Condition {
    Condition::all()
        .add(Expr::col(col("expiration")).is_not_null())
        .add(Expr::col(col("expiration")).lte(ms(Some(now_ms()))))
}

impl DatabaseStore {
    /// The same store with this `CACHE_MAX_VALUE_BYTES`.
    pub(crate) fn max_value(mut self, bytes: usize) -> Self {
        self.max_value = bytes;
        self
    }

    pub(crate) fn new(db: Db, table: &str, timeout: Duration) -> Self {
        Self {
            db,
            table: table.to_owned(),
            locks: format!("{table}_locks"),
            timeout,
            max_value: super::DEFAULT_MAX_VALUE_BYTES,
            #[cfg(test)]
            clean_always: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            prune_delay_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn should_clean(&self) -> bool {
        #[cfg(test)]
        if self.clean_always.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        super::sometimes()
    }

    async fn timed<T>(&self, work: impl std::future::Future<Output = Result<T>>) -> Result<T> {
        tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| {
                Error::internal(format!(
                    "the database cache store did not answer within {:?}",
                    self.timeout
                ))
            })?
    }

    fn build<S: sea_orm::StatementBuilder>(&self, stmt: &S) -> Statement {
        self.db.conn().get_database_backend().build(stmt)
    }

    async fn exec(&self, stmt: Statement) -> Result<u64> {
        Ok(self.db.conn().execute_raw(stmt).await?.rows_affected())
    }

    /// Insert a row; `false` when the key exists already.
    async fn insert(&self, table: &str, values: [(&str, Value); 3]) -> Result<bool> {
        let mut insert = Query::insert();
        insert
            .into_table(col(table))
            .columns(values.iter().map(|(c, _)| col(c)))
            .values(values.into_iter().map(|(_, v)| SimpleExpr::from(v)))
            .map_err(|e| Error::internal(e.to_string()))?;
        match self.db.conn().execute_raw(self.build(&insert)).await {
            Ok(_) => Ok(true),
            Err(e) if matches!(e.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) => {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Update `table`'s row `key` with `values` when it has expired; `true` when updated.
    async fn take_expired(
        &self,
        table: &str,
        key: &str,
        values: [(&str, Value); 2],
    ) -> Result<bool> {
        let update = Query::update()
            .table(col(table))
            .values(
                values
                    .into_iter()
                    .map(|(c, v)| (col(c), SimpleExpr::from(v))),
            )
            .and_where(Expr::col(col("key")).eq(key))
            .cond_where(expired_cond())
            .to_owned();
        Ok(self.exec(self.build(&update)).await? > 0)
    }

    /// The live row's `(value, expiration)`.
    /// The value is fetched only when it holds at most `max_value` bytes (the size is measured in the database);
    /// a larger one is a [`super::TooLarge`] error without its text crossing the connection.
    async fn read(&self, key: &str) -> Result<Option<(String, Option<i64>)>> {
        let select = read_query(
            self.db.conn().get_database_backend(),
            &self.table,
            key,
            self.max_value,
        );
        let Some(row) = self.db.conn().query_one_raw(self.build(&select)).await? else {
            return Ok(None);
        };
        let Some(value) = row.try_get::<Option<String>>("", "value")? else {
            let bytes: i64 = row.try_get("", "bytes")?;
            return Err(super::too_large(
                u64::try_from(bytes).unwrap_or(0),
                self.max_value,
            ));
        };
        Ok(Some((value, row.try_get("", "expiration")?)))
    }

    /// Remove expired rows of both tables, on about one write in a hundred (`put` and `add`). Best effort, before
    /// the write and within a short budget of its own ([`PRUNE_BUDGET`]): a failing prune is only logged, and a
    /// caller's timeout that fires during a slow prune fires before anything is stored, so an `add` that stored its
    /// key is never reported as failed (the caller would then retry it, or give up a claim it holds).
    async fn maybe_prune(&self) {
        if !self.should_clean() {
            return;
        }
        let prune = async {
            #[cfg(test)]
            {
                let delay = self
                    .prune_delay_ms
                    .load(std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            for table in [&self.table, &self.locks] {
                self.prune_table(table, PRUNE_BATCH, PRUNE_BATCHES).await?;
            }
            Ok::<(), Error>(())
        };
        match tokio::time::timeout(self.timeout.min(PRUNE_BUDGET), prune).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::debug!(error = %e, "cache: pruning expired rows failed"),
            Err(_) => tracing::debug!("cache: pruning expired rows timed out"),
        }
    }

    /// Delete `table`'s expired rows, at most `batch` per statement and `batches` statements; the rows deleted.
    async fn prune_table(&self, table: &str, batch: u64, batches: usize) -> Result<u64> {
        let mut removed = 0;
        for _ in 0..batches {
            let n = self
                .exec(self.build(&prune_statement(
                    self.db.conn().get_database_backend(),
                    table,
                    batch,
                )))
                .await?;
            removed += n;
            if n < batch {
                break;
            }
        }
        Ok(removed)
    }

    /// Store `value` under `key` (insert or overwrite).
    async fn upsert(&self, key: &str, value: &str, ttl: Option<Duration>) -> Result<()> {
        let mut insert = Query::insert();
        insert
            .into_table(col(&self.table))
            .columns([col("key"), col("value"), col("expiration")])
            .values([
                Value::from(key).into(),
                Value::from(value).into(),
                ms(expires_at(ttl)).into(),
            ])
            .map_err(|e| Error::internal(e.to_string()))?
            .on_conflict(
                OnConflict::column(col("key"))
                    .update_columns([col("value"), col("expiration")])
                    .to_owned(),
            );
        self.exec(self.build(&insert)).await?;
        Ok(())
    }

    async fn add_row(&self, key: &str, value: &str, expires: Option<u64>) -> Result<bool> {
        let row = [
            ("key", Value::from(key)),
            ("value", Value::from(value)),
            ("expiration", ms(expires)),
        ];
        if self.insert(&self.table, row).await? {
            return Ok(true);
        }
        self.take_expired(
            &self.table,
            key,
            [("value", Value::from(value)), ("expiration", ms(expires))],
        )
        .await
    }
}

/// The SQL of `value`'s size in bytes on `backend`, and of the column itself (quoted for that backend).
/// `SELECT <bytes> AS bytes, CASE WHEN <bytes> <= limit THEN value END AS value, expiration … WHERE key = … AND live`.
/// The limit is a typed sea-query value (never a `?` inside custom SQL): sea-query rewrites only the backend's own
/// placeholder inside a custom expression, so a literal `?` reached PostgreSQL unchanged (its placeholders are `$n`).
fn read_query(
    backend: sea_orm::DatabaseBackend,
    table: &str,
    key: &str,
    max_value: usize,
) -> sea_orm::sea_query::SelectStatement {
    let (bytes, value) = byte_length(backend);
    let limit = i64::try_from(max_value).unwrap_or(i64::MAX);
    Query::select()
        .expr_as(Expr::cust(bytes), col("bytes"))
        .expr_as(
            Expr::case(Expr::cust(bytes).lte(limit), Expr::cust(value)),
            col("value"),
        )
        .column(col("expiration"))
        .from(col(table))
        .and_where(Expr::col(col("key")).eq(key))
        .cond_where(live_cond())
        .to_owned()
}

fn byte_length(backend: sea_orm::DatabaseBackend) -> (&'static str, &'static str) {
    match backend {
        sea_orm::DatabaseBackend::MySql => ("LENGTH(`value`)", "`value`"),
        sea_orm::DatabaseBackend::Postgres => ("OCTET_LENGTH(\"value\")", "\"value\""),
        _ => ("LENGTH(CAST(\"value\" AS BLOB))", "\"value\""),
    }
}

/// `SUBSTR(key, 1, <characters of text>)`: the key's head, compared with `text` instead of LIKE, because a prefix
/// may hold `%` or `_`. INTEGER arguments: PostgreSQL has no `substr(…, bigint, bigint)`.
fn key_head(text: &str) -> sea_orm::sea_query::FunctionCall {
    let len = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    Func::cust(col("SUBSTR"))
        .arg(Expr::col(col("key")))
        .arg(1_i32)
        .arg(len)
}

/// One pruning statement: at most `batch` expired rows of `table`. MySQL has `DELETE … LIMIT`; PostgreSQL and SQLite
/// delete the keys of a limited sub-select (MySQL refuses `LIMIT` in an `IN` sub-select of the same table).
fn prune_statement(
    backend: sea_orm::DatabaseBackend,
    table: &str,
    batch: u64,
) -> sea_orm::sea_query::DeleteStatement {
    let mut delete = Query::delete();
    delete.from_table(col(table));
    if backend == sea_orm::DatabaseBackend::MySql {
        delete.cond_where(expired_cond()).limit(batch);
    } else {
        let keys = Query::select()
            .column(col("key"))
            .from(col(table))
            .cond_where(expired_cond())
            .limit(batch)
            .to_owned();
        delete.and_where(Expr::col(col("key")).in_subquery(keys));
    }
    delete
}

impl CacheStore for DatabaseStore {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        let key = stored_key(key);
        Box::pin(self.timed(async move { Ok(self.read(&key).await?.map(|(value, _)| value)) }))
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<()>> {
        let key = stored_key(key);
        Box::pin(async move {
            let key: &str = &key;
            self.maybe_prune().await;
            self.timed(self.upsert(key, value, ttl)).await
        })
    }

    fn add<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let key = stored_key(key);
        Box::pin(async move {
            let key: &str = &key;
            // `add` of ever new keys (idempotency keys, Watchfire's tick claims) would otherwise only grow the table.
            self.maybe_prune().await;
            self.timed(self.add_row(key, value, expires_at(ttl))).await
        })
    }

    fn increment<'a>(&'a self, key: &'a str, by: i64) -> BoxFuture<'a, Result<i64>> {
        let key = stored_key(key);
        Box::pin(self.timed(async move {
            let key: &str = &key;
            for _ in 0..CAS_ATTEMPTS {
                match self.read(key).await? {
                    None => {
                        if self.add_row(key, &by.to_string(), None).await? {
                            return Ok(by);
                        }
                    }
                    Some((current, _)) => {
                        let next = add_to(Some(&current), by)?;
                        if next.to_string() == current {
                            return Ok(next);
                        }
                        let update = Query::update()
                            .table(col(&self.table))
                            .values([(col("value"), Value::from(next.to_string()).into())])
                            .and_where(Expr::col(col("key")).eq(key))
                            .and_where(Expr::col(col("value")).eq(current))
                            .cond_where(live_cond())
                            .to_owned();
                        if self.exec(self.build(&update)).await? > 0 {
                            return Ok(next);
                        }
                    }
                }
                tokio::task::yield_now().await;
            }
            Err(Error::internal(
                "the cache counter kept changing under concurrent writers",
            ))
        }))
    }

    fn forget<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>> {
        let key = stored_key(key);
        Box::pin(self.timed(async move {
            let key: &str = &key;
            let delete = Query::delete()
                .from_table(col(&self.table))
                .and_where(Expr::col(col("key")).eq(key))
                .cond_where(live_cond())
                .to_owned();
            let removed = self.exec(self.build(&delete)).await? > 0;
            // An expired row is removed too, but does not count as "was there".
            let delete = Query::delete()
                .from_table(col(&self.table))
                .and_where(Expr::col(col("key")).eq(key))
                .to_owned();
            self.exec(self.build(&delete)).await?;
            Ok(removed)
        }))
    }

    fn flush<'a>(&'a self, prefix: &'a str, keep: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.timed(async move {
            for table in [&self.table, &self.locks] {
                let mut delete = Query::delete();
                delete.from_table(col(table));
                if !prefix.is_empty() {
                    delete.and_where(Expr::expr(key_head(prefix)).eq(prefix));
                }
                for kept in keep {
                    delete.and_where(Expr::expr(key_head(kept)).ne(kept.as_str()));
                }
                self.exec(self.build(&delete)).await?;
            }
            Ok(())
        }))
    }

    fn acquire_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let name = stored_key(name);
        Box::pin(self.timed(async move {
            let name: &str = &name;
            let expires = expires_at(ttl);
            let row = [
                ("key", Value::from(name)),
                ("owner", Value::from(owner)),
                ("expiration", ms(expires)),
            ];
            if self.insert(&self.locks, row).await? {
                return Ok(true);
            }
            self.take_expired(
                &self.locks,
                name,
                [("owner", Value::from(owner)), ("expiration", ms(expires))],
            )
            .await
        }))
    }

    fn release_lock<'a>(&'a self, name: &'a str, owner: &'a str) -> BoxFuture<'a, Result<bool>> {
        let name = stored_key(name);
        Box::pin(self.timed(async move {
            let name: &str = &name;
            let delete = Query::delete()
                .from_table(col(&self.locks))
                .and_where(Expr::col(col("key")).eq(name))
                .and_where(Expr::col(col("owner")).eq(owner))
                .cond_where(live_cond())
                .to_owned();
            Ok(self.exec(self.build(&delete)).await? > 0)
        }))
    }

    fn refresh_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let name = stored_key(name);
        Box::pin(self.timed(async move {
            let name: &str = &name;
            // One statement: only this owner's live row gets the new expiry.
            let update = Query::update()
                .table(col(&self.locks))
                .values([(col("expiration"), SimpleExpr::from(ms(expires_at(ttl))))])
                .and_where(Expr::col(col("key")).eq(name))
                .and_where(Expr::col(col("owner")).eq(owner))
                .cond_where(live_cond())
                .to_owned();
            Ok(self.exec(self.build(&update)).await? > 0)
        }))
    }

    fn force_release_lock<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        let name = stored_key(name);
        Box::pin(self.timed(async move {
            let name: &str = &name;
            let delete = Query::delete()
                .from_table(col(&self.locks))
                .and_where(Expr::col(col("key")).eq(name))
                .to_owned();
            self.exec(self.build(&delete)).await?;
            Ok(())
        }))
    }

    fn lock_owner<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        let name = stored_key(name);
        Box::pin(self.timed(async move {
            let name: &str = &name;
            let select = Query::select()
                .column(col("owner"))
                .from(col(&self.locks))
                .and_where(Expr::col(col("key")).eq(name))
                .cond_where(live_cond())
                .to_owned();
            let row = self.db.conn().query_one_raw(self.build(&select)).await?;
            Ok(match row {
                Some(row) => Some(row.try_get("", "owner")?),
                None => None,
            })
        }))
    }
}

/// The cache tables, for the app's migration (`smeltery new` writes one that calls these).
///
/// ```
/// use smeltery_core::Result;
/// use smeltery_core::db::migration::{Migration, Schema};
///
/// pub struct CreateCacheTables;
///
/// impl Migration for CreateCacheTables {
///     fn name(&self) -> &'static str {
///         "2026_10_04_000000_create_cache_tables"
///     }
///
///     async fn up(&self, schema: &Schema) -> Result<()> {
///         smeltery_core::cache::migrations::up(schema).await
///     }
///
///     async fn down(&self, schema: &Schema) -> Result<()> {
///         smeltery_core::cache::migrations::down(schema).await
///     }
/// }
/// ```
///
/// | Table | Columns |
/// |---|---|
/// | `cache` (`CACHE_TABLE`) | `key` (unique), `value` (text; `MEDIUMTEXT` on MySQL), `expiration` (Unix ms, `NULL` = never, indexed) |
/// | `cache_locks` (`<CACHE_TABLE>_locks`) | `key` (unique), `owner`, `expiration` (Unix ms, `NULL` = never, indexed) |
pub mod migrations {
    use crate::config::env;
    use crate::db::Backend;
    use crate::db::migration::Schema;
    use crate::error::Result;

    fn table() -> String {
        env("CACHE_TABLE", "cache")
    }

    /// Create the `cache` and `cache_locks` tables.
    ///
    /// # Errors
    /// A table exists already, or a statement fails.
    pub async fn up(schema: &Schema) -> Result<()> {
        let table = table();
        schema
            .create(&table, |t| {
                t.string("key").unique();
                t.text("value");
                t.big_integer("expiration").nullable().index();
            })
            .await?;
        let locks = format!("{table}_locks");
        schema
            .create(&locks, |t| {
                t.string("key").unique();
                t.string("owner");
                t.big_integer("expiration").nullable().index();
            })
            .await?;
        if schema.backend() == Backend::MySql {
            for sql in mysql_statements(&table) {
                schema.raw(&sql).await?;
            }
        }
        Ok(())
    }

    /// MySQL only: `value` as `MEDIUMTEXT` (`TEXT` holds 64 KB; cached pages and lists outgrow it), and the
    /// `key` / `owner` columns compared byte for byte (`utf8mb4_bin`). MySQL's default collation ignores case
    /// and accents, so `profile:Admin` and `profile:ádmin` were one entry and one lock.
    ///
    /// An app whose cache tables were created before these statements existed runs them in a migration of
    /// its own (see the README's Cache section).
    pub fn mysql_statements(table: &str) -> Vec<String> {
        let quote = |name: &str| format!("`{}`", name.replace('`', "``"));
        let entries = quote(table);
        let locks = quote(&format!("{table}_locks"));
        let binary = "VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL";
        vec![
            format!(
                "ALTER TABLE {entries} MODIFY `value` MEDIUMTEXT NOT NULL, MODIFY `key` {binary}"
            ),
            format!("ALTER TABLE {locks} MODIFY `key` {binary}, MODIFY `owner` {binary}"),
        ]
    }

    /// Drop both tables.
    ///
    /// # Errors
    /// A statement fails.
    pub async fn down(schema: &Schema) -> Result<()> {
        let table = table();
        schema.drop_if_exists(&format!("{table}_locks")).await?;
        schema.drop_if_exists(&table).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F-1: the read query used a literal `?` inside custom SQL, which PostgreSQL rejects (its placeholders are
    /// `$1`, `$2` …). Every placeholder must be the backend's own, and the size limit must be a bound value.
    #[test]
    fn the_read_query_uses_each_backends_own_placeholders() {
        use sea_orm::DatabaseBackend as B;
        for backend in [B::Postgres, B::MySql, B::Sqlite] {
            let stmt = backend.build(&read_query(backend, "cache", "k", 1234));
            let sql = stmt.sql.clone();
            let values = stmt.values.map(|v| v.0).unwrap_or_default();
            assert!(sql.contains("CASE WHEN"), "{backend:?}: {sql}");
            assert!(
                values.contains(&Value::from(1234_i64)),
                "{backend:?}: the limit is bound: {values:?}"
            );
            let marks = sql.matches('?').count();
            if backend == B::Postgres {
                assert_eq!(marks, 0, "PostgreSQL never sees `?`: {sql}");
                for n in 1..=values.len() {
                    assert!(sql.contains(&format!("${n}")), "PostgreSQL: ${n} in {sql}");
                }
            } else {
                assert!(!sql.contains("$1"), "{backend:?}: {sql}");
                assert_eq!(marks, values.len(), "{backend:?}: one `?` per value: {sql}");
            }
        }
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn add_prunes_expired_rows() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        migrations::up(&crate::db::migration::Schema::new(&db))
            .await
            .unwrap();
        let store = DatabaseStore::new(db.clone(), "cache", Duration::from_secs(5));
        for i in 0..5 {
            let key = format!("tick:{i}");
            assert!(
                store
                    .add(&key, "x", Some(Duration::from_millis(1)))
                    .await
                    .unwrap()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        store
            .clean_always
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            store
                .add("tick:5", "x", Some(Duration::from_secs(60)))
                .await
                .unwrap()
        );
        let count = db
            .conn()
            .query_one_raw(
                store.build(
                    &Query::select()
                        .expr(Expr::cust("COUNT(*) AS n"))
                        .from(col("cache"))
                        .to_owned(),
                ),
            )
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap();
        assert_eq!(count, 1, "the expired claims are gone");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_failing_prune_never_fails_the_write() {
        // Only the entries table: pruning the (missing) locks table fails.
        let db = Db::connect("sqlite::memory:").await.unwrap();
        crate::db::migration::Schema::new(&db)
            .create("cache", |t| {
                t.string("key").unique();
                t.text("value");
                t.big_integer("expiration").nullable().index();
            })
            .await
            .unwrap();
        let store = DatabaseStore::new(db, "cache", Duration::from_secs(5));
        store
            .clean_always
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            store
                .add("claim", "x", Some(Duration::from_secs(60)))
                .await
                .unwrap(),
            "stored, so `true`"
        );
        assert!(
            !store
                .add("claim", "x", Some(Duration::from_secs(60)))
                .await
                .unwrap()
        );
        store
            .put("k", "v", Some(Duration::from_secs(60)))
            .await
            .unwrap();
        assert_eq!(store.get("k").await.unwrap().as_deref(), Some("v"));
    }

    /// A caller's timeout around `add` (Watchfire's claim) and a slow prune: the answer and the stored row agree.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_slow_prune_never_makes_a_stored_add_look_failed() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        migrations::up(&crate::db::migration::Schema::new(&db))
            .await
            .unwrap();
        let store = DatabaseStore::new(db, "cache", Duration::from_secs(5));
        // Every prune takes far longer than the caller waits.
        store
            .prune_delay_ms
            .store(10_000, std::sync::atomic::Ordering::Relaxed);
        for i in 0..3 {
            let key = format!("claim:{i}");
            store
                .clean_always
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let answer = tokio::time::timeout(
                Duration::from_secs(2),
                store.add(&key, "x", Some(Duration::from_secs(60))),
            )
            .await;
            store
                .clean_always
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let stored = store.get(&key).await.unwrap().is_some();
            match answer {
                Ok(Ok(true)) => assert!(stored, "`true` means stored"),
                Ok(Ok(false)) => panic!("a new key was refused"),
                Ok(Err(e)) => assert!(!stored, "an error, yet stored: {e}"),
                Err(_) => assert!(!stored, "timed out, yet stored"),
            }
            assert!(stored, "the prune's budget leaves the write its time");
        }
    }

    /// Expired rows go in bounded statements; live rows stay.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn pruning_deletes_in_bounded_batches() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        migrations::up(&crate::db::migration::Schema::new(&db))
            .await
            .unwrap();
        let store = DatabaseStore::new(db, "cache", Duration::from_secs(5));
        // Inserted directly: `add` / `put` would prune now and then themselves.
        for i in 0..28 {
            let expiration = if i < 25 { ms(Some(1)) } else { ms(None) };
            let key = if i < 25 {
                format!("old:{i}")
            } else {
                format!("live:{}", i - 25)
            };
            assert!(
                store
                    .insert(
                        "cache",
                        [
                            ("key", Value::from(key)),
                            ("value", Value::from("x")),
                            ("expiration", expiration),
                        ],
                    )
                    .await
                    .unwrap()
            );
        }
        // Two statements of ten: twenty rows, the rest waits for the next prune.
        assert_eq!(store.prune_table("cache", 10, 2).await.unwrap(), 20);
        // The next one stops at the first short batch.
        assert_eq!(store.prune_table("cache", 10, 20).await.unwrap(), 5);
        assert_eq!(store.prune_table("cache", 10, 20).await.unwrap(), 0);
        for i in 0..3 {
            assert!(store.get(&format!("live:{i}")).await.unwrap().is_some());
        }
    }

    #[test]
    fn pruning_statements_are_bounded_on_every_backend() {
        use sea_orm::DatabaseBackend;
        let mysql = prune_statement(DatabaseBackend::MySql, "cache", 500)
            .to_string(sea_orm::sea_query::MysqlQueryBuilder);
        assert!(mysql.starts_with("DELETE FROM `cache` WHERE"), "{mysql}");
        assert!(mysql.ends_with("LIMIT 500"), "{mysql}");
        let pg = prune_statement(DatabaseBackend::Postgres, "cache", 500)
            .to_string(sea_orm::sea_query::PostgresQueryBuilder);
        assert!(
            pg.starts_with(
                r#"DELETE FROM "cache" WHERE "key" IN (SELECT "key" FROM "cache" WHERE"#
            ),
            "{pg}"
        );
        assert!(pg.ends_with("LIMIT 500)"), "{pg}");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_large_value_is_never_fetched() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        migrations::up(&crate::db::migration::Schema::new(&db))
            .await
            .unwrap();
        let store = DatabaseStore::new(db, "cache", Duration::from_secs(5)).max_value(100);
        store.put("big", &"é".repeat(60), None).await.unwrap();
        let err = store.get("big").await.unwrap_err();
        assert!(err.to_string().contains("120 bytes"), "{err}");
        store.put("small", &"é".repeat(50), None).await.unwrap();
        assert_eq!(
            store.get("small").await.unwrap().map(|v| v.len()),
            Some(100)
        );
        assert_eq!(store.get("missing").await.unwrap(), None);
    }

    #[test]
    fn long_keys_are_stored_as_a_head_and_a_hash() {
        let short = "k".repeat(MAX_PLAIN_KEY);
        assert_eq!(stored_key(&short), short.as_str());
        let long = format!("app_cache_search:{}", "é".repeat(300));
        let stored = stored_key(&long);
        assert_eq!(stored.chars().count(), 192);
        assert!(stored.starts_with("app_cache_search:"));
        assert!(stored.ends_with(&crate::crypto::sha256_hex(&long)));
        assert_ne!(stored_key(&format!("{long}x")), stored);
    }

    #[test]
    fn mysql_columns_compare_bytes_and_table_names_are_quoted() {
        let sql = migrations::mysql_statements("ca`che");
        assert_eq!(
            sql[0],
            "ALTER TABLE `ca``che` MODIFY `value` MEDIUMTEXT NOT NULL, MODIFY `key` VARCHAR(255) \
             CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL"
        );
        assert_eq!(
            sql[1],
            "ALTER TABLE `ca``che_locks` MODIFY `key` VARCHAR(255) CHARACTER SET utf8mb4 COLLATE \
             utf8mb4_bin NOT NULL, MODIFY `owner` VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL"
        );
    }

    #[test]
    fn flush_compares_the_key_head() {
        let head = Func::cust(col("SUBSTR"))
            .arg(Expr::col(col("key")))
            .arg(1_i32)
            .arg(3_i32);
        let delete = Query::delete()
            .from_table(col("cache"))
            .and_where(Expr::expr(head).eq("a%_"))
            .to_owned();
        assert_eq!(
            delete.to_string(sea_orm::sea_query::SqliteQueryBuilder),
            r#"DELETE FROM "cache" WHERE SUBSTR("key", 1, 3) = 'a%_'"#
        );
    }
}
