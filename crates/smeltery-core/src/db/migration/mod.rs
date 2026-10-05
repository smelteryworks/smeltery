//! Migrations: versioned schema changes, run in order and recorded in the `migrations`
//! table.
//!
//! ```
//! use smeltery_core::db::migration::{Migration, Migrator, Schema};
//! use smeltery_core::Result;
//!
//! /// Creates `posts`.
//! pub struct CreatePostsTable;
//!
//! impl Migration for CreatePostsTable {
//!     fn name(&self) -> &'static str {
//!         "2026_10_03_120000_create_posts_table"
//!     }
//!
//!     async fn up(&self, schema: &Schema) -> Result<()> {
//!         schema
//!             .create("posts", |t| {
//!                 t.id();
//!                 t.string("title");
//!                 t.text("body");
//!                 t.timestamps();
//!             })
//!             .await
//!     }
//!
//!     async fn down(&self, schema: &Schema) -> Result<()> {
//!         schema.drop_if_exists("posts").await
//!     }
//! }
//!
//! pub fn register(m: &mut Migrator) {
//!     m.add(CreatePostsTable);
//! }
//! # let mut m = Migrator::new();
//! # register(&mut m);
//! # assert_eq!(m.names(), ["2026_10_03_120000_create_posts_table"]);
//! ```

use std::future::Future;

use sea_orm::sea_query::{Expr, ExprTrait, Order, Query};
use sea_orm::{Statement, Value};

use crate::app::BoxFuture;
use crate::db::Db;
use crate::error::{Error, Result};

pub(crate) mod schema;

pub use schema::{Blueprint, ColumnBuilder, Schema};

/// The table that records which migrations ran.
pub const MIGRATIONS_TABLE: &str = "migrations";

/// One schema change. `up` applies it, `down` reverts it.
///
/// Each migration runs in its own transaction on SQLite and PostgreSQL, so a failing `up`
/// leaves neither its changes nor a record behind. MySQL commits DDL statements as they
/// run.
pub trait Migration: Send + Sync + 'static {
    /// The unique name, recorded in the `migrations` table: the file name without the `m`
    /// prefix, e.g. `2026_10_03_120000_create_posts_table`.
    fn name(&self) -> &'static str;

    /// Apply the change.
    fn up(&self, schema: &Schema) -> impl Future<Output = Result<()>> + Send;

    /// Revert the change.
    fn down(&self, schema: &Schema) -> impl Future<Output = Result<()>> + Send;
}

/// Object-safe form of [`Migration`].
trait ErasedMigration: Send + Sync {
    fn name(&self) -> &'static str;
    fn up<'a>(&'a self, schema: &'a Schema) -> BoxFuture<'a, Result<()>>;
    fn down<'a>(&'a self, schema: &'a Schema) -> BoxFuture<'a, Result<()>>;
}

impl<M: Migration> ErasedMigration for M {
    fn name(&self) -> &'static str {
        Migration::name(self)
    }

    fn up<'a>(&'a self, schema: &'a Schema) -> BoxFuture<'a, Result<()>> {
        Box::pin(Migration::up(self, schema))
    }

    fn down<'a>(&'a self, schema: &'a Schema) -> BoxFuture<'a, Result<()>> {
        Box::pin(Migration::down(self, schema))
    }
}

/// A migration's state, from [`Migrator::status`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MigrationStatus {
    /// The migration name.
    pub name: String,
    /// The batch it ran in, `None` while pending.
    pub batch: Option<i32>,
}

impl MigrationStatus {
    /// Whether the migration has run.
    pub fn ran(&self) -> bool {
        self.batch.is_some()
    }
}

/// A row of the `migrations` table.
#[derive(Clone, Debug)]
struct Ran {
    id: i64,
    name: String,
    batch: i32,
}

/// The registered migrations, oldest first, and the commands over them.
///
/// `database/migrations/mod.rs` fills it in its `register` function, which
/// [`AppBuilder::migrations`](crate::AppBuilder::migrations) calls.
#[derive(Default)]
pub struct Migrator {
    migrations: Vec<Box<dyn ErasedMigration>>,
}

impl std::fmt::Debug for Migrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migrator")
            .field("migrations", &self.names())
            .finish()
    }
}

impl Migrator {
    /// No migrations.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a migration (after the ones already added).
    pub fn add(&mut self, migration: impl Migration) -> &mut Self {
        self.migrations.push(Box::new(migration));
        self
    }

    /// The registered names, in order.
    pub fn names(&self) -> Vec<&'static str> {
        self.migrations.iter().map(|m| m.name()).collect()
    }

    fn get(&self, name: &str) -> Option<&dyn ErasedMigration> {
        self.migrations
            .iter()
            .find(|m| m.name() == name)
            .map(|m| m.as_ref())
    }

    /// Run every pending migration, in one new batch. Returns the names that ran.
    ///
    /// # Errors
    /// Two migrations share a name, or one fails (the ones before it stay applied).
    pub async fn migrate(&self, db: &Db) -> Result<Vec<String>> {
        self.check_names()?;
        ensure_table(db).await?;
        let ran = ran(db).await?;
        let batch = ran.iter().map(|r| r.batch).max().unwrap_or(0) + 1;
        let mut done = Vec::new();
        for migration in &self.migrations {
            if ran.iter().any(|r| r.name == migration.name()) {
                continue;
            }
            let schema = Schema::begin(db).await?;
            let result = match migration.up(&schema).await {
                Ok(()) => record(&schema, migration.name(), batch).await,
                Err(e) => Err(e),
            };
            finish(schema, result, migration.name(), "migrating").await?;
            tracing::info!(migration = migration.name(), batch, "migrated");
            done.push(migration.name().to_owned());
        }
        Ok(done)
    }

    /// Revert the last batch, or with `steps` the last `steps` migrations. Returns the names
    /// that were rolled back, newest first.
    ///
    /// # Errors
    /// A recorded migration is no longer registered, or a `down` fails.
    pub async fn rollback(&self, db: &Db, steps: Option<usize>) -> Result<Vec<String>> {
        ensure_table(db).await?;
        let mut ran = ran(db).await?;
        ran.sort_by_key(|r| std::cmp::Reverse(r.id));
        let targets: Vec<Ran> = match steps {
            Some(n) => ran.into_iter().take(n).collect(),
            None => {
                let last = ran.first().map(|r| r.batch);
                ran.into_iter().filter(|r| Some(r.batch) == last).collect()
            }
        };
        let mut done = Vec::new();
        for target in targets {
            let migration = self.get(&target.name).ok_or_else(|| {
                Error::internal(format!(
                    "migration `{}` is recorded in the `migrations` table but not registered",
                    target.name
                ))
            })?;
            let schema = Schema::begin(db).await?;
            let result = match migration.down(&schema).await {
                Ok(()) => forget(&schema, target.id).await,
                Err(e) => Err(e),
            };
            finish(schema, result, &target.name, "rolling back").await?;
            tracing::info!(migration = %target.name, "rolled back");
            done.push(target.name);
        }
        Ok(done)
    }

    /// Drop every table in the database (not only the migrated ones), then run every
    /// migration. Returns the names that ran.
    ///
    /// # Errors
    /// A drop or a migration fails.
    pub async fn fresh(&self, db: &Db) -> Result<Vec<String>> {
        self.check_names()?;
        crate::db::drop_all_tables(db).await?;
        self.migrate(db).await
    }

    /// Every registered migration with its batch (or pending), then any recorded migration
    /// that is no longer registered.
    ///
    /// # Errors
    /// The `migrations` table cannot be read.
    pub async fn status(&self, db: &Db) -> Result<Vec<MigrationStatus>> {
        ensure_table(db).await?;
        let ran = ran(db).await?;
        let mut out: Vec<MigrationStatus> = self
            .migrations
            .iter()
            .map(|m| MigrationStatus {
                name: m.name().to_owned(),
                batch: ran.iter().find(|r| r.name == m.name()).map(|r| r.batch),
            })
            .collect();
        for r in &ran {
            if self.get(&r.name).is_none() {
                out.push(MigrationStatus {
                    name: r.name.clone(),
                    batch: Some(r.batch),
                });
            }
        }
        Ok(out)
    }

    fn check_names(&self) -> Result<()> {
        let mut names = self.names();
        names.sort_unstable();
        if let Some(pair) = names.windows(2).find(|w| w.first() == w.get(1)) {
            return Err(Error::internal(format!(
                "two migrations are named `{}`",
                pair.first().copied().unwrap_or_default()
            )));
        }
        Ok(())
    }
}

/// Commit on success, roll back on failure (and say which migration failed).
async fn finish(schema: Schema, result: Result<()>, name: &str, doing: &str) -> Result<()> {
    match result {
        Ok(()) => schema.commit().await,
        Err(e) => {
            if let Err(rollback) = schema.rollback().await {
                tracing::error!(error = %rollback, "rollback failed");
            }
            Err(Error::internal(format!("{doing} `{name}` failed: {e}")))
        }
    }
}

/// Create the `migrations` table when it is missing.
async fn ensure_table(db: &Db) -> Result<()> {
    let mut t = Blueprint::new(MIGRATIONS_TABLE);
    t.id();
    t.string("name").unique();
    t.integer("batch");
    t.datetime("ran_at");
    let schema = Schema::new(db);
    if schema.has_table(MIGRATIONS_TABLE).await? {
        return Ok(());
    }
    for sql in t.create_sql(db.backend(), true)? {
        schema.raw(&sql).await?;
    }
    Ok(())
}

async fn ran(db: &Db) -> Result<Vec<Ran>> {
    let stmt = db.backend().sea().build(
        &Query::select()
            .columns(["id", "name", "batch"])
            .from(MIGRATIONS_TABLE)
            .order_by("id", Order::Asc)
            .to_owned(),
    );
    let schema = Schema::new(db);
    let rows = schema.query_raw(stmt).await?;
    rows.iter()
        .map(|row| {
            Ok(Ran {
                id: row.try_get("", "id")?,
                name: row.try_get("", "name")?,
                batch: row.try_get("", "batch")?,
            })
        })
        .collect()
}

async fn record(schema: &Schema, name: &str, batch: i32) -> Result<()> {
    let now = sea_orm::prelude::ChronoUtc::now();
    let mut insert = Query::insert();
    insert
        .into_table(MIGRATIONS_TABLE)
        .columns(["name", "batch", "ran_at"])
        .values([
            Value::from(name.to_owned()).into(),
            Value::from(batch).into(),
            Value::from(now).into(),
        ])
        .map_err(|e| Error::internal(e.to_string()))?;
    let stmt = schema.backend().sea().build(&insert);
    schema.exec_raw(stmt).await?;
    Ok(())
}

async fn forget(schema: &Schema, id: i64) -> Result<()> {
    let stmt: Statement = schema.backend().sea().build(
        &Query::delete()
            .from_table(MIGRATIONS_TABLE)
            .and_where(Expr::col("id").eq(id))
            .to_owned(),
    );
    schema.exec_raw(stmt).await?;
    Ok(())
}
