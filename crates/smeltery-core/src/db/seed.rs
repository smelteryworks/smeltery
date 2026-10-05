//! Seeders: code that fills the database with data, run by `db:seed`.
//!
//! ```
//! use smeltery_core::db::Db;
//! use smeltery_core::db::seed::{Seeder, Seeders};
//! use smeltery_core::Result;
//!
//! pub struct DatabaseSeeder;
//!
//! impl Seeder for DatabaseSeeder {
//!     async fn run(&self, db: &Db) -> Result<()> {
//!         db.execute("INSERT INTO settings (name) VALUES ('site')").await?;
//!         Ok(())
//!     }
//! }
//!
//! pub fn register(s: &mut Seeders) {
//!     s.add(DatabaseSeeder);
//! }
//! # let mut s = Seeders::new();
//! # register(&mut s);
//! # assert_eq!(s.names(), ["DatabaseSeeder"]);
//! ```

use std::future::Future;

use crate::app::BoxFuture;
use crate::db::Db;
use crate::error::{Error, Result};

/// Fills the database with data.
pub trait Seeder: Send + Sync + 'static {
    /// Insert the data.
    fn run(&self, db: &Db) -> impl Future<Output = Result<()>> + Send;
}

trait ErasedSeeder: Send + Sync {
    fn run<'a>(&'a self, db: &'a Db) -> BoxFuture<'a, Result<()>>;
}

impl<S: Seeder> ErasedSeeder for S {
    fn run<'a>(&'a self, db: &'a Db) -> BoxFuture<'a, Result<()>> {
        Box::pin(Seeder::run(self, db))
    }
}

/// The registered seeders, in order. `database/seeders/mod.rs` fills it in its `register`
/// function, which [`AppBuilder::seeders`](crate::AppBuilder::seeders) calls.
#[derive(Default)]
pub struct Seeders {
    seeders: Vec<(&'static str, Box<dyn ErasedSeeder>)>,
}

impl std::fmt::Debug for Seeders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seeders")
            .field("seeders", &self.names())
            .finish()
    }
}

impl Seeders {
    /// No seeders.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a seeder; its name is the type's name (`DatabaseSeeder`).
    pub fn add<S: Seeder>(&mut self, seeder: S) -> &mut Self {
        let full = std::any::type_name::<S>();
        let name = full.rsplit("::").next().unwrap_or(full);
        self.seeders.push((name, Box::new(seeder)));
        self
    }

    /// The registered names, in order.
    pub fn names(&self) -> Vec<&'static str> {
        self.seeders.iter().map(|(name, _)| *name).collect()
    }

    /// Run every seeder in order, or only the one named `class`. Returns the names that ran.
    ///
    /// # Errors
    /// No seeder is named `class`, or a seeder fails (the ones before it keep their data).
    pub async fn run(&self, db: &Db, class: Option<&str>) -> Result<Vec<&'static str>> {
        let mut ran = Vec::new();
        for (name, seeder) in &self.seeders {
            if class.is_some_and(|c| c != *name) {
                continue;
            }
            seeder
                .run(db)
                .await
                .map_err(|e| Error::internal(format!("seeder `{name}` failed: {e}")))?;
            tracing::info!(seeder = name, "seeded");
            ran.push(*name);
        }
        if let Some(class) = class
            && ran.is_empty()
        {
            return Err(Error::internal(format!(
                "no seeder named `{class}` is registered (registered: {})",
                self.names().join(", ")
            )));
        }
        Ok(ran)
    }
}
