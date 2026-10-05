//! The schema builder migrations use: [`Schema`] runs statements, [`Blueprint`] describes a
//! table's columns, and the SQL comes from sea-query for the connected backend.

use sea_orm::sea_query::{
    ColumnDef, ForeignKey, ForeignKeyAction, ForeignKeyCreateStatement, Index, MysqlQueryBuilder,
    PostgresQueryBuilder, SqliteQueryBuilder, Table, TableAlterStatement, TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement, Value};

use crate::db::{Backend, Db};
use crate::error::{Error, Result};

/// Runs schema changes inside a migration. Every statement goes through the migration's
/// transaction when the backend has transactional DDL (SQLite, PostgreSQL).
///
/// ```
/// use smeltery_core::db::migration::Schema;
/// use smeltery_core::Result;
///
/// async fn up(schema: &Schema) -> Result<()> {
///     schema
///         .create("comments", |t| {
///             t.id();
///             t.foreign_id("post_id").constrained("posts").cascade_on_delete();
///             t.text("body");
///             t.boolean("approved").default(false);
///             t.timestamps();
///         })
///         .await
/// }
/// ```
pub struct Schema {
    exec: Exec,
    backend: Backend,
}

enum Exec {
    Conn(DatabaseConnection),
    Txn(DatabaseTransaction),
}

impl std::fmt::Debug for Schema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Schema")
            .field("backend", &self.backend)
            .field("transaction", &matches!(self.exec, Exec::Txn(_)))
            .finish()
    }
}

impl Schema {
    /// A schema over the database outside any migration (no transaction), e.g. to check
    /// [`has_table`](Self::has_table) from a seeder or a test.
    pub fn new(db: &Db) -> Self {
        Self {
            exec: Exec::Conn(db.conn().clone()),
            backend: db.backend(),
        }
    }

    /// A schema inside a new transaction when the backend has transactional DDL.
    pub(crate) async fn begin(db: &Db) -> Result<Self> {
        if !db.backend().transactional_ddl() {
            return Ok(Self::new(db));
        }
        let txn = sea_orm::TransactionTrait::begin(db.conn()).await?;
        if db.backend() == Backend::Sqlite {
            // A pooled SQLite connection keeps its own copy of the schema and re-reads it only
            // when a statement notices the change. `ALTER TABLE … DROP/RENAME COLUMN` of a
            // column missing from that stale copy fails at prepare time without the re-read
            // (sqlite3.c `sqlite3AlterDropColumn` / `sqlite3AlterRenameColumn` never set
            // `checkSchema`). Reading `sqlite_master` first starts the transaction's read,
            // whose schema-cookie check drops a stale copy; the open transaction then keeps
            // the schema fixed until commit.
            txn.execute_unprepared("SELECT count(*) FROM sqlite_master")
                .await?;
        }
        Ok(Self {
            exec: Exec::Txn(txn),
            backend: db.backend(),
        })
    }

    /// Commit the transaction, if there is one.
    pub(crate) async fn commit(self) -> Result<()> {
        if let Exec::Txn(txn) = self.exec {
            txn.commit().await?;
        }
        Ok(())
    }

    /// Roll the transaction back, if there is one.
    pub(crate) async fn rollback(self) -> Result<()> {
        if let Exec::Txn(txn) = self.exec {
            txn.rollback().await?;
        }
        Ok(())
    }

    /// The database engine.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub(crate) async fn exec_raw(&self, stmt: Statement) -> Result<u64> {
        let res = match &self.exec {
            Exec::Conn(c) => c.execute_raw(stmt).await?,
            Exec::Txn(t) => t.execute_raw(stmt).await?,
        };
        Ok(res.rows_affected())
    }

    pub(crate) async fn query_raw(&self, stmt: Statement) -> Result<Vec<sea_orm::QueryResult>> {
        Ok(match &self.exec {
            Exec::Conn(c) => c.query_all_raw(stmt).await?,
            Exec::Txn(t) => t.query_all_raw(stmt).await?,
        })
    }

    /// Run the statements of one schema change. Outside a migration on SQLite they run in one transaction on one
    /// pool connection that first reads `sqlite_master`: a pooled SQLite connection keeps its own copy of the schema,
    /// and `CREATE TABLE` / `CREATE INDEX` check "already exists" against that copy at prepare time without re-reading
    /// it (sqlite3.c `sqlite3StartTable` / `sqlite3CreateIndex` never set `checkSchema`), so a table another
    /// connection had dropped was reported as existing. The read's schema-cookie check drops a stale copy (as in
    /// [`Schema::begin`] for migrations).
    async fn run_all(&self, statements: Vec<String>) -> Result<()> {
        if let Exec::Conn(conn) = &self.exec
            && self.backend == Backend::Sqlite
        {
            let txn = sea_orm::TransactionTrait::begin(conn).await?;
            txn.execute_unprepared("SELECT count(*) FROM sqlite_master")
                .await?;
            for sql in statements {
                txn.execute_raw(Statement::from_string(self.backend.sea(), sql.clone()))
                    .await
                    .map_err(|e| Error::internal(format!("{e} (while running: {sql})")))?;
            }
            txn.commit().await?;
            return Ok(());
        }
        for sql in statements {
            self.raw(&sql).await?;
        }
        Ok(())
    }

    /// Create a table; `define` adds its columns.
    ///
    /// # Errors
    /// The table exists, or a statement fails.
    pub async fn create(&self, table: &str, define: impl FnOnce(&mut Blueprint)) -> Result<()> {
        let mut blueprint = Blueprint::new(table);
        define(&mut blueprint);
        self.run_all(blueprint.create_sql(self.backend, false)?)
            .await
    }

    /// Add columns (and their indexes and foreign keys) to an existing table.
    ///
    /// # Errors
    /// A statement fails.
    pub async fn table(&self, table: &str, define: impl FnOnce(&mut Blueprint)) -> Result<()> {
        let mut blueprint = Blueprint::new(table);
        define(&mut blueprint);
        self.run_all(blueprint.alter_sql(self.backend)?).await
    }

    /// Drop a table.
    ///
    /// # Errors
    /// The table does not exist, or the statement fails.
    pub async fn drop(&self, table: &str) -> Result<()> {
        self.run_all(vec![drop_table_sql(self.backend, table, false, false)])
            .await
    }

    /// Drop a table when it exists.
    ///
    /// # Errors
    /// The statement fails.
    pub async fn drop_if_exists(&self, table: &str) -> Result<()> {
        self.run_all(vec![drop_table_sql(self.backend, table, true, false)])
            .await
    }

    /// Rename a table.
    ///
    /// # Errors
    /// The statement fails.
    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let sql = build(
            self.backend,
            Table::rename().table(from.to_owned(), to.to_owned()),
        );
        self.run_all(vec![sql]).await
    }

    /// Whether the table exists.
    ///
    /// # Errors
    /// The catalog query fails.
    pub async fn has_table(&self, table: &str) -> Result<bool> {
        let sql = match self.backend {
            Backend::Sqlite => "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            Backend::Postgres => {
                "SELECT tablename FROM pg_catalog.pg_tables \
                 WHERE schemaname = current_schema() AND tablename = $1"
            }
            Backend::MySql => {
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = DATABASE() AND table_name = ?"
            }
        };
        let stmt = Statement::from_sql_and_values(
            self.backend.sea(),
            sql,
            [Value::from(table.to_owned())],
        );
        Ok(!self.query_raw(stmt).await?.is_empty())
    }

    /// Run SQL as written (one statement). The text is not escaped in any way: build it from constants, never
    /// from outside input (values go through [`Db::execute_with`](crate::db::Db::execute_with)).
    ///
    /// # Errors
    /// The statement fails.
    pub async fn raw(&self, sql: &str) -> Result<()> {
        self.exec_raw(Statement::from_string(self.backend.sea(), sql.to_owned()))
            .await
            .map_err(|e| Error::internal(format!("{e} (while running: {sql})")))?;
        Ok(())
    }
}

/// The columns of a table being created or changed, filled inside
/// [`Schema::create`] / [`Schema::table`].
///
/// Columns are `NOT NULL` unless marked [`nullable`](ColumnBuilder::nullable).
#[derive(Debug)]
pub struct Blueprint {
    table: String,
    columns: Vec<Column>,
    /// Columns to drop ([`Blueprint::drop_column`], [`Schema::table`] only).
    drops: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Id,
    String(u32),
    Text,
    Integer,
    BigInteger,
    Boolean,
    Float,
    Double,
    Decimal(u32, u32),
    Date,
    DateTime,
    Json,
    Uuid,
}

#[derive(Clone, Debug)]
struct Column {
    name: String,
    kind: Kind,
    nullable: bool,
    unique: bool,
    index: bool,
    default: Option<Value>,
    foreign: Option<Foreign>,
}

#[derive(Clone, Debug)]
struct Foreign {
    table: String,
    column: String,
    cascade: bool,
}

/// One column being declared; returned by the [`Blueprint`] methods to add modifiers.
#[derive(Debug)]
pub struct ColumnBuilder<'a> {
    column: &'a mut Column,
}

impl ColumnBuilder<'_> {
    /// Allow `NULL` (the Rust field is then an `Option`).
    pub fn nullable(self) -> Self {
        self.column.nullable = true;
        self
    }

    /// Add a `UNIQUE` constraint.
    pub fn unique(self) -> Self {
        self.column.unique = true;
        self
    }

    /// Add an index on the column (named `<table>_<column>_index`).
    pub fn index(self) -> Self {
        self.column.index = true;
        self
    }

    /// The value used when an insert gives none: `.default(0)`, `.default("draft")`,
    /// `.default(false)`.
    pub fn default(self, value: impl Into<Value>) -> Self {
        self.column.default = Some(value.into());
        self
    }

    /// A foreign key to `table`'s `id` column (named `<table>_<column>_foreign`).
    pub fn constrained(self, table: &str) -> Self {
        self.column.foreign = Some(Foreign {
            table: table.to_owned(),
            column: "id".to_owned(),
            cascade: false,
        });
        self
    }

    /// Delete this row when the row it points at is deleted (`ON DELETE CASCADE`). Call it
    /// after [`constrained`](Self::constrained).
    pub fn cascade_on_delete(self) -> Self {
        if let Some(foreign) = &mut self.column.foreign {
            foreign.cascade = true;
        }
        self
    }
}

impl Blueprint {
    pub(crate) fn new(table: &str) -> Self {
        Self {
            table: table.to_owned(),
            columns: Vec::new(),
            drops: Vec::new(),
        }
    }

    fn add(&mut self, name: &str, kind: Kind) -> ColumnBuilder<'_> {
        self.columns.push(Column {
            name: name.to_owned(),
            kind,
            nullable: false,
            unique: false,
            index: false,
            default: None,
            foreign: None,
        });
        let last = self.columns.len() - 1;
        // `last` is in bounds: an element was just pushed.
        #[allow(clippy::indexing_slicing)]
        ColumnBuilder {
            column: &mut self.columns[last],
        }
    }

    /// `id`: a 64-bit auto-increment primary key (a Rust `i64`).
    pub fn id(&mut self) -> ColumnBuilder<'_> {
        self.add("id", Kind::Id)
    }

    /// A `varchar(255)` column (`String`).
    pub fn string(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::String(255))
    }

    /// A `varchar(len)` column (`String`).
    pub fn string_len(&mut self, name: &str, len: u32) -> ColumnBuilder<'_> {
        self.add(name, Kind::String(len))
    }

    /// A `text` column (`String`).
    pub fn text(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::Text)
    }

    /// A 32-bit integer column (`i32`).
    pub fn integer(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::Integer)
    }

    /// A 64-bit integer column (`i64`).
    pub fn big_integer(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::BigInteger)
    }

    /// A boolean column (`bool`).
    pub fn boolean(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::Boolean)
    }

    /// A 32-bit float column (`f32`).
    pub fn float(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::Float)
    }

    /// A 64-bit float column (`f64`).
    pub fn double(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::Double)
    }

    /// A fixed-point `decimal(precision, scale)` column.
    pub fn decimal(&mut self, name: &str, precision: u32, scale: u32) -> ColumnBuilder<'_> {
        self.add(name, Kind::Decimal(precision, scale))
    }

    /// A date column (`Date`, chrono's `NaiveDate`).
    pub fn date(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::Date)
    }

    /// A timestamp column, with time zone where the backend has one (`DateTimeUtc`).
    pub fn datetime(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::DateTime)
    }

    /// A JSON column (`Json`, a `serde_json::Value`).
    pub fn json(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::Json)
    }

    /// A UUID column (`Uuid`).
    pub fn uuid(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::Uuid)
    }

    /// A 64-bit integer column meant to point at another table's `id`; add
    /// [`constrained`](ColumnBuilder::constrained) for the foreign key.
    pub fn foreign_id(&mut self, name: &str) -> ColumnBuilder<'_> {
        self.add(name, Kind::BigInteger)
    }

    /// Nullable `created_at` and `updated_at` timestamps, which
    /// [`Record`](crate::db::Record) fills in.
    pub fn timestamps(&mut self) {
        self.datetime("created_at").nullable();
        self.datetime("updated_at").nullable();
    }

    /// Drop the column `name` (in [`Schema::table`]; `down` of a migration that added it), after the columns this
    /// blueprint adds. Its indexes made by `.index()` / `.unique()` on an added column (`<table>_<column>_index`,
    /// `<table>_<column>_unique`) go first; PostgreSQL and MySQL also drop any other index of the column with it.
    /// SQLite (3.35 or later; Smeltery's bundled SQLite is newer) refuses a column that is a primary key, `UNIQUE`
    /// in its `CREATE TABLE`, part of a foreign key or used by an index, view or trigger the framework did not name;
    /// MySQL refuses a column of a foreign key.
    ///
    /// ```
    /// # async fn demo(schema: &smeltery_core::db::migration::Schema) -> smeltery_core::Result<()> {
    /// schema.table("users", |t| {
    ///     t.drop_column("two_factor_secret");
    ///     t.drop_column("two_factor_confirmed_at");
    /// }).await
    /// # }
    /// ```
    pub fn drop_column(&mut self, name: &str) {
        self.drops.push(name.to_owned());
    }

    /// The `CREATE TABLE` statement and its `CREATE INDEX` statements.
    pub(crate) fn create_sql(&self, backend: Backend, if_not_exists: bool) -> Result<Vec<String>> {
        if let Some(column) = self.drops.first() {
            return Err(Error::internal(format!(
                "`drop_column(\"{column}\")` belongs in `schema.table(…)`, not in `create`"
            )));
        }
        let mut create = Table::create();
        create.table(self.table.clone());
        if if_not_exists {
            create.if_not_exists();
        }
        for column in &self.columns {
            create.col(column_def(column, backend, true));
            if let Some(foreign) = &column.foreign {
                create.foreign_key(&mut foreign_key(&self.table, &column.name, foreign));
            }
        }
        if self.columns.is_empty() {
            return Err(Error::internal(format!(
                "table `{}` has no columns",
                self.table
            )));
        }
        let mut out = vec![build_create(backend, &create)];
        out.extend(self.index_sql(backend, false, if_not_exists));
        Ok(out)
    }

    /// `ALTER TABLE … ADD COLUMN` per column, then indexes and foreign keys.
    pub(crate) fn alter_sql(&self, backend: Backend) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for column in &self.columns {
            if column.kind == Kind::Id {
                return Err(Error::internal(format!(
                    "cannot add an `id` primary key to the existing table `{}`",
                    self.table
                )));
            }
            let mut def = column_def(column, backend, false);
            // SQLite cannot add a constraint to an existing table: the reference goes on
            // the column itself.
            if backend == Backend::Sqlite
                && let Some(foreign) = &column.foreign
            {
                def.extra(sqlite_references(foreign));
            }
            let mut alter = Table::alter();
            alter.table(self.table.clone()).add_column(def);
            out.push(build_alter(backend, &alter));
        }
        out.extend(self.index_sql(backend, true, false));
        if backend != Backend::Sqlite {
            for column in &self.columns {
                if let Some(foreign) = &column.foreign {
                    let fk = foreign_key(&self.table, &column.name, foreign);
                    out.push(match backend {
                        Backend::Postgres => fk.to_string(PostgresQueryBuilder),
                        _ => fk.to_string(MysqlQueryBuilder),
                    });
                }
            }
        }
        for column in &self.drops {
            if backend == Backend::Sqlite {
                // SQLite refuses to drop an indexed column: the framework's own indexes of it go first.
                for suffix in ["index", "unique"] {
                    let mut drop = Index::drop();
                    drop.name(format!("{}_{column}_{suffix}", self.table))
                        .if_exists();
                    out.push(drop.to_string(SqliteQueryBuilder));
                }
            }
            let mut alter = Table::alter();
            alter.table(self.table.clone()).drop_column(column.clone());
            out.push(build_alter(backend, &alter));
        }
        Ok(out)
    }

    /// `CREATE INDEX` for `.index()` columns; with `unique_as_index`, `CREATE UNIQUE
    /// INDEX` for `.unique()` columns too (an added column cannot carry `UNIQUE` on SQLite).
    fn index_sql(
        &self,
        backend: Backend,
        unique_as_index: bool,
        if_not_exists: bool,
    ) -> Vec<String> {
        let mut out = Vec::new();
        for column in &self.columns {
            let mut kinds = Vec::new();
            if column.unique && unique_as_index {
                kinds.push(true);
            }
            if column.index {
                kinds.push(false);
            }
            for unique in kinds {
                let suffix = if unique { "unique" } else { "index" };
                let mut index = Index::create();
                index
                    .name(format!("{}_{}_{suffix}", self.table, column.name))
                    .table(self.table.clone())
                    .col(column.name.clone());
                if unique {
                    index.unique();
                }
                if if_not_exists {
                    index.if_not_exists();
                }
                out.push(match backend {
                    Backend::Sqlite => index.to_string(SqliteQueryBuilder),
                    Backend::Postgres => index.to_string(PostgresQueryBuilder),
                    Backend::MySql => index.to_string(MysqlQueryBuilder),
                });
            }
        }
        out
    }
}

/// `CONSTRAINT <table>_<column>_foreign FOREIGN KEY (column) REFERENCES table (id)`, with
/// `ON DELETE CASCADE` when asked (otherwise the backend's default, `NO ACTION`).
fn foreign_key(table: &str, column: &str, foreign: &Foreign) -> ForeignKeyCreateStatement {
    let mut fk = ForeignKey::create();
    fk.name(format!("{table}_{column}_foreign"))
        .from(table.to_owned(), column.to_owned())
        .to(foreign.table.clone(), foreign.column.clone());
    if foreign.cascade {
        fk.on_delete(ForeignKeyAction::Cascade);
    }
    fk
}

fn sqlite_references(foreign: &Foreign) -> String {
    let mut sql = format!(
        "REFERENCES \"{}\" (\"{}\")",
        foreign.table.replace('"', "\"\""),
        foreign.column.replace('"', "\"\"")
    );
    if foreign.cascade {
        sql.push_str(" ON DELETE CASCADE");
    }
    sql
}

fn column_def(column: &Column, backend: Backend, creating: bool) -> ColumnDef {
    let mut def = ColumnDef::new(column.name.clone());
    match column.kind {
        Kind::Id => {
            // SQLite only auto-increments an `integer` primary key (the rowid alias).
            if backend == Backend::Sqlite {
                def.integer();
            } else {
                def.big_integer();
            }
            def.not_null().auto_increment().primary_key();
            return def;
        }
        Kind::String(len) => def.string_len(len),
        Kind::Text => def.text(),
        Kind::Integer => def.integer(),
        Kind::BigInteger => def.big_integer(),
        Kind::Boolean => def.boolean(),
        Kind::Float => def.float(),
        Kind::Double => def.double(),
        Kind::Decimal(p, s) => def.decimal_len(p, s),
        Kind::Date => def.date(),
        Kind::DateTime => def.timestamp_with_time_zone(),
        Kind::Json => match backend {
            // `jsonb` indexes and compares; it is what Postgres apps usually want.
            Backend::Postgres => def.json_binary(),
            _ => def.json(),
        },
        Kind::Uuid => def.uuid(),
    };
    if column.nullable {
        def.null();
    } else {
        def.not_null();
    }
    if column.unique && creating {
        def.unique_key();
    }
    if let Some(value) = &column.default {
        def.default(value.clone());
    }
    def
}

fn build_create(backend: Backend, stmt: &TableCreateStatement) -> String {
    match backend {
        Backend::Sqlite => stmt.to_string(SqliteQueryBuilder),
        Backend::Postgres => stmt.to_string(PostgresQueryBuilder),
        Backend::MySql => stmt.to_string(MysqlQueryBuilder),
    }
}

fn build_alter(backend: Backend, stmt: &TableAlterStatement) -> String {
    match backend {
        Backend::Sqlite => stmt.to_string(SqliteQueryBuilder),
        Backend::Postgres => stmt.to_string(PostgresQueryBuilder),
        Backend::MySql => stmt.to_string(MysqlQueryBuilder),
    }
}

fn build(backend: Backend, stmt: &mut sea_orm::sea_query::TableRenameStatement) -> String {
    match backend {
        Backend::Sqlite => stmt.to_string(SqliteQueryBuilder),
        Backend::Postgres => stmt.to_string(PostgresQueryBuilder),
        Backend::MySql => stmt.to_string(MysqlQueryBuilder),
    }
}

/// `DROP TABLE [IF EXISTS] t`; `cascade` adds `CASCADE` on PostgreSQL (used by
/// `migrate:fresh`, which drops every table whatever points at it).
pub(crate) fn drop_table_sql(
    backend: Backend,
    table: &str,
    if_exists: bool,
    cascade: bool,
) -> String {
    let mut drop = Table::drop();
    drop.table(table.to_owned());
    if if_exists {
        drop.if_exists();
    }
    if cascade && backend == Backend::Postgres {
        drop.cascade();
    }
    match backend {
        Backend::Sqlite => drop.to_string(SqliteQueryBuilder),
        Backend::Postgres => drop.to_string(PostgresQueryBuilder),
        Backend::MySql => drop.to_string(MysqlQueryBuilder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posts() -> Blueprint {
        let mut t = Blueprint::new("posts");
        t.id();
        t.foreign_id("user_id")
            .constrained("users")
            .cascade_on_delete();
        t.string("title").unique();
        t.string_len("slug", 80).index();
        t.text("body").nullable();
        t.boolean("published").default(false);
        t.integer("views").default(0);
        t.timestamps();
        t
    }

    fn additions() -> Blueprint {
        let mut t = Blueprint::new("posts");
        t.foreign_id("editor_id").nullable().constrained("users");
        t.string("code").unique();
        t.date("published_on").nullable().index();
        t
    }

    #[test]
    fn create_table_sql_for_every_backend() {
        assert_eq!(
            posts().create_sql(Backend::Sqlite, false).unwrap(),
            [
                r#"CREATE TABLE "posts" ( "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT, "user_id" integer NOT NULL, "title" varchar(255) NOT NULL UNIQUE, "slug" varchar(80) NOT NULL, "body" text NULL, "published" boolean NOT NULL DEFAULT FALSE, "views" integer NOT NULL DEFAULT 0, "created_at" timestamp_with_timezone_text NULL, "updated_at" timestamp_with_timezone_text NULL, FOREIGN KEY ("user_id") REFERENCES "users" ("id") ON DELETE CASCADE )"#,
                r#"CREATE INDEX "posts_slug_index" ON "posts" ("slug")"#,
            ]
        );
        assert_eq!(
            posts().create_sql(Backend::Postgres, false).unwrap(),
            [
                r#"CREATE TABLE "posts" ( "id" bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL PRIMARY KEY, "user_id" bigint NOT NULL, "title" varchar(255) NOT NULL UNIQUE, "slug" varchar(80) NOT NULL, "body" text NULL, "published" bool NOT NULL DEFAULT FALSE, "views" integer NOT NULL DEFAULT 0, "created_at" timestamp with time zone NULL, "updated_at" timestamp with time zone NULL, CONSTRAINT "posts_user_id_foreign" FOREIGN KEY ("user_id") REFERENCES "users" ("id") ON DELETE CASCADE )"#,
                r#"CREATE INDEX "posts_slug_index" ON "posts" ("slug")"#,
            ]
        );
        assert_eq!(
            posts().create_sql(Backend::MySql, false).unwrap(),
            [
                "CREATE TABLE `posts` ( `id` bigint NOT NULL PRIMARY KEY AUTO_INCREMENT, `user_id` bigint NOT NULL, `title` varchar(255) NOT NULL UNIQUE, `slug` varchar(80) NOT NULL, `body` text NULL, `published` bool NOT NULL DEFAULT FALSE, `views` int NOT NULL DEFAULT 0, `created_at` timestamp NULL, `updated_at` timestamp NULL, CONSTRAINT `posts_user_id_foreign` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`) ON DELETE CASCADE )",
                "CREATE INDEX `posts_slug_index` ON `posts` (`slug`)",
            ]
        );
    }

    #[test]
    fn column_types_for_every_backend() {
        let mut t = Blueprint::new("types");
        t.big_integer("a");
        t.float("b");
        t.double("c");
        t.decimal("d", 10, 2);
        t.date("e");
        t.datetime("f");
        t.json("g");
        t.uuid("h");
        t.string("i").default("draft");
        assert_eq!(
            t.create_sql(Backend::Sqlite, true).unwrap(),
            [
                r#"CREATE TABLE IF NOT EXISTS "types" ( "a" integer NOT NULL, "b" float NOT NULL, "c" double NOT NULL, "d" real(10, 2) NOT NULL, "e" date_text NOT NULL, "f" timestamp_with_timezone_text NOT NULL, "g" json_text NOT NULL, "h" uuid_text NOT NULL, "i" varchar(255) NOT NULL DEFAULT 'draft' )"#
            ]
        );
        assert_eq!(
            t.create_sql(Backend::Postgres, true).unwrap(),
            [
                r#"CREATE TABLE IF NOT EXISTS "types" ( "a" bigint NOT NULL, "b" real NOT NULL, "c" double precision NOT NULL, "d" decimal(10, 2) NOT NULL, "e" date NOT NULL, "f" timestamp with time zone NOT NULL, "g" jsonb NOT NULL, "h" uuid NOT NULL, "i" varchar(255) NOT NULL DEFAULT 'draft' )"#
            ]
        );
        assert_eq!(
            t.create_sql(Backend::MySql, true).unwrap(),
            [
                "CREATE TABLE IF NOT EXISTS `types` ( `a` bigint NOT NULL, `b` float NOT NULL, `c` double NOT NULL, `d` decimal(10, 2) NOT NULL, `e` date NOT NULL, `f` timestamp NOT NULL, `g` json NOT NULL, `h` binary(16) NOT NULL, `i` varchar(255) NOT NULL DEFAULT 'draft' )"
            ]
        );
    }

    #[test]
    fn alter_table_sql_for_every_backend() {
        assert_eq!(
            additions().alter_sql(Backend::Sqlite).unwrap(),
            [
                r#"ALTER TABLE "posts" ADD COLUMN "editor_id" integer NULL REFERENCES "users" ("id")"#,
                r#"ALTER TABLE "posts" ADD COLUMN "code" varchar(255) NOT NULL"#,
                r#"ALTER TABLE "posts" ADD COLUMN "published_on" date_text NULL"#,
                r#"CREATE UNIQUE INDEX "posts_code_unique" ON "posts" ("code")"#,
                r#"CREATE INDEX "posts_published_on_index" ON "posts" ("published_on")"#,
            ]
        );
        assert_eq!(
            additions().alter_sql(Backend::Postgres).unwrap(),
            [
                r#"ALTER TABLE "posts" ADD COLUMN "editor_id" bigint NULL"#,
                r#"ALTER TABLE "posts" ADD COLUMN "code" varchar(255) NOT NULL"#,
                r#"ALTER TABLE "posts" ADD COLUMN "published_on" date NULL"#,
                r#"CREATE UNIQUE INDEX "posts_code_unique" ON "posts" ("code")"#,
                r#"CREATE INDEX "posts_published_on_index" ON "posts" ("published_on")"#,
                r#"ALTER TABLE "posts" ADD CONSTRAINT "posts_editor_id_foreign" FOREIGN KEY ("editor_id") REFERENCES "users" ("id")"#,
            ]
        );
        assert_eq!(
            additions().alter_sql(Backend::MySql).unwrap(),
            [
                "ALTER TABLE `posts` ADD COLUMN `editor_id` bigint NULL",
                "ALTER TABLE `posts` ADD COLUMN `code` varchar(255) NOT NULL",
                "ALTER TABLE `posts` ADD COLUMN `published_on` date NULL",
                "CREATE UNIQUE INDEX `posts_code_unique` ON `posts` (`code`)",
                "CREATE INDEX `posts_published_on_index` ON `posts` (`published_on`)",
                "ALTER TABLE `posts` ADD CONSTRAINT `posts_editor_id_foreign` FOREIGN KEY (`editor_id`) REFERENCES `users` (`id`)",
            ]
        );
        let mut cascade = Blueprint::new("c");
        cascade
            .foreign_id("p_id")
            .nullable()
            .constrained("p")
            .cascade_on_delete();
        assert_eq!(
            cascade.alter_sql(Backend::Sqlite).unwrap()[0],
            r#"ALTER TABLE "c" ADD COLUMN "p_id" integer NULL REFERENCES "p" ("id") ON DELETE CASCADE"#
        );
    }

    #[test]
    fn invalid_blueprints_are_errors() {
        assert!(
            Blueprint::new("empty")
                .create_sql(Backend::Sqlite, false)
                .is_err()
        );
        let mut t = Blueprint::new("posts");
        t.id();
        assert!(t.alter_sql(Backend::Postgres).is_err());
    }

    #[test]
    fn drop_and_rename_sql() {
        assert_eq!(
            drop_table_sql(Backend::Sqlite, "posts", true, true),
            r#"DROP TABLE IF EXISTS "posts""#
        );
        assert_eq!(
            drop_table_sql(Backend::Postgres, "posts", true, true),
            r#"DROP TABLE IF EXISTS "posts" CASCADE"#
        );
        assert_eq!(
            drop_table_sql(Backend::Postgres, "posts", false, false),
            r#"DROP TABLE "posts""#
        );
        assert_eq!(
            drop_table_sql(Backend::MySql, "posts", true, true),
            "DROP TABLE IF EXISTS `posts`"
        );
        let rename = |b| build(b, Table::rename().table("a".to_owned(), "b".to_owned()));
        assert_eq!(rename(Backend::Sqlite), r#"ALTER TABLE "a" RENAME TO "b""#);
        assert_eq!(
            rename(Backend::Postgres),
            r#"ALTER TABLE "a" RENAME TO "b""#
        );
        assert_eq!(rename(Backend::MySql), "RENAME TABLE `a` TO `b`");
    }

    #[test]
    fn dropped_columns_lose_their_framework_indexes_first_on_sqlite() {
        let mut t = Blueprint::new("users");
        t.drop_column("two_factor_secret");
        assert_eq!(
            t.alter_sql(Backend::Sqlite).unwrap(),
            [
                r#"DROP INDEX IF EXISTS "users_two_factor_secret_index""#,
                r#"DROP INDEX IF EXISTS "users_two_factor_secret_unique""#,
                r#"ALTER TABLE "users" DROP COLUMN "two_factor_secret""#,
            ]
        );
        assert_eq!(
            t.alter_sql(Backend::Postgres).unwrap(),
            [r#"ALTER TABLE "users" DROP COLUMN "two_factor_secret""#]
        );
        assert_eq!(
            t.alter_sql(Backend::MySql).unwrap(),
            ["ALTER TABLE `users` DROP COLUMN `two_factor_secret`"]
        );
        assert!(
            t.create_sql(Backend::Sqlite, false).is_err(),
            "not in create"
        );
    }
}
