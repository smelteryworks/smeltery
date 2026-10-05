//! The database: the [`Db`] handle, models ([`Record`]), migrations, seeders and factories.
//!
//! Models are [SeaORM 2.0](https://www.sea-ql.org/SeaORM/) entities. Smeltery adds the
//! [`Record`] trait (`Post::all(&db)`, `Post::find(&db, id)`, `post.update(&db, …)` …),
//! route model binding with [`Found`], its own migration schema builder
//! ([`migration::Schema`]), seeders and factories. SeaORM's query API is available through
//! [`prelude`] for everything else, on purpose (see `Db::conn`).
//!
//! The `sqlite`, `postgres` and `mysql` features enable the database drivers. Without any of
//! them the types still compile, and connecting fails with an error naming the missing
//! feature.

use std::path::PathBuf;
use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, DatabaseConnection, DbBackend, DbErr};

use crate::app::App;
use crate::error::{Error, Result};

mod events;
pub mod factory;
pub mod migration;
mod page;
mod record;
pub mod seed;

pub use events::{
    MAX_LISTENER_DEPTH, ModelChange, ModelEvent, ModelListener, listeners_paused, without_listeners,
};
pub use page::{DEFAULT_MAX_PER_PAGE, DEFAULT_PER_PAGE, MAX_PAGE, Page, PageQuery, paginate};
pub(crate) use record::timestamp_value;
pub use record::{ActiveModelOf, Found, PrimaryKeyOf, Record};

/// Everything a model file needs, in one import: `use smeltery::db::prelude::*;`.
///
/// It holds the `sea_orm` crate itself (so the SeaORM macros resolve their `sea_orm::` paths),
/// SeaORM's entity prelude and query traits, `Set` / `NotSet` / `Unchanged`, the chrono and
/// JSON column types (`DateTimeUtc`, `Date`, `Json`, `Uuid` …), `serde`'s derives, and
/// Smeltery's [`Db`], [`Record`] and [`Found`].
///
/// SeaORM's `ModelTrait` is left out: its `delete` would shadow [`Record::delete`]. Import it
/// from `sea_orm` when you need `find_related`.
pub mod prelude {
    pub use sea_orm;
    pub use serde;
    pub use serde::{Deserialize, Serialize};

    // SeaORM's `entity::prelude`, item by item, without `ModelTrait` (see above).
    pub use sea_orm::entity::prelude::{
        ActiveBelongsTo, ActiveEnum, ActiveHasMany, ActiveHasOne, ActiveModelBehavior,
        ActiveModelTrait, BelongsTo, ChronoDate, ChronoDateTime, ChronoDateTimeLocal,
        ChronoDateTimeUtc, ChronoDateTimeWithTimeZone, ChronoTime, ChronoUtc, ColumnDef,
        ColumnTrait, ColumnType, ColumnTypeTrait, ConnectionTrait, CursorTrait, DatabaseConnection,
        Date, DateTime, DateTimeLocal, DateTimeUtc, DateTimeWithTimeZone, DbConn, DbErr,
        DeriveActiveEnum, DeriveActiveModel, DeriveActiveModelBehavior, DeriveActiveModelEx,
        DeriveColumn, DeriveDisplay, DeriveEntity, DeriveEntityModel, DeriveIden,
        DeriveIntoActiveModel, DeriveModel, DeriveModelEx, DerivePartialModel, DerivePrimaryKey,
        DeriveRelatedEntity, DeriveRelation, DeriveValueType, DynIden, EntityName, EntityTrait,
        EnumIter, Expr, ForeignKeyAction, FromJsonQueryResult, HasMany, HasOne, Iden, IdenStatic,
        Json, Linked, LoaderTrait, PaginatorTrait, PrimaryKeyArity, PrimaryKeyToColumn,
        PrimaryKeyTrait, QueryFilter, QueryResult, Related, RelatedSelfVia, RelationDef,
        RelationTrait, Select, SelectExt, StringLen, Time, Uuid, Value, async_trait,
    };
    pub use sea_orm::{
        ActiveValue, Condition, IntoActiveModel, NotSet, Order, QueryOrder, QuerySelect, Set,
        TransactionTrait, Unchanged,
    };

    pub use super::{ActiveModelOf, Db, Found, Page, PageQuery, PrimaryKeyOf, Record};
}

/// The database engine behind a [`Db`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Backend {
    /// SQLite.
    Sqlite,
    /// PostgreSQL.
    Postgres,
    /// MySQL or MariaDB.
    MySql,
}

impl Backend {
    /// The backend a connection URL names (`sqlite:`, `postgres:` / `postgresql:`, `mysql:` /
    /// `mariadb:`), or `None` for any other scheme.
    pub fn from_url(url: &str) -> Option<Self> {
        let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
        match scheme.as_str() {
            "sqlite" => Some(Self::Sqlite),
            "postgres" | "postgresql" => Some(Self::Postgres),
            "mysql" | "mariadb" => Some(Self::MySql),
            _ => None,
        }
    }

    /// The Cargo feature that enables this backend.
    pub fn feature(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
        }
    }

    /// Whether DDL (`CREATE TABLE` …) can run inside a transaction and be rolled back.
    /// True for SQLite and PostgreSQL; MySQL commits DDL implicitly.
    pub fn transactional_ddl(self) -> bool {
        matches!(self, Self::Sqlite | Self::Postgres)
    }

    fn enabled(self) -> bool {
        match self {
            Self::Sqlite => cfg!(feature = "sqlite"),
            Self::Postgres => cfg!(feature = "postgres"),
            Self::MySql => cfg!(feature = "mysql"),
        }
    }

    pub(crate) fn sea(self) -> DbBackend {
        match self {
            Self::Sqlite => DbBackend::Sqlite,
            Self::Postgres => DbBackend::Postgres,
            Self::MySql => DbBackend::MySql,
        }
    }
}

/// Pool options for [`Db::connect_with`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DbOptions {
    /// The most connections the pool opens (`DB_POOL_MAX`).
    pub pool_max: u32,
    /// How long opening a connection, or waiting for a free one, may take
    /// (`DB_CONNECT_TIMEOUT`).
    pub connect_timeout: Duration,
    /// The directory a relative SQLite path is resolved against; `None` uses the process's
    /// current directory. The app sets it to its root (`SMELTERY_ROOT`).
    pub root: Option<PathBuf>,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self {
            pool_max: 10,
            connect_timeout: Duration::from_secs(5),
            root: None,
        }
    }
}

/// How long a SQLite connection waits for another connection's lock before failing with
/// "database is locked".
pub const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

impl DbOptions {
    /// Set the pool size.
    pub fn pool_max(mut self, pool_max: u32) -> Self {
        self.pool_max = pool_max;
        self
    }

    /// Set the connect / acquire timeout.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Resolve a relative SQLite path (`sqlite://database/database.sqlite`) against `dir`.
    pub fn root(mut self, dir: impl Into<PathBuf>) -> Self {
        self.root = Some(dir.into());
        self
    }
}

/// The database handle: a pool of connections, cheap to clone.
///
/// Handlers take it as an argument (`db: Db`); other code gets it from
/// [`App::db`]. The app connects at boot when `DATABASE_URL` is set.
///
/// ```no_run
/// # async fn demo() -> smeltery_core::Result<()> {
/// use smeltery_core::db::Db;
///
/// let db = Db::connect("sqlite::memory:").await?;
/// db.execute("CREATE TABLE notes (body TEXT)").await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Db {
    conn: DatabaseConnection,
    backend: Backend,
    /// The app's model listeners; `None` when there are none, so `Record` writes skip them with one check.
    listeners: Option<std::sync::Arc<events::Listeners>>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the URL: it may hold a password.
        f.debug_struct("Db")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl Db {
    /// Connect with the default pool options (10 connections, 5 s timeout).
    ///
    /// # Errors
    /// The URL names no known backend, the backend's feature is not enabled, or the
    /// database cannot be reached in time.
    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_with(url, DbOptions::default()).await
    }

    /// Connect with these pool options.
    ///
    /// SQLite files are created when missing (their directory must exist); a relative path is
    /// resolved against [`DbOptions::root`] when it is set. File databases run in WAL journal
    /// mode with `synchronous=NORMAL` and wait up to [`SQLITE_BUSY_TIMEOUT`] for a lock, so
    /// readers do not block the writer and several processes (`serve` and `migrate`) can share
    /// the file. An in-memory SQLite URL (`sqlite::memory:`) uses one connection that is never
    /// recycled, because every connection to `:memory:` is a separate, empty database.
    ///
    /// # Errors
    /// See [`Db::connect`].
    pub async fn connect_with(url: &str, options: DbOptions) -> Result<Self> {
        let original = url;
        // sea-orm picks its MySQL driver only for `mysql:` (sea-orm 2.0.4 `src/database/db_connection.rs:875`);
        // `mariadb:` is the same protocol.
        let rewritten = mariadb_as_mysql(url);
        let url = rewritten.as_str();
        let backend = Backend::from_url(url).ok_or_else(|| {
            Error::internal(
                "DATABASE_URL must start with sqlite:, postgres:, postgresql:, mysql: or mariadb:",
            )
        })?;
        if !backend.enabled() {
            return Err(Error::internal(format!(
                "the `{}` feature of smeltery is not enabled, so `{}` databases cannot be used",
                backend.feature(),
                backend.feature()
            )));
        }
        #[cfg(feature = "sqlite")]
        if backend == Backend::Sqlite && !is_sqlite_memory(url) && !sqlite_read_only(url) {
            create_private_sqlite_file(url, options.root.as_deref());
        }
        let mut opts = ConnectOptions::new(url.to_owned());
        opts.max_connections(options.pool_max.max(1))
            .connect_timeout(options.connect_timeout)
            .acquire_timeout(options.connect_timeout)
            .sqlx_logging(false);
        if backend == Backend::Sqlite {
            if is_sqlite_memory(url) {
                // No ping before handing the connection out: a caller giving up during that ping (a timeout)
                // drops the connection it was about to get, and the pool opens a new one (sqlx-core 0.9.0
                // `src/pool/inner.rs:266` and `:469-483`), which for `:memory:` is a new, empty database
                // (D-391). An in-memory connection has no network to lose.
                opts.max_connections(1)
                    .min_connections(1)
                    .idle_timeout(None)
                    .max_lifetime(None)
                    .test_before_acquire(false);
            }
            #[cfg(feature = "sqlite")]
            {
                use sea_orm::sqlx::sqlite::{SqliteJournalMode, SqliteSynchronous};
                let memory = is_sqlite_memory(url);
                // WAL writes `-wal` / `-shm` files next to the database; a read-only or immutable
                // database keeps its own journal mode.
                let wal = !memory && !sqlite_read_only(url);
                let root = options.root.clone().filter(|_| !memory);
                // sea-orm parses the URL, then applies this function (sea-orm 2.0.4
                // `src/driver/sqlx_sqlite.rs:71-97`); sqlx runs the pragmas on every new
                // connection (sqlx-sqlite 0.9.0 `src/options/connect.rs:70-80`).
                opts.map_sqlx_sqlite_opts(move |o| {
                    // `recursive_triggers`: without it `INSERT OR REPLACE` removes the old row without firing its
                    // DELETE triggers (SQLite docs, "ON CONFLICT", REPLACE), so a trigger-kept table such as an FTS5
                    // search index keeps the old row's words. No framework trigger depends on the default (off).
                    let mut o = o
                        .create_if_missing(true)
                        .busy_timeout(SQLITE_BUSY_TIMEOUT)
                        .pragma("recursive_triggers", "ON");
                    // Only a path with neither a root nor a drive is relative: on Windows
                    // `sqlite:///C:/x` gives `/C:/x`, which `is_relative()` calls relative.
                    let file = o.get_filename();
                    if let Some(root) = &root
                        && file.is_relative()
                        && !file.has_root()
                    {
                        let path = root.join(file);
                        o = o.filename(path);
                    }
                    if wal {
                        o = o
                            .journal_mode(SqliteJournalMode::Wal)
                            .synchronous(SqliteSynchronous::Normal);
                    }
                    o
                });
            }
        }
        let conn = tokio::time::timeout(
            options.connect_timeout + Duration::from_secs(1),
            sea_orm::Database::connect(opts),
        )
        .await
        .map_err(|_| Error::internal("timed out connecting to the database"))?
        .map_err(|e| connect_error(&e.to_string(), original, url))?;
        Ok(Self {
            conn,
            backend,
            listeners: None,
        })
    }

    /// This handle with the app's model listeners (none: unchanged).
    pub(crate) fn with_listeners(
        mut self,
        listeners: Vec<std::sync::Arc<dyn ModelListener>>,
    ) -> Self {
        self.listeners =
            (!listeners.is_empty()).then(|| std::sync::Arc::new(events::Listeners::new(listeners)));
        self
    }

    /// Run the listeners on `app`'s owned tasks (the app that built this handle).
    pub(crate) fn set_listener_owner(&self, app: &App) {
        if let Some(listeners) = &self.listeners {
            listeners.set_owner(app.downgrade());
        }
    }

    /// The model listeners, when the app registered any.
    pub(crate) fn listeners(&self) -> Option<&std::sync::Arc<events::Listeners>> {
        self.listeners.as_ref()
    }

    /// The database engine.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The SeaORM connection, for SeaORM's own API (`Entity::find()…all(db.conn())`,
    /// transactions, raw statements). Exposing it is deliberate: Smeltery wraps the
    /// everyday calls in [`Record`] and leaves the full query builder to SeaORM.
    pub fn conn(&self) -> &DatabaseConnection {
        &self.conn
    }

    /// Start a transaction that will write. On SQLite it is `BEGIN IMMEDIATE`: the write lock is taken at the
    /// start, waiting up to the busy timeout for another writer, so a transaction that reads before it writes never
    /// fails with "database is locked" when another one wrote in between (a `BEGIN DEFERRED` reader that later
    /// writes gets `SQLITE_BUSY` at once: the busy timeout cannot help it). Other databases get a plain `BEGIN`.
    /// Use it for every transaction that reads and then writes; commit or roll back the returned transaction.
    ///
    /// # Errors
    /// The connection fails, or the write lock is not free within the busy timeout.
    pub async fn begin_write(&self) -> Result<sea_orm::DatabaseTransaction> {
        use sea_orm::TransactionTrait as _;
        // sea-orm 2.0.4 `src/database/connection.rs:140-174` (`SqliteTransactionMode`, `TransactionOptions`),
        // `src/database/transaction.rs:131-140` (`BEGIN <mode>` for a top-level SQLite transaction).
        let options = sea_orm::TransactionOptions {
            sqlite_transaction_mode: (self.backend == Backend::Sqlite)
                .then_some(sea_orm::SqliteTransactionMode::Immediate),
            ..Default::default()
        };
        Ok(self.conn.begin_with_options(options).await?)
    }

    /// Run one SQL statement without parameters; returns the number of affected rows.
    ///
    /// The text runs as it is: never build it with `format!` from user input. Values go through
    /// [`execute_with`](Self::execute_with) / [`query_with`](Self::query_with), which bind them as parameters.
    ///
    /// # Errors
    /// The statement fails.
    pub async fn execute(&self, sql: &str) -> Result<u64> {
        Ok(self.conn.execute_unprepared(sql).await?.rows_affected())
    }

    /// Run one SQL statement with bound parameters; returns the number of affected rows. Placeholders are the
    /// backend's own: `?` on SQLite and MySQL, `$1`, `$2` … on PostgreSQL.
    ///
    /// ```no_run
    /// # async fn demo(db: smeltery_core::db::Db, email: &str) -> smeltery_core::Result<()> {
    /// db.execute_with("UPDATE users SET active = ? WHERE email = ?", [true.into(), email.into()])
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// The statement fails.
    pub async fn execute_with(
        &self,
        sql: &str,
        values: impl IntoIterator<Item = sea_orm::Value>,
    ) -> Result<u64> {
        let stmt = sea_orm::Statement::from_sql_and_values(self.backend.sea(), sql, values);
        Ok(self.conn.execute_raw(stmt).await?.rows_affected())
    }

    /// Run one query with bound parameters and return its rows (placeholders as in
    /// [`execute_with`](Self::execute_with)). Read a column with `row.try_get::<T>("", "column")`.
    ///
    /// ```no_run
    /// # async fn demo(db: smeltery_core::db::Db) -> smeltery_core::Result<()> {
    /// let rows = db
    ///     .query_with("SELECT name FROM users WHERE id > ?", [10.into()])
    ///     .await?;
    /// for row in rows {
    ///     let name: String = row.try_get("", "name")?;
    ///     println!("{name}");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// The query fails.
    pub async fn query_with(
        &self,
        sql: &str,
        values: impl IntoIterator<Item = sea_orm::Value>,
    ) -> Result<Vec<sea_orm::QueryResult>> {
        let stmt = sea_orm::Statement::from_sql_and_values(self.backend.sea(), sql, values);
        Ok(self.conn.query_all_raw(stmt).await?)
    }

    /// Close every connection of the pool.
    ///
    /// # Errors
    /// The driver reports an error while closing.
    pub async fn close(&self) -> Result<()> {
        Ok(self.conn.close_by_ref().await?)
    }
}

/// `mariadb:…` as `mysql:…` (any case); every other URL unchanged.
fn mariadb_as_mysql(url: &str) -> String {
    match url.split_once(':') {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("mariadb") => format!("mysql:{rest}"),
        _ => url.to_owned(),
    }
}

/// The connect error without the password: sea-orm quotes the whole URL when it cannot parse it or finds no
/// driver for it (sea-orm 2.0.4 `src/database/mod.rs:183-187`, `:211-214`), and that text reaches the console
/// and the log.
fn connect_error(message: &str, original: &str, url: &str) -> Error {
    let clean = crate::config::redact_url_in(&crate::config::redact_url_in(message, url), original);
    let hint = if clean.contains("cannot be parsed") {
        " (percent-encode special characters such as / ? # @ in the user name and password)"
    } else {
        ""
    };
    Error::internal(format!("cannot connect to the database: {clean}{hint}"))
}

/// Create a missing SQLite database file as `0600` (Unix) before sqlx opens it: SQLite would create it with
/// `0644` minus the umask, readable by every local user, and gives the `-wal` / `-shm` files the main file's
/// mode. Best effort: any problem is left for the connect to report.
#[cfg(feature = "sqlite")]
fn create_private_sqlite_file(url: &str, root: Option<&std::path::Path>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let Some(path) = sqlite_path(url, root) else {
            return;
        };
        if path.parent().is_some_and(|dir| dir.is_dir()) {
            // `create_new`: an existing file (or a symlink) is never touched.
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path);
        }
    }
    #[cfg(not(unix))]
    let _ = (url, root);
}

/// The file a SQLite URL names, read the way sqlx-sqlite 0.9.0 does (`src/options/parse.rs:168-180`, `:26-34`)
/// and resolved against `root` like [`Db::connect_with`].
#[cfg(feature = "sqlite")]
#[cfg_attr(not(unix), allow(dead_code))]
fn sqlite_path(url: &str, root: Option<&std::path::Path>) -> Option<PathBuf> {
    let rest = url
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:");
    let file = rest.split('?').next().unwrap_or_default();
    if file.is_empty() || file == ":memory:" {
        return None;
    }
    let file = PathBuf::from(crate::config::percent_decode(file));
    Some(match root {
        Some(root) if file.is_relative() && !file.has_root() => root.join(file),
        _ => file,
    })
}

/// `sqlite::memory:`, `sqlite://:memory:`, or a URL with `mode=memory`.
fn is_sqlite_memory(url: &str) -> bool {
    url.contains(":memory:") || url.contains("mode=memory")
}

/// A SQLite URL with `mode=ro` or `immutable=true|1` in its query (the values sqlx-sqlite
/// 0.9.0 accepts, `src/options/parse.rs:89-95`).
#[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
fn sqlite_read_only(url: &str) -> bool {
    let Some((_, query)) = url.split_once('?') else {
        return false;
    };
    form_urlencoded::parse(query.as_bytes()).any(|(key, value)| {
        (key == "mode" && value == "ro")
            || (key == "immutable" && (value == "true" || value == "1"))
    })
}

impl From<DbErr> for Error {
    fn from(error: DbErr) -> Self {
        Self::other(error)
    }
}

impl App {
    /// The database handle.
    ///
    /// # Errors
    /// No database is configured (`DATABASE_URL` is empty).
    pub fn db(&self) -> Result<Db> {
        self.service::<Db>()
            .map(|db| (*db).clone())
            .ok_or_else(|| Error::internal("no database is configured: set DATABASE_URL in .env"))
    }
}

impl axum::extract::FromRequestParts<App> for Db {
    type Rejection = Error;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        app.db()
    }
}

/// Every table in the current database/schema: on SQLite the ordinary and the virtual tables (an FTS5 search index
/// is one table), without SQLite's internal tables and without the shadow tables a virtual table keeps its data in
/// (`posts_search_data`, `_idx`, `_content`, `_docsize`, `_config`).
pub(crate) async fn table_names(db: &Db) -> Result<Vec<String>> {
    let sql = match db.backend {
        Backend::Sqlite => {
            let tables = sqlite_tables(db).await?;
            return Ok(tables.into_iter().map(|(name, _)| name).collect());
        }
        Backend::Postgres => {
            "SELECT tablename::text AS name FROM pg_catalog.pg_tables \
             WHERE schemaname = current_schema() ORDER BY tablename"
        }
        Backend::MySql => {
            "SELECT CAST(table_name AS CHAR) AS name FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' ORDER BY table_name"
        }
    };
    let rows = db
        .conn
        .query_all_raw(sea_orm::Statement::from_string(db.backend.sea(), sql))
        .await?;
    rows.iter()
        .map(|row| row.try_get::<String>("", "name").map_err(Error::from))
        .collect()
}

/// SQLite's ordinary and virtual tables of the `main` schema, by name, each with whether it is virtual.
/// `PRAGMA table_list` (SQLite 3.37+; the bundled library is 3.51) types a virtual table's own storage tables as
/// `shadow` (sqlite3.c `PragTyp_TABLE_LIST`, `sqlite3ShadowTableName`, `sqlite3MarkAllShadowTablesOf`), so they are
/// left out: dropping the virtual table drops them.
async fn sqlite_tables(db: &Db) -> Result<Vec<(String, bool)>> {
    let sql = "SELECT name AS name, type AS type FROM pragma_table_list \
               WHERE schema = 'main' AND type IN ('table', 'virtual') \
               AND name NOT LIKE 'sqlite_%' ORDER BY name";
    let rows = db
        .conn
        .query_all_raw(sea_orm::Statement::from_string(db.backend.sea(), sql))
        .await?;
    rows.iter()
        .map(|row| {
            let name = row.try_get::<String>("", "name")?;
            let kind = row.try_get::<String>("", "type")?;
            Ok((name, kind == "virtual"))
        })
        .collect()
}

/// Drop every table, ignoring foreign keys between them. On SQLite the virtual tables go first (they own their
/// shadow tables, and an FTS5 index reads the table it indexes), then the rest.
pub(crate) async fn drop_all_tables(db: &Db) -> Result<Vec<String>> {
    let tables = if db.backend == Backend::Sqlite {
        let mut tables = sqlite_tables(db).await?;
        // A stable sort: the virtual tables first, each group by name.
        tables.sort_by_key(|(_, is_virtual)| !*is_virtual);
        tables.into_iter().map(|(name, _)| name).collect()
    } else {
        table_names(db).await?
    };
    if tables.is_empty() {
        return Ok(tables);
    }
    let drops: Vec<String> = tables
        .iter()
        .map(|t| migration::schema::drop_table_sql(db.backend, t, true, true))
        .collect();
    match db.backend {
        Backend::Postgres => {
            // `CASCADE` removes the constraints that point at each table.
            for sql in &drops {
                db.execute(sql).await?;
            }
        }
        Backend::Sqlite => drop_sqlite(db, &drops).await?,
        Backend::MySql => drop_mysql(db, &drops).await?,
    }
    Ok(tables)
}

/// A pool connection whose foreign-key checks are switched off: dropped before [`restored`](Self::restored) (an
/// error on the way, a cancelled caller) it is closed instead of going back to the pool, so no later query runs
/// on a connection without foreign-key checks.
#[cfg(any(feature = "sqlite", feature = "mysql"))]
struct ChecksOff<DB: sea_orm::sqlx::Database>(Option<sea_orm::sqlx::pool::PoolConnection<DB>>);

#[cfg(any(feature = "sqlite", feature = "mysql"))]
impl<DB: sea_orm::sqlx::Database> ChecksOff<DB> {
    fn conn(&mut self) -> Result<&mut DB::Connection> {
        self.0
            .as_deref_mut()
            .ok_or_else(|| Error::internal("the connection is gone"))
    }

    /// The checks are on again: the connection may go back to the pool.
    fn restored(mut self) {
        drop(self.0.take());
    }
}

#[cfg(any(feature = "sqlite", feature = "mysql"))]
impl<DB: sea_orm::sqlx::Database> Drop for ChecksOff<DB> {
    fn drop(&mut self) {
        if let Some(conn) = self.0.as_mut() {
            conn.close_on_drop();
        }
    }
}

/// Test hook: stop `drop_sqlite` right before it turns the checks back on (a failure or a cancelled caller there).
#[cfg(all(test, feature = "sqlite"))]
static STOP_BEFORE_RESTORE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// SQLite: `PRAGMA foreign_keys` is per connection and ignored inside a transaction, so
/// the drops run on one pool connection with the checks off, then turn them back on.
#[cfg(feature = "sqlite")]
async fn drop_sqlite(db: &Db, drops: &[String]) -> Result<()> {
    use sea_orm::sqlx::{self, AssertSqlSafe};
    let conn = db
        .conn
        .get_sqlite_connection_pool()
        .acquire()
        .await
        .map_err(Error::other)?;
    let mut off = ChecksOff(Some(conn));
    sqlx::raw_sql("PRAGMA foreign_keys = OFF")
        .execute(off.conn()?)
        .await
        .map_err(Error::other)?;
    let mut result = Ok(());
    for sql in drops {
        if let Err(e) = sqlx::raw_sql(AssertSqlSafe(sql.clone()))
            .execute(off.conn()?)
            .await
        {
            result = Err(Error::other(e));
            break;
        }
    }
    #[cfg(test)]
    if STOP_BEFORE_RESTORE.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(Error::internal("stopped before the checks were restored"));
    }
    sqlx::raw_sql("PRAGMA foreign_keys = ON")
        .execute(off.conn()?)
        .await
        .map_err(Error::other)?;
    off.restored();
    result
}

#[cfg(not(feature = "sqlite"))]
async fn drop_sqlite(_db: &Db, _drops: &[String]) -> Result<()> {
    Err(Error::internal("the `sqlite` feature is not enabled"))
}

/// MySQL: `FOREIGN_KEY_CHECKS` is per session, so the drops run on one pool connection.
#[cfg(feature = "mysql")]
async fn drop_mysql(db: &Db, drops: &[String]) -> Result<()> {
    use sea_orm::sqlx::{self, AssertSqlSafe};
    let conn = db
        .conn
        .get_mysql_connection_pool()
        .acquire()
        .await
        .map_err(Error::other)?;
    let mut off = ChecksOff(Some(conn));
    sqlx::raw_sql("SET FOREIGN_KEY_CHECKS = 0")
        .execute(off.conn()?)
        .await
        .map_err(Error::other)?;
    let mut result = Ok(());
    for sql in drops {
        if let Err(e) = sqlx::raw_sql(AssertSqlSafe(sql.clone()))
            .execute(off.conn()?)
            .await
        {
            result = Err(Error::other(e));
            break;
        }
    }
    sqlx::raw_sql("SET FOREIGN_KEY_CHECKS = 1")
        .execute(off.conn()?)
        .await
        .map_err(Error::other)?;
    off.restored();
    result
}

#[cfg(not(feature = "mysql"))]
async fn drop_mysql(_db: &Db, _drops: &[String]) -> Result<()> {
    Err(Error::internal("the `mysql` feature is not enabled"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_from_url() {
        assert_eq!(Backend::from_url("sqlite::memory:"), Some(Backend::Sqlite));
        assert_eq!(
            Backend::from_url("postgresql://u@h/db"),
            Some(Backend::Postgres)
        );
        assert_eq!(Backend::from_url("mysql://u@h/db"), Some(Backend::MySql));
        assert_eq!(Backend::from_url("redis://x"), None);
        assert!(is_sqlite_memory("sqlite::memory:"));
        assert!(is_sqlite_memory("sqlite://file?mode=memory&cache=shared"));
        assert!(!is_sqlite_memory("sqlite://database/app.sqlite"));
    }

    #[tokio::test]
    async fn connect_rejects_unknown_schemes() {
        let err = Db::connect("redis://localhost").await.unwrap_err();
        assert!(err.to_string().contains("DATABASE_URL must start with"));
    }

    #[test]
    fn mariadb_urls_use_the_mysql_driver() {
        assert_eq!(mariadb_as_mysql("mariadb://u:p@h/db"), "mysql://u:p@h/db");
        assert_eq!(mariadb_as_mysql("MariaDB://u@h/db"), "mysql://u@h/db");
        assert_eq!(mariadb_as_mysql("mysql://u@h/db"), "mysql://u@h/db");
        assert_eq!(mariadb_as_mysql("sqlite::memory:"), "sqlite::memory:");
        assert_eq!(Backend::from_url("mariadb://u@h/db"), Some(Backend::MySql));
    }

    /// Nothing listens on 127.0.0.1:1, so these fail without any network traffic beyond a refused connect.
    #[cfg(feature = "mysql")]
    #[tokio::test]
    async fn connect_errors_never_show_the_password() {
        let options = DbOptions::default().connect_timeout(Duration::from_secs(2));
        for url in [
            // sea-orm had no driver for `mariadb:` and quoted the URL.
            "mariadb://app:S3CRET_PW@127.0.0.1:1/db",
            // An unescaped `/` in the password: the URL does not parse, and sea-orm quoted it.
            "mysql://app:S3CRET/PW@127.0.0.1:1/db",
        ] {
            let err = Db::connect_with(url, options.clone())
                .await
                .unwrap_err()
                .to_string();
            assert!(!err.contains("S3CRET"), "{err}");
            assert!(!err.contains("no supporting driver"), "{err}");
        }
        let err = Db::connect_with("mysql://app:S3CRET/PW@127.0.0.1:1/db", options)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("percent-encode"), "{err}");
    }

    #[test]
    fn connect_errors_are_redacted() {
        let url = "postgres://app:hunter22@db.internal/app";
        let err = connect_error(
            &format!("The connection string '{url}' has no supporting driver."),
            url,
            url,
        )
        .to_string();
        assert!(!err.contains("hunter22"), "{err}");
        assert!(err.contains("postgres://***@db.internal/app"), "{err}");
    }

    #[cfg(all(unix, feature = "sqlite"))]
    #[tokio::test]
    async fn a_new_sqlite_database_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::connect_with(
            "sqlite://app.sqlite?mode=rwc",
            DbOptions::default().root(dir.path()),
        )
        .await
        .unwrap();
        db.execute("CREATE TABLE t (id INTEGER)").await.unwrap();
        for name in ["app.sqlite", "app.sqlite-wal", "app.sqlite-shm"] {
            let path = dir.path().join(name);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode & 0o077, 0, "{name} is {mode:o}");
        }
        db.close().await.unwrap();
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn bound_parameters_carry_values_as_data() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.execute("CREATE TABLE notes (body TEXT)").await.unwrap();
        let hostile = "x'); DROP TABLE notes; --";
        assert_eq!(
            db.execute_with("INSERT INTO notes (body) VALUES (?)", [hostile.into()])
                .await
                .unwrap(),
            1
        );
        let rows = db
            .query_with("SELECT body FROM notes WHERE body = ?", [hostile.into()])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].try_get::<String>("", "body").unwrap(), hostile);
    }

    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn connect_names_the_missing_feature() {
        let err = Db::connect("postgres://u@localhost/db").await.unwrap_err();
        assert!(err.to_string().contains("`postgres` feature"), "{err}");
    }

    /// Write transactions that read first, running at once on a SQLite file, all commit (`BEGIN IMMEDIATE`).
    #[cfg(feature = "sqlite")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn write_transactions_that_read_first_do_not_lock_each_other_out() {
        use sea_orm::ConnectionTrait as _;
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path()
                .join("db.sqlite")
                .display()
                .to_string()
                .replace('\\', "/")
        );
        let db = Db::connect(&url).await.unwrap();
        db.execute("CREATE TABLE counters (n INTEGER)")
            .await
            .unwrap();
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let db = db.clone();
                tokio::spawn(async move {
                    let txn = db.begin_write().await?;
                    let backend = txn.get_database_backend();
                    txn.query_all_raw(sea_orm::Statement::from_string(
                        backend,
                        "SELECT count(*) FROM counters",
                    ))
                    .await?;
                    txn.execute_raw(sea_orm::Statement::from_string(
                        backend,
                        "INSERT INTO counters (n) VALUES (1)",
                    ))
                    .await?;
                    txn.commit().await?;
                    Ok::<_, Error>(())
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        let rows = db
            .query_with("SELECT count(*) AS n FROM counters", [])
            .await
            .unwrap();
        let n: i64 = rows[0].try_get("", "n").unwrap();
        assert_eq!(n, 16);
    }

    /// A query given up (a caller's timeout) just as the pool hands it the only connection must not cost the
    /// in-memory database: the connection goes back to the pool, never closed and replaced by a new, empty one.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_query_cancelled_at_the_hand_over_never_replaces_the_memory_database() {
        use sea_orm::TransactionTrait as _;
        use std::future::Future as _;
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.execute("CREATE TABLE notes (body TEXT)").await.unwrap();
        let pool = db.conn().get_sqlite_connection_pool().clone();
        // Real time, never paused: sqlx's SQLite worker is its own thread, and paused time jumps past the pool's
        // acquire timeout while that thread is busy (CLAUDE.md §8). The timing does not depend on the clock's
        // speed: the waiting query is simply not polled while its budget runs out.
        let mut gave_up = 0;
        for _ in 0..5 {
            let held = db.conn().begin().await.unwrap();
            // A query with a 50 ms budget starts waiting for the connection, and is then not polled for a while
            // (its task is busy elsewhere).
            let mut waiting = Box::pin(tokio::time::timeout(
                Duration::from_millis(50),
                db.execute("SELECT count(*) FROM notes"),
            ));
            let first =
                std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
            assert!(first.is_pending());
            // The connection is released and handed to the waiting query.
            held.commit().await.unwrap();
            let released = std::time::Instant::now();
            while pool.num_idle() == 0 {
                assert!(
                    released.elapsed() < Duration::from_secs(10),
                    "the connection went back to the pool"
                );
                tokio::task::yield_now().await;
            }
            // Its budget runs out before it is polled again. On that poll `timeout` polls the query first (it
            // takes the connection and starts on it), then sees the deadline passed and gives up, dropping it
            // mid-way. Rarely the SQLite thread answers the whole query within that one poll (the test thread was
            // descheduled in between): then nothing was given up, and the round is repeated.
            tokio::time::sleep(Duration::from_millis(150)).await;
            if waiting.await.is_err() {
                gave_up += 1;
            }

            db.execute("SELECT count(*) FROM notes")
                .await
                .expect("the same database");
        }
        assert!(gave_up > 0, "the query gave up at the hand-over");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn memory_database_keeps_its_data_and_lists_tables() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.execute("CREATE TABLE a (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        db.execute("CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id))")
            .await
            .unwrap();
        db.execute("INSERT INTO a (id) VALUES (1)").await.unwrap();
        db.execute("INSERT INTO b (id, a_id) VALUES (1, 1)")
            .await
            .unwrap();
        assert_eq!(table_names(&db).await.unwrap(), ["a", "b"]);
        assert_eq!(drop_all_tables(&db).await.unwrap(), ["a", "b"]);
        assert!(table_names(&db).await.unwrap().is_empty());
        assert_eq!(db.backend(), Backend::Sqlite);
        assert_eq!(format!("{db:?}"), "Db { backend: Sqlite, .. }");
    }

    /// An FTS5 search index over `posts`, kept current by triggers (the shape Prospect's migration helper writes).
    #[cfg(feature = "sqlite")]
    async fn posts_with_a_search_index(db: &Db) {
        for sql in [
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT NOT NULL)",
            "CREATE VIRTUAL TABLE posts_search USING fts5(title, content='posts', content_rowid='id')",
            "CREATE TRIGGER posts_search_ai AFTER INSERT ON posts BEGIN \
             INSERT INTO posts_search(rowid, title) VALUES (new.id, new.title); END",
            "CREATE TABLE zebras (id INTEGER PRIMARY KEY)",
            "INSERT INTO posts (title) VALUES ('forge')",
        ] {
            db.execute(sql).await.unwrap();
        }
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn virtual_tables_are_listed_once_without_their_shadow_tables() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        posts_with_a_search_index(&db).await;
        assert_eq!(
            table_names(&db).await.unwrap(),
            ["posts", "posts_search", "zebras"]
        );
        // The virtual table goes first, so its triggers and shadow tables never outlive it half-way.
        assert_eq!(
            drop_all_tables(&db).await.unwrap(),
            ["posts_search", "posts", "zebras"]
        );
        let left = db
            .query_with("SELECT name FROM sqlite_master", [])
            .await
            .unwrap();
        assert!(left.is_empty(), "{} schema objects left", left.len());
        // Twice in a row (`migrate:fresh` after `migrate:fresh`).
        posts_with_a_search_index(&db).await;
        assert_eq!(drop_all_tables(&db).await.unwrap().len(), 3);
        assert!(table_names(&db).await.unwrap().is_empty());
    }

    /// `INSERT OR REPLACE` fires the DELETE triggers of the row it replaces (`recursive_triggers` is on), so an
    /// FTS5 index kept by triggers drops the old row's words and stays consistent.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn insert_or_replace_fires_delete_triggers() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        posts_with_a_search_index(&db).await;
        db.execute(
            "CREATE TRIGGER posts_search_ad AFTER DELETE ON posts BEGIN              INSERT INTO posts_search(posts_search, rowid, title) VALUES ('delete', old.id, old.title); END",
        )
        .await
        .unwrap();
        db.execute("INSERT OR REPLACE INTO posts (id, title) VALUES (1, 'anvil')")
            .await
            .unwrap();
        let hits = |word: &'static str| {
            let db = db.clone();
            async move {
                db.query_with(
                    "SELECT rowid FROM posts_search WHERE posts_search MATCH ?",
                    [word.into()],
                )
                .await
                .unwrap()
                .len()
            }
        };
        assert_eq!(hits("forge").await, 0, "the replaced row's words are gone");
        assert_eq!(hits("anvil").await, 1);
        db.execute("INSERT INTO posts_search(posts_search, rank) VALUES('integrity-check', 1)")
            .await
            .unwrap();
    }

    /// With `recursive_triggers` on, a trigger that writes to its own table fires itself again; the README's guard
    /// (`WHEN new.x IS NOT old.x`) keeps such a trigger working.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_guarded_self_updating_trigger_works() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        for sql in [
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL, slug TEXT)",
            "CREATE TRIGGER notes_slug AFTER UPDATE ON notes WHEN new.slug IS NOT lower(new.body) BEGIN \
             UPDATE notes SET slug = lower(new.body) WHERE id = new.id; END",
            "INSERT INTO notes (body) VALUES ('Forge')",
            "UPDATE notes SET body = 'Anvil' WHERE id = 1",
        ] {
            db.execute(sql).await.unwrap();
        }
        let rows = db.query_with("SELECT slug FROM notes", []).await.unwrap();
        assert_eq!(rows[0].try_get::<String>("", "slug").unwrap(), "anvil");
        // The same trigger without a guard recurses until SQLite's trigger depth limit and fails.
        db.execute("DROP TRIGGER notes_slug").await.unwrap();
        db.execute(
            "CREATE TRIGGER notes_loop AFTER UPDATE ON notes BEGIN \
             UPDATE notes SET slug = lower(new.body) WHERE id = new.id; END",
        )
        .await
        .unwrap();
        assert!(
            db.execute("UPDATE notes SET body = 'Bellows' WHERE id = 1")
                .await
                .is_err()
        );
    }

    /// A wipe stopped while foreign-key checks are off never hands that connection back to the pool.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn foreign_key_checks_of_a_stopped_wipe_never_reach_the_pool() {
        // Sweep W7-08: one pooled connection, so a connection put back with the checks off would serve the next
        // query.
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path()
                .join("fk.sqlite")
                .display()
                .to_string()
                .replace('\\', "/")
        );
        let db = Db::connect_with(&url, DbOptions::default().pool_max(1))
            .await
            .unwrap();
        db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        STOP_BEFORE_RESTORE.store(true, std::sync::atomic::Ordering::SeqCst);
        let stopped = drop_all_tables(&db).await;
        STOP_BEFORE_RESTORE.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(stopped.is_err());
        let rows = db.query_with("PRAGMA foreign_keys", []).await.unwrap();
        let on: i64 = rows[0].try_get("", "foreign_keys").unwrap();
        assert_eq!(
            on, 1,
            "a connection with the checks off went back to the pool"
        );
    }

    /// A new connection reads the schema from the file; the shadow tables are still recognised.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn shadow_tables_stay_hidden_after_a_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("app.sqlite").display()
        );
        let db = Db::connect(&url).await.unwrap();
        posts_with_a_search_index(&db).await;
        db.close().await.unwrap();
        let db = Db::connect(&url).await.unwrap();
        assert_eq!(
            table_names(&db).await.unwrap(),
            ["posts", "posts_search", "zebras"]
        );
        assert_eq!(drop_all_tables(&db).await.unwrap().len(), 3);
        db.close().await.unwrap();
    }
}
