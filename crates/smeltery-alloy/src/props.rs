//! Props: the data a page sends to its React / Vue component, and how each one loads.

use std::future::Future;

use serde::Serialize;
use serde_json::Value;
use smeltery_core::{BoxFuture, Error, Result};

/// A prop computed only when the response includes it.
pub(crate) type Lazy = Box<dyn FnOnce() -> BoxFuture<'static, Result<Value>> + Send>;

/// Where a prop's value comes from.
pub(crate) enum Source {
    /// Serialized when the prop was added.
    Value(Value),
    /// Computed when included.
    Lazy(Lazy),
    /// The value could not be serialized (reported when the page is sent).
    Failed(String),
}

/// When a prop is sent (Inertia's prop evaluation rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Load {
    /// On full visits and on partial reloads that name it.
    Regular,
    /// Only on partial reloads that name it.
    Optional,
    /// Listed in `deferredProps` on full visits; sent on partial reloads that name it.
    Deferred,
    /// On every response, partial reloads included.
    Always,
}

/// How the client merges a prop into the one it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Merge {
    /// `mergeProps`: appended.
    Append,
    /// `prependProps`: prepended.
    Prepend,
    /// `deepMergeProps`: merged deeply.
    Deep,
}

pub(crate) struct Prop {
    pub(crate) key: String,
    pub(crate) source: Source,
    pub(crate) load: Load,
    pub(crate) group: String,
    pub(crate) merge: Option<Merge>,
    pub(crate) match_on: Vec<String>,
}

impl std::fmt::Debug for Prop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prop")
            .field("key", &self.key)
            .field("load", &self.load)
            .finish_non_exhaustive()
    }
}

fn serialize(value: impl Serialize) -> Source {
    match serde_json::to_value(value) {
        Ok(v) => Source::Value(v),
        Err(e) => Source::Failed(e.to_string()),
    }
}

fn lazy<F, Fut, T>(f: F) -> Source
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T>> + Send + 'static,
    T: Serialize,
{
    Source::Lazy(Box::new(move || {
        Box::pin(async move {
            let value = f().await?;
            serde_json::to_value(value).map_err(Error::other)
        })
    }))
}

/// Props in the order they were added; a key added again replaces the earlier prop.
///
/// The page builder ([`crate::render`]) has the same methods; `Props` is what the shared-props function
/// ([`crate::Alloy::share`]) returns.
///
/// ```
/// use smeltery::alloy::Props;
///
/// let props = Props::new()
///     .with("app", serde_json::json!({ "name": "Forge" }))
///     .optional("stats", || async { Ok(42) });
/// assert_eq!(props.keys().collect::<Vec<_>>(), ["app", "stats"]);
/// ```
#[derive(Debug, Default)]
pub struct Props {
    pub(crate) props: Vec<Prop>,
}

impl Props {
    /// No props.
    pub fn new() -> Self {
        Self::default()
    }

    /// The keys, in order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.props.iter().map(|p| p.key.as_str())
    }

    fn push(mut self, key: impl Into<String>, source: Source, load: Load) -> Self {
        let key = key.into();
        self.props.retain(|p| p.key != key);
        self.props.push(Prop {
            key,
            source,
            load,
            group: "default".to_owned(),
            merge: None,
            match_on: Vec::new(),
        });
        self
    }

    fn last(&mut self) -> Option<&mut Prop> {
        self.props.last_mut()
    }

    /// A prop serialized now (so borrowed data works): sent on full visits and on partial reloads that name it.
    #[must_use]
    pub fn with(self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.push(key, serialize(value), Load::Regular)
    }

    /// Like [`Props::with`], but computed by `f` only when the response includes it.
    #[must_use]
    pub fn with_lazy<F, Fut, T>(self, key: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize,
    {
        self.push(key, lazy(f), Load::Regular)
    }

    /// A prop never sent on a full visit, computed by `f` only for a partial reload that names it
    /// (`router.reload({ only: ['key'] })`).
    #[must_use]
    pub fn optional<F, Fut, T>(self, key: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize,
    {
        self.push(key, lazy(f), Load::Optional)
    }

    /// A deferred prop (group `default`): the full visit lists it in `deferredProps`, and the client loads it with a
    /// partial reload right after the page shows.
    #[must_use]
    pub fn defer<F, Fut, T>(self, key: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize,
    {
        self.defer_in("default", key, f)
    }

    /// A deferred prop in `group` (props of one group load in one request).
    #[must_use]
    pub fn defer_in<F, Fut, T>(self, group: impl Into<String>, key: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Serialize,
    {
        let mut props = self.push(key, lazy(f), Load::Deferred);
        if let Some(p) = props.last() {
            p.group = group.into();
        }
        props
    }

    fn merged(self, key: impl Into<String>, value: impl Serialize, merge: Merge) -> Self {
        let mut props = self.push(key, serialize(value), Load::Regular);
        if let Some(p) = props.last() {
            p.merge = Some(merge);
        }
        props
    }

    /// A prop the client appends to the one it holds (`mergeProps`), e.g. the next page of a list.
    #[must_use]
    pub fn merge(self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.merged(key, value, Merge::Append)
    }

    /// A prop the client prepends to the one it holds (`prependProps`).
    #[must_use]
    pub fn prepend(self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.merged(key, value, Merge::Prepend)
    }

    /// A prop the client merges deeply into the one it holds (`deepMergeProps`).
    #[must_use]
    pub fn deep_merge(self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.merged(key, value, Merge::Deep)
    }

    /// Merge items of prop `key` by `field` instead of appending duplicates (`matchPropsOn: ["key.field"]`).
    /// Does nothing when there is no prop `key`.
    #[must_use]
    pub fn match_on(mut self, key: &str, field: impl Into<String>) -> Self {
        if let Some(p) = self.props.iter_mut().find(|p| p.key == key) {
            p.match_on.push(field.into());
        }
        self
    }

    /// A prop sent on every response, even on partial reloads that do not name it.
    #[must_use]
    pub fn always(self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.push(key, serialize(value), Load::Always)
    }
}
