#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod console;
mod driver;
mod error;
mod highlight;
mod hit;
pub mod migration;
mod search;
mod settings;
mod spec;
pub mod testing;
mod text;

use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock, Weak};

use smeltery_core::db::{Db, ModelChange, ModelEvent, ModelListener, Record};
use smeltery_core::{App, AppBuilder, BoxFuture, Error, Result};

pub use error::ProspectError;
pub use highlight::{Highlight, Highlights, Segment};
pub use hit::Hit;
pub use search::{Direction, FilterValue, Search};
pub use settings::{Driver, ProspectSettings};
pub use spec::{IndexSpec, Language, TextColumn, Weight};
pub use text::{MAX_TERMS, MIN_PREFIX, MYSQL_STOPWORDS, SearchText};

use driver::memory::MemoryEngine;
use spec::Resolved;

/// A model that can be searched: implement it next to the model's entity (`app/models/post.rs`) and register the
/// model with `.prospect(|p| { p.model::<Post>(); })`.
///
/// The model needs one integer primary key (`t.id()`), and its search index needs a migration
/// ([`migration::SearchIndex`]).
pub trait Searchable:
    Record + Clone + sea_orm::ModelTrait<Entity = <Self as Record>::Entity> + sea_orm::FromQueryResult
{
    /// Declare the searched, filtered and sorted columns (see [`IndexSpec`]).
    fn index(i: &mut IndexSpec);

    /// A search of this model for `text` (parsed with [`SearchText::parse`]); refine it with the [`Search`] methods
    /// and run it with `paginate`, `get`, `keys` or `count`.
    fn search(prospect: &Prospect, text: &str) -> Search<Self> {
        Search::new(prospect, text)
    }
}

/// The registration of searchable models, inside [`ProspectExt::prospect`].
#[derive(Default)]
pub struct Models {
    entries: Vec<std::result::Result<(TypeId, Resolved), ProspectError>>,
}

impl std::fmt::Debug for Models {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Models")
            .field("count", &self.entries.len())
            .finish()
    }
}

impl Models {
    /// Make `M` searchable. Its [`IndexSpec`] is checked against its entity; a mistake stops the app's build with
    /// the table and the column.
    pub fn model<M: Searchable>(&mut self) -> &mut Self {
        self.entries
            .push(spec::resolve::<M>().map(|r| (TypeId::of::<M>(), r)));
        self
    }
}

/// Installs Prospect on an [`AppBuilder`] (`bootstrap/app.rs`).
pub trait ProspectExt: Sized {
    /// Install Prospect with the settings from `.env` ([`ProspectSettings::from_env`]) and the models `register`
    /// names: the [`Prospect`] service (a handler argument), and the console commands `prospect:status`,
    /// `prospect:import` and `prospect:flush`. The build fails on an invalid spec, a model registered twice, two
    /// models with one index name, or an unknown `PROSPECT_DRIVER`.
    fn prospect(self, register: impl FnOnce(&mut Models)) -> Self;

    /// Like [`prospect`](Self::prospect), with these settings.
    fn prospect_with(self, settings: ProspectSettings, register: impl FnOnce(&mut Models)) -> Self;
}

impl ProspectExt for AppBuilder {
    fn prospect(self, register: impl FnOnce(&mut Models)) -> Self {
        self.prospect_with(ProspectSettings::from_env(), register)
    }

    fn prospect_with(self, settings: ProspectSettings, register: impl FnOnce(&mut Models)) -> Self {
        let mut models = Models::default();
        register(&mut models);
        let mut problem: Option<ProspectError> = None;
        let driver = match settings.checked_driver() {
            Ok(driver) => driver,
            Err(e) => {
                problem = Some(e);
                Driver::Database
            }
        };
        let mut specs: HashMap<TypeId, Arc<Resolved>> = HashMap::new();
        let mut names: HashSet<String> = HashSet::new();
        for entry in models.entries {
            match entry {
                Err(e) => {
                    problem.get_or_insert(e);
                }
                Ok((type_id, resolved)) => {
                    if specs.contains_key(&type_id) || !names.insert(resolved.index.clone()) {
                        problem.get_or_insert(ProspectError::Spec {
                            table: resolved.table,
                            reason: format!(
                                "the index `{}` is registered twice (a model registered twice, or two models with one \
                                 index name)",
                                resolved.index
                            ),
                        });
                        continue;
                    }
                    specs.insert(type_id, Arc::new(resolved));
                }
            }
        }
        // The memory engine is written by model events. They are listened to only when that engine can be used: with
        // `PROSPECT_DRIVER=memory`, or under `APP_ENV=testing` (where `testing::fake` switches to it). The database
        // driver needs no events, so a production app with it registers no listener at all.
        let listens = driver == Driver::Memory || self.settings().env == "testing";
        let inner = Arc::new(Inner {
            settings,
            driver: RwLock::new(driver),
            memory: RwLock::new(Arc::new(MemoryEngine::default())),
            models: specs,
            db: OnceLock::new(),
            checked: Mutex::new(HashSet::new()),
            reported: Mutex::new(HashSet::new()),
            mysql_min_token: tokio::sync::OnceCell::new(),
            listens,
        });
        let prospect = Prospect {
            inner: Arc::clone(&inner),
        };
        let mut builder = self.service(prospect.clone());
        if listens {
            builder = builder.model_listener(SyncListener(Arc::downgrade(&inner)));
        }
        builder
            .commands(|c| {
                c.add(console::Status);
                c.add(console::Import);
                c.add(console::Flush);
            })
            .on_boot(move |app| async move {
                if let Some(e) = problem {
                    return Err(e.into());
                }
                if let Ok(db) = app.db() {
                    let _ = prospect.inner.db.set(db);
                }
                Ok(())
            })
            .on_serve(|app| async move {
                if let Ok(prospect) = Prospect::of(&app) {
                    prospect.report_missing_indexes().await;
                }
                Ok(())
            })
    }
}

/// The search service: a handler argument (`prospect: Prospect`), or [`Prospect::of`] elsewhere. Cheap to clone.
#[derive(Clone)]
pub struct Prospect {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Prospect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prospect")
            .field("driver", &self.driver())
            .field("models", &self.inner.models.len())
            .finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub(crate) settings: ProspectSettings,
    driver: RwLock<Driver>,
    memory: RwLock<Arc<MemoryEngine>>,
    pub(crate) models: HashMap<TypeId, Arc<Resolved>>,
    db: OnceLock<Db>,
    /// Models whose database index was found as their spec wants it.
    checked: Mutex<HashSet<TypeId>>,
    /// Models whose missing index was logged already.
    reported: Mutex<HashSet<TypeId>>,
    /// `innodb_ft_min_token_size`, read once.
    pub(crate) mysql_min_token: tokio::sync::OnceCell<usize>,
    listens: bool,
}

impl Prospect {
    /// The app's Prospect.
    ///
    /// # Errors
    /// Prospect is not installed (`.prospect(…)` in `bootstrap/app.rs`).
    pub fn of(app: &App) -> Result<Self> {
        app.service::<Prospect>()
            .map(|p| (*p).clone())
            .ok_or_else(|| {
                Error::internal(
                    "Prospect is not installed: add `.prospect(|p| { … })` in bootstrap/app.rs",
                )
            })
    }

    /// The driver that answers searches.
    pub fn driver(&self) -> Driver {
        *self
            .inner
            .driver
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The settings.
    pub fn settings(&self) -> &ProspectSettings {
        &self.inner.settings
    }

    /// Run `fut` with Prospect's model listener (and every other model listener) switched off for the writes it
    /// makes in this task; with an engine, call [`import`](Self::import) afterwards. The database driver keeps its
    /// index current inside the database, so it is not affected.
    pub async fn paused<F: std::future::Future>(&self, fut: F) -> F::Output {
        smeltery_core::db::without_listeners(fut).await
    }

    /// Fill the index of `M` from its table, returning the rows indexed. On SQLite the database driver rebuilds the
    /// FTS5 index from the table (`'rebuild'`); PostgreSQL and MySQL keep their index current themselves (0). The
    /// memory engine reads every row in chunks of `PROSPECT_BATCH`.
    ///
    /// # Errors
    /// `M` is not registered, there is no database, or a statement fails.
    pub async fn import<M: Searchable>(&self) -> Result<u64> {
        let spec = self.spec_of::<M>()?;
        let db = self.db()?;
        match self.driver() {
            Driver::Database => driver::database::rebuild(&db, &spec).await,
            Driver::Memory => driver::memory::import::<M>(self, &db, &spec).await,
        }
    }

    /// Bring the documents of these keys of `M` up to date: for writes that do not go through `Record` (raw SQL,
    /// SeaORM's `update_many`). The database driver needs nothing (its index follows the table); the memory engine
    /// re-reads each row (indexed when it exists and its `only_when` column is true, removed otherwise).
    ///
    /// # Errors
    /// `M` is not registered, there is no database, or a query fails.
    pub async fn sync<M: Searchable>(&self, keys: impl IntoIterator<Item = i64>) -> Result<()> {
        let spec = self.spec_of::<M>()?;
        if self.driver() == Driver::Database {
            return Ok(());
        }
        let db = self.db()?;
        let keys: Vec<i64> = keys.into_iter().collect();
        driver::memory::sync::<M>(self, &db, &spec, &keys).await
    }

    /// Remove every document of `M` from the engine. Refused by the database driver: its index follows the table.
    ///
    /// # Errors
    /// `M` is not registered, or the driver is `database`.
    pub async fn flush<M: Searchable>(&self) -> Result<()> {
        let spec = self.spec_of::<M>()?;
        self.flush_spec(&spec)
    }

    pub(crate) fn flush_spec(&self, spec: &Resolved) -> Result<()> {
        match self.driver() {
            Driver::Database => Err(ProspectError::Unsupported(
                "the database index follows the table: there is nothing to flush".to_owned(),
            )
            .into()),
            Driver::Memory => {
                self.memory().flush(&spec.index);
                Ok(())
            }
        }
    }

    pub(crate) fn spec_of<M: Searchable>(
        &self,
    ) -> std::result::Result<Arc<Resolved>, ProspectError> {
        self.inner
            .models
            .get(&TypeId::of::<M>())
            .cloned()
            .ok_or(ProspectError::NotRegistered)
    }

    pub(crate) fn db(&self) -> Result<Db> {
        self.inner
            .db
            .get()
            .cloned()
            .ok_or_else(|| Error::internal("Prospect needs a database: set DATABASE_URL in .env"))
    }

    pub(crate) fn memory(&self) -> Arc<MemoryEngine> {
        Arc::clone(
            &self
                .inner
                .memory
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Switch to a fresh memory engine (the testing fake).
    pub(crate) fn use_fresh_memory(&self) -> std::result::Result<Arc<MemoryEngine>, ProspectError> {
        if !self.inner.listens {
            return Err(ProspectError::Unsupported(
                "the memory engine needs APP_ENV=testing or PROSPECT_DRIVER=memory when the app builds".to_owned(),
            ));
        }
        let engine = Arc::new(MemoryEngine::default());
        *self
            .inner
            .memory
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::clone(&engine);
        *self
            .inner
            .driver
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Driver::Memory;
        Ok(engine)
    }

    /// Check that the database has `spec`'s index as the spec wants it; a passed check is remembered, a failure is
    /// logged once per model (ERROR) and answered as [`ProspectError::IndexMissing`].
    pub(crate) async fn ensure_index(
        &self,
        type_id: TypeId,
        spec: &Resolved,
        db: &Db,
    ) -> Result<()> {
        if lock(&self.inner.checked).contains(&type_id) {
            return Ok(());
        }
        match driver::database::check_index(self, db, spec).await? {
            None => {
                lock(&self.inner.checked).insert(type_id);
                Ok(())
            }
            Some(reason) => {
                if lock(&self.inner.reported).insert(type_id) {
                    tracing::error!(
                        table = spec.table,
                        reason = %reason,
                        "prospect: the search index is missing or does not match the spec; write a migration with \
                         SearchIndex::on(..) and run `migrate`"
                    );
                }
                Err(ProspectError::IndexMissing {
                    table: spec.table,
                    reason,
                }
                .into())
            }
        }
    }

    /// Every registered model's index state, by table name.
    pub(crate) async fn statuses(
        &self,
    ) -> Vec<(Arc<Resolved>, Result<Option<String>>, Option<u64>)> {
        let mut specs: Vec<Arc<Resolved>> = self.inner.models.values().cloned().collect();
        specs.sort_by(|a, b| a.table.cmp(b.table));
        let mut out = Vec::new();
        for spec in specs {
            let db = self.db();
            let (state, rows) = match &db {
                Ok(db) => (
                    driver::database::check_index(self, db, &spec).await,
                    driver::database::row_count(db, &spec).await.ok(),
                ),
                Err(_) => (Err(Error::internal("no database")), None),
            };
            out.push((spec, state, rows));
        }
        out
    }

    async fn report_missing_indexes(&self) {
        if self.driver() != Driver::Database {
            return;
        }
        for (spec, state, _) in self.statuses().await {
            match state {
                Ok(None) => {}
                Ok(Some(reason)) => tracing::error!(
                    table = spec.table,
                    reason = %reason,
                    "prospect: the search index is missing or does not match the spec; write a migration with \
                     SearchIndex::on(..) and run `migrate`"
                ),
                Err(e) => {
                    tracing::error!(table = spec.table, error = %e, "prospect: the search index could not be checked")
                }
            }
        }
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl axum::extract::FromRequestParts<App> for Prospect {
    type Rejection = Error;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        Prospect::of(app)
    }
}

/// Writes the memory engine from model events (registered only when that engine can be used).
struct SyncListener(Weak<Inner>);

impl ModelListener for SyncListener {
    fn changed<'a>(&'a self, _db: &'a Db, event: ModelEvent<'a>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(inner) = self.0.upgrade() else {
                return;
            };
            let Some(spec) = inner.models.get(&event.model_type) else {
                return;
            };
            let prospect = Prospect {
                inner: Arc::clone(&inner),
            };
            if prospect.driver() != Driver::Memory {
                return;
            }
            let Some((key, document)) = (spec.document_of)(event.row, spec) else {
                return;
            };
            let engine = prospect.memory();
            match (event.change, document) {
                (ModelChange::Deleted, _) | (_, None) => engine.delete(&spec.index, key),
                (_, Some(document)) => engine.upsert(&spec.index, key, document),
            }
        })
    }
}
