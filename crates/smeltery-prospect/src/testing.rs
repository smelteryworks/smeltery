//! Test helpers: switch an app to the recording memory engine and assert what was indexed.
//!
//! ```no_run
//! # mod post {
//! # use smeltery::db::prelude::*;
//! # #[sea_orm::model]
//! # #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
//! # #[sea_orm(table_name = "posts")]
//! # pub struct Model {
//! #     #[sea_orm(primary_key)]
//! #     pub id: i64,
//! #     pub title: String,
//! # }
//! # impl ActiveModelBehavior for ActiveModel {}
//! # impl smeltery::prospect::Searchable for Model {
//! #     fn index(i: &mut smeltery::prospect::IndexSpec) { i.text("title"); }
//! # }
//! # }
//! # use post::Model as Post;
//! use smeltery::db::prelude::*;
//! use smeltery::prospect::{ProspectExt as _, testing};
//! use smeltery::testing::TestApp;
//!
//! let app = TestApp::new(|b| b.prospect(|p| { p.model::<Post>(); }));
//! let fake = testing::fake(&app);
//! let db = app.db();
//! let post = app
//!     .block_on(Post::create(&db, post::ActiveModel { title: Set("Forge".into()), ..Default::default() }))
//!     .unwrap();
//! fake.assert_indexed::<Post>(post.id);
//! fake.assert_synced_times::<Post>(1);
//! ```

use std::sync::Arc;

use smeltery_core::testing::TestApp;

use crate::driver::memory::MemoryEngine;
use crate::spec::DocValue;
use crate::{Prospect, Searchable};

/// The memory engine of an app under test, with assertions.
pub struct FakeSearch {
    prospect: Prospect,
    engine: Arc<MemoryEngine>,
}

impl std::fmt::Debug for FakeSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeSearch").finish_non_exhaustive()
    }
}

/// Switch `app`'s Prospect to a fresh, recording memory engine: from now on `Record` writes of searchable models
/// are indexed in memory, and searches run against it (the records still come from the database).
///
/// # Panics
/// Prospect is not installed, or the app was not built under `APP_ENV=testing` (as `TestApp` builds) or with
/// `PROSPECT_DRIVER=memory`.
#[allow(clippy::panic)]
pub fn fake(app: &TestApp) -> FakeSearch {
    let prospect = Prospect::of(app.app()).unwrap_or_else(|e| panic!("{e}"));
    let engine = prospect
        .use_fresh_memory()
        .unwrap_or_else(|e| panic!("{e}"));
    FakeSearch { prospect, engine }
}

#[allow(clippy::panic)]
impl FakeSearch {
    fn index<M: Searchable>(&self) -> String {
        self.prospect
            .spec_of::<M>()
            .unwrap_or_else(|e| panic!("{e}"))
            .index
            .clone()
    }

    /// Whether the record `key` of `M` is in the index.
    pub fn is_indexed<M: Searchable>(&self, key: i64) -> bool {
        self.engine.document(&self.index::<M>(), key).is_some()
    }

    /// The indexed text of `column` of record `key`, if any.
    pub fn indexed_text<M: Searchable>(&self, key: i64, column: &str) -> Option<String> {
        self.engine
            .document(&self.index::<M>(), key)
            .and_then(|d| d.get(column).and_then(DocValue::text).map(str::to_owned))
    }

    /// The declared columns the index holds for record `key` (what an engine would receive), if indexed.
    pub fn indexed_columns<M: Searchable>(&self, key: i64) -> Option<Vec<String>> {
        self.engine
            .document(&self.index::<M>(), key)
            .map(|d| d.keys().cloned().collect())
    }

    /// The number of documents of `M`.
    pub fn count<M: Searchable>(&self) -> usize {
        self.engine.len(&self.index::<M>())
    }

    /// Panics unless record `key` of `M` is indexed.
    pub fn assert_indexed<M: Searchable>(&self, key: i64) {
        assert!(
            self.is_indexed::<M>(key),
            "record {key} of `{}` is not indexed",
            self.index::<M>()
        );
    }

    /// Panics if record `key` of `M` is indexed.
    pub fn assert_not_indexed<M: Searchable>(&self, key: i64) {
        assert!(
            !self.is_indexed::<M>(key),
            "record {key} of `{}` is indexed",
            self.index::<M>()
        );
    }

    /// Panics unless the engine received exactly `times` writes (indexed or removed) for `M`.
    pub fn assert_synced_times<M: Searchable>(&self, times: usize) {
        let index = self.index::<M>();
        let got = self
            .engine
            .writes()
            .iter()
            .filter(|w| w.index == index)
            .count();
        assert_eq!(got, times, "writes to the index `{index}`");
    }
}
