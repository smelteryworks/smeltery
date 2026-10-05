//! Model listeners: code told about every [`Record::create`](super::Record::create),
//! [`update`](super::Record::update) and [`delete`](super::Record::delete) after it succeeded.
//!
//! A listener is registered with [`AppBuilder::model_listener`](crate::AppBuilder::model_listener) and sees the
//! saved row. Listeners run after the statement returned (`Record` writes through the pool, never inside a
//! transaction of the caller), return nothing (they log their own errors) and never fail or undo the write. Writes
//! that do not go through `Record` (raw SQL, SeaORM's own `insert` / `update_many` / `delete_many`) are not seen.
//!
//! ```
//! use std::sync::atomic::{AtomicU64, Ordering};
//!
//! use smeltery_core::db::{Db, ModelChange, ModelEvent, ModelListener};
//! use smeltery_core::{AppBuilder, BoxFuture};
//!
//! #[derive(Default)]
//! struct CountDeletes(AtomicU64);
//!
//! impl ModelListener for CountDeletes {
//!     fn changed<'a>(&'a self, _db: &'a Db, event: ModelEvent<'a>) -> BoxFuture<'a, ()> {
//!         Box::pin(async move {
//!             if event.change == ModelChange::Deleted && event.table == "posts" {
//!                 self.0.fetch_add(1, Ordering::Relaxed);
//!             }
//!         })
//!     }
//! }
//!
//! fn build(app: AppBuilder) -> AppBuilder {
//!     app.model_listener(CountDeletes::default())
//! }
//! # let _ = build;
//! ```

use std::any::{Any, TypeId};
use std::future::Future;
use std::sync::Arc;

use super::Db;
use crate::app::BoxFuture;

/// What happened to a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ModelChange {
    /// [`Record::create`](super::Record::create) inserted it.
    Created,
    /// [`Record::update`](super::Record::update) changed it (an update that changed nothing writes nothing and is
    /// not reported).
    Updated,
    /// [`Record::delete`](super::Record::delete) deleted it.
    Deleted,
}

/// One successful write through [`Record`](super::Record), as a [`ModelListener`] sees it.
#[non_exhaustive]
pub struct ModelEvent<'a> {
    /// What happened.
    pub change: ModelChange,
    /// The model's table (`posts`).
    pub table: &'static str,
    /// The `TypeId` of the model type (`Post`), to find out cheaply whether the event concerns a model.
    pub model_type: TypeId,
    /// The row: as saved after a create or an update; after a delete, the row as the caller of
    /// [`delete`](super::Record::delete) held it. Read it with [`ModelEvent::model`].
    pub row: &'a (dyn Any + Send + Sync),
}

impl<'a> ModelEvent<'a> {
    /// An event about `row` of the model `M` (its `model_type` is `M`'s): what `Record` builds, and what a unit
    /// test of a listener passes to [`ModelListener::changed`].
    pub fn new<M: Any + Send + Sync>(change: ModelChange, table: &'static str, row: &'a M) -> Self {
        Self {
            change,
            table,
            model_type: TypeId::of::<M>(),
            row,
        }
    }

    /// The row as a `M`, when the event concerns the model `M`.
    pub fn model<M: Any>(&self) -> Option<&M> {
        self.row.downcast_ref::<M>()
    }

    /// Whether the event concerns the model `M`.
    pub fn is<M: Any>(&self) -> bool {
        self.model_type == TypeId::of::<M>()
    }
}

impl std::fmt::Debug for ModelEvent<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the row: it may hold personal data.
        f.debug_struct("ModelEvent")
            .field("change", &self.change)
            .field("table", &self.table)
            .finish_non_exhaustive()
    }
}

/// Code told about successful [`Record`](super::Record) writes; register it with
/// [`AppBuilder::model_listener`](crate::AppBuilder::model_listener).
///
/// `changed` runs after the write, in registration order, and `create` / `update` / `delete` wait for every
/// listener before they return, so it must be quick: hand slow work to a queue or to
/// [`App::spawn_owned`](crate::App::spawn_owned). It returns nothing: a listener logs its own errors, and the write it
/// is told about has happened either way.
///
/// The listeners run on a task the app owns: when the caller is cancelled while they run (a request timeout, a
/// closed connection), they still finish, and the app's shutdown waits for them like for its other owned work.
///
/// Writes a listener makes through `Record` are reported to the listeners too; wrap them in [`without_listeners`]
/// when they must not be (a listener writing to the table it listens to would loop). Such chains stop at
/// [`MAX_LISTENER_DEPTH`] levels: a deeper write is not reported, and an ERROR names its table.
pub trait ModelListener: Send + Sync + 'static {
    /// One write happened.
    fn changed<'a>(&'a self, db: &'a Db, event: ModelEvent<'a>) -> BoxFuture<'a, ()>;
}

/// The listeners of a [`Db`] and the app that owns their tasks (set once the app exists).
pub(crate) struct Listeners {
    list: Vec<Arc<dyn ModelListener>>,
    owner: std::sync::OnceLock<crate::app::WeakApp>,
}

impl Listeners {
    pub(crate) fn new(list: Vec<Arc<dyn ModelListener>>) -> Self {
        Self {
            list,
            owner: std::sync::OnceLock::new(),
        }
    }

    /// Run the listeners on `app`'s owned tasks from now on.
    pub(crate) fn set_owner(&self, app: crate::app::WeakApp) {
        let _ = self.owner.set(app);
    }
}

tokio::task_local! {
    static PAUSED: ();
    /// How many listener runs the current write is nested in (a write made by a listener is one level deeper).
    static DEPTH: u32;
}

/// How deep writes made by listeners (and the writes their listeners make, and so on) are still reported; a write
/// at this depth tells no listener, so a listener that writes what it listens to cannot loop forever.
pub const MAX_LISTENER_DEPTH: u32 = 8;

/// Run `fut` with model listeners switched off: `Record` writes inside it (in this task) tell no listener. Use it
/// for bulk work such as a seeder that creates many rows and then rebuilds what the listeners keep up to date in one
/// go.
///
/// ```no_run
/// # async fn demo(db: smeltery_core::db::Db) -> smeltery_core::Result<()> {
/// use smeltery_core::db::without_listeners;
///
/// without_listeners(async {
///     // Post::create(&db, …).await?; … no listener runs
///     Ok::<_, smeltery_core::Error>(())
/// })
/// .await?;
/// # Ok(())
/// # }
/// ```
///
/// Tasks spawned inside `fut` are not covered (the switch is task-local).
pub async fn without_listeners<F: Future>(fut: F) -> F::Output {
    PAUSED.scope((), fut).await
}

/// Whether the current task runs inside [`without_listeners`].
pub fn listeners_paused() -> bool {
    PAUSED.try_with(|()| ()).is_ok()
}

#[cfg(test)]
thread_local! {
    /// How often [`notify`] ran on this thread: the "no listener = no work" test reads it.
    pub(crate) static DISPATCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) static CUT_SHORT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Tell every listener of `db` about one write. Callers check [`Db::listeners`] first, so an app without listeners
/// never builds this future. With an owning app the listeners run on an owned task (a cancelled caller does not cut
/// them short); without one they run here, and a WARN reports a cancellation in the middle.
pub(crate) async fn notify<M: Any + Send + Sync + Clone>(
    db: &Db,
    listeners: &Arc<Listeners>,
    change: ModelChange,
    table: &'static str,
    row: &M,
) {
    #[cfg(test)]
    DISPATCHES.with(|d| d.set(d.get() + 1));
    // The switch is task-local: read it here, in the caller's task.
    if listeners_paused() {
        return;
    }
    let depth = DEPTH.try_with(|d| *d).unwrap_or(0);
    if depth >= MAX_LISTENER_DEPTH {
        tracing::error!(
            table,
            change = ?change,
            depth,
            "model listeners write in a chain this deep (a listener writes what it listens to?): this write is not \
             reported; use without_listeners for writes listeners must not hear"
        );
        return;
    }
    let depth = depth + 1;
    if let Some(app) = listeners.owner.get().and_then(crate::app::WeakApp::upgrade) {
        let (db, set, row) = (db.clone(), Arc::clone(listeners), row.clone());
        // The caller's span (the request's) goes along, so the listeners' log lines stay in it.
        let handle = app.tasks().spawn(tracing::Instrument::instrument(
            DEPTH.scope(depth, async move {
                run_all(&db, &set.list, change, table, &row).await;
            }),
            tracing::Span::current(),
        ));
        // Waiting keeps the order "write, listeners, return"; dropping the handle (a cancelled caller) leaves the
        // task running. Panics are caught inside, so the handle has nothing to report.
        let _ = handle.await;
        return;
    }
    let mut guard = CutShort {
        table,
        change,
        done: false,
    };
    DEPTH
        .scope(depth, run_all(db, &listeners.list, change, table, row))
        .await;
    guard.done = true;
}

/// Logs when an inline dispatch is dropped before every listener ran.
struct CutShort {
    table: &'static str,
    change: ModelChange,
    done: bool,
}

impl Drop for CutShort {
    fn drop(&mut self) {
        if !self.done {
            #[cfg(test)]
            CUT_SHORT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tracing::warn!(
                table = self.table,
                change = ?self.change,
                "the caller was cancelled while model listeners ran: some were not told about this write"
            );
        }
    }
}

async fn run_all<M: Any + Send + Sync>(
    db: &Db,
    list: &[Arc<dyn ModelListener>],
    change: ModelChange,
    table: &'static str,
    row: &M,
) {
    for listener in list {
        // The write has happened: a panicking listener is logged and never reaches the caller of
        // `create` / `update` / `delete`.
        if run_caught(listener.as_ref(), db, ModelEvent::new(change, table, row))
            .await
            .is_err()
        {
            tracing::error!(table, change = ?change, "a model listener panicked");
        }
    }
}

/// Run one listener, catching a panic while it builds or runs its future.
async fn run_caught(
    listener: &dyn ModelListener,
    db: &Db,
    event: ModelEvent<'_>,
) -> std::result::Result<(), ()> {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::task::Poll;
    let Ok(mut fut) = catch_unwind(AssertUnwindSafe(|| listener.changed(db, event))) else {
        return Err(());
    };
    std::future::poll_fn(
        |cx| match catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx))) {
            Ok(Poll::Ready(())) => Poll::Ready(Ok(())),
            Ok(Poll::Pending) => Poll::Pending,
            Err(_) => Poll::Ready(Err(())),
        },
    )
    .await
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::Record;

    mod note {
        use crate::db::prelude::*;

        #[sea_orm::model]
        #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
        #[sea_orm(table_name = "notes")]
        pub struct Model {
            #[sea_orm(primary_key)]
            pub id: i64,
            pub body: String,
        }

        impl ActiveModelBehavior for ActiveModel {}
    }

    struct Quiet;

    impl ModelListener for Quiet {
        fn changed<'a>(&'a self, _db: &'a Db, _event: ModelEvent<'a>) -> BoxFuture<'a, ()> {
            Box::pin(async {})
        }
    }

    #[allow(clippy::unwrap_used)]
    async fn writes(db: &Db) {
        use sea_orm::Set;
        let row = note::Model::create(
            db,
            note::ActiveModel {
                body: Set("a".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let row = row.update(db, |m| m.body = Set("b".into())).await.unwrap();
        row.delete(db).await.unwrap();
    }

    /// The hot path: without a registered listener, `Record` writes never reach the listener code at all (one
    /// `Option` check per write).
    #[tokio::test(flavor = "current_thread")]
    async fn no_listener_means_no_listener_work() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)")
            .await
            .unwrap();
        DISPATCHES.with(|d| d.set(0));
        writes(&db).await;
        assert_eq!(DISPATCHES.with(std::cell::Cell::get), 0);
        assert!(db.listeners().is_none());
        assert!(db.clone().with_listeners(Vec::new()).listeners().is_none());

        let db = db.with_listeners(vec![Arc::new(Quiet)]);
        writes(&db).await;
        assert_eq!(DISPATCHES.with(std::cell::Cell::get), 3);
    }

    /// Waits for its gate before it finishes.
    struct Gated(Arc<tokio::sync::Notify>, Arc<std::sync::atomic::AtomicU64>);

    impl ModelListener for Gated {
        fn changed<'a>(&'a self, _db: &'a Db, _event: ModelEvent<'a>) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                self.0.notified().await;
                self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        }
    }

    /// A `Db` outside an app runs its listeners in the caller: a cancelled caller cuts them short, and that is
    /// reported (WARN) instead of passing in silence.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::unwrap_used)]
    async fn a_cancelled_inline_dispatch_is_reported() {
        use sea_orm::Set;
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)")
            .await
            .unwrap();
        let gate = Arc::new(tokio::sync::Notify::new());
        let told = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let db = db.with_listeners(vec![Arc::new(Gated(gate.clone(), told.clone()))]);
        let before = CUT_SHORT.load(std::sync::atomic::Ordering::SeqCst);
        let write = note::Model::create(
            &db,
            note::ActiveModel {
                body: Set("a".into()),
                ..Default::default()
            },
        );
        // The listener waits for a gate that never opens while the caller waits: the caller gives up.
        let res = tokio::time::timeout(std::time::Duration::from_millis(50), write).await;
        assert!(res.is_err());
        assert_eq!(
            CUT_SHORT.load(std::sync::atomic::Ordering::SeqCst),
            before + 1
        );
        assert_eq!(told.load(std::sync::atomic::Ordering::SeqCst), 0);
        // The row was written all the same.
        assert_eq!(note::Model::count(&db).await.unwrap(), 1);
    }
}
