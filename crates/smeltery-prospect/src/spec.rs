//! [`IndexSpec`]: what a model declares in `impl Searchable`, and its checked form.

use std::any::Any;
use std::collections::BTreeMap;

use sea_orm::sea_query::ArrayType;
use sea_orm::{
    EntityName, EntityTrait, IdenStatic, Iterable, ModelTrait, PrimaryKeyToColumn, Value,
};

use crate::Searchable;
use crate::error::ProspectError;

/// A text column's weight: `A` ranks highest. SQLite uses them as `bm25` column weights (10, 5, 2, 1), PostgreSQL as
/// its four `setweight` classes; MySQL's relevance has no column weights.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Weight {
    /// The highest.
    A,
    /// The default.
    #[default]
    B,
    /// Lower.
    C,
    /// The lowest.
    D,
}

impl Weight {
    pub(crate) fn letter(self) -> char {
        match self {
            Self::A => 'A',
            Self::B => 'B',
            Self::C => 'C',
            Self::D => 'D',
        }
    }

    pub(crate) fn bm25(self) -> f64 {
        match self {
            Self::A => 10.0,
            Self::B => 5.0,
            Self::C => 2.0,
            Self::D => 1.0,
        }
    }
}

/// How words are compared. `Simple` (the default) compares words as they are (lower-cased, and on SQLite without
/// diacritics); `English` also reduces English words to their stem (`forging` finds `forge`) on SQLite (the
/// `porter` tokenizer) and PostgreSQL (the `english` configuration). MySQL has no stemming.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Language {
    /// No stemming.
    #[default]
    Simple,
    /// English stemming.
    English,
}

impl Language {
    /// The PostgreSQL text search configuration.
    pub(crate) fn pg_config(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::English => "english",
        }
    }

    /// The FTS5 tokenizer.
    pub(crate) fn fts5_tokenizer(self) -> &'static str {
        match self {
            Self::Simple => "unicode61 remove_diacritics 2",
            Self::English => "porter unicode61 remove_diacritics 2",
        }
    }
}

/// The value kinds a filter or sort column may have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Integer,
    Bool,
    String,
    DateTime,
    Other,
}

impl Kind {
    fn of(array: &ArrayType) -> Self {
        match array {
            ArrayType::TinyInt
            | ArrayType::SmallInt
            | ArrayType::Int
            | ArrayType::BigInt
            | ArrayType::TinyUnsigned
            | ArrayType::SmallUnsigned
            | ArrayType::Unsigned
            | ArrayType::BigUnsigned => Self::Integer,
            ArrayType::Bool => Self::Bool,
            ArrayType::String | ArrayType::Char => Self::String,
            ArrayType::ChronoDateTime
            | ArrayType::ChronoDateTimeUtc
            | ArrayType::ChronoDateTimeWithTimeZone
            | ArrayType::ChronoDateTimeLocal => Self::DateTime,
            _ => Self::Other,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Integer => "an integer",
            Self::Bool => "a bool",
            Self::String => "a string",
            Self::DateTime => "a date-time",
            Self::Other => "a supported type",
        }
    }
}

/// What a model makes searchable; filled in [`Searchable::index`].
///
/// ```
/// # mod post {
/// # use smeltery::db::prelude::*;
/// # #[sea_orm::model]
/// # #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
/// # #[sea_orm(table_name = "posts")]
/// # pub struct Model {
/// #     #[sea_orm(primary_key)]
/// #     pub id: i64,
/// #     pub title: String,
/// #     pub body: String,
/// #     pub user_id: i64,
/// #     pub team_id: i64,
/// #     pub published: bool,
/// #     pub created_at: Option<DateTimeUtc>,
/// # }
/// # impl ActiveModelBehavior for ActiveModel {}
/// use smeltery::prospect::{IndexSpec, Searchable, Weight};
///
/// impl Searchable for Model {
///     fn index(i: &mut IndexSpec) {
///         i.text("title").weight(Weight::A); // searched, ranked highest
///         i.text("body"); // Weight::B
///         i.filter("user_id"); // where_eq / where_in / … on it
///         i.sort("created_at"); // order_by on it
///         i.only_when("published"); // rows with false are not found
///         i.scoped_by("team_id"); // every search names a team
///     }
/// }
/// # }
/// # fn main() {}
/// ```
#[derive(Debug, Default)]
pub struct IndexSpec {
    texts: Vec<(String, Weight)>,
    filters: Vec<String>,
    sorts: Vec<String>,
    only_when: Option<String>,
    scope: Option<String>,
    name: Option<String>,
    language: Language,
}

/// A text column being declared: [`IndexSpec::text`] returns it.
#[derive(Debug)]
pub struct TextColumn<'a> {
    spec: &'a mut IndexSpec,
    at: usize,
}

impl TextColumn<'_> {
    /// Rank matches in this column with `weight` (default [`Weight::B`]).
    pub fn weight(self, weight: Weight) -> Self {
        if let Some(text) = self.spec.texts.get_mut(self.at) {
            text.1 = weight;
        }
        self
    }
}

impl IndexSpec {
    /// Search the string column `column`. The order of `text` calls is the order of the index's columns.
    pub fn text(&mut self, column: &str) -> TextColumn<'_> {
        self.texts.push((column.to_owned(), Weight::B));
        let at = self.texts.len() - 1;
        TextColumn { spec: self, at }
    }

    /// Allow `where_eq` / `where_in` / `where_not_in` / `where_between` on `column` (an integer, bool, string or
    /// date-time column).
    pub fn filter(&mut self, column: &str) -> &mut Self {
        self.filters.push(column.to_owned());
        self
    }

    /// Allow `order_by(column, …)` (an integer, bool, string or date-time column).
    pub fn sort(&mut self, column: &str) -> &mut Self {
        self.sorts.push(column.to_owned());
        self
    }

    /// Find only rows whose bool `column` is true.
    pub fn only_when(&mut self, column: &str) -> &mut Self {
        self.only_when = Some(column.to_owned());
        self
    }

    /// Every search must name a value of `column` with `within(value)`, or `across_scopes()` explicitly; a search
    /// with neither fails before any query. The column is also a filter column.
    pub fn scoped_by(&mut self, column: &str) -> &mut Self {
        self.scope = Some(column.to_owned());
        self
    }

    /// The index's name (default: the table's). The database driver's index is `<table>_search` either way.
    pub fn name(&mut self, name: &str) -> &mut Self {
        self.name = Some(name.to_owned());
        self
    }

    /// How words are compared (default [`Language::Simple`]); it must be the language of the index's migration.
    pub fn language(&mut self, language: Language) -> &mut Self {
        self.language = language;
        self
    }
}

/// A value of a model's column, as the memory engine keeps it.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub(crate) enum DocValue {
    Null,
    Int(i64),
    Bool(bool),
    Str(String),
    /// RFC 3339 in UTC, so text order is time order.
    Time(String),
}

impl DocValue {
    pub(crate) fn of(value: Value) -> Self {
        match value {
            Value::Bool(Some(b)) => Self::Bool(b),
            Value::TinyInt(Some(n)) => Self::Int(n.into()),
            Value::SmallInt(Some(n)) => Self::Int(n.into()),
            Value::Int(Some(n)) => Self::Int(n.into()),
            Value::BigInt(Some(n)) => Self::Int(n),
            Value::TinyUnsigned(Some(n)) => Self::Int(n.into()),
            Value::SmallUnsigned(Some(n)) => Self::Int(n.into()),
            Value::Unsigned(Some(n)) => Self::Int(n.into()),
            Value::BigUnsigned(Some(n)) => Self::Int(i64::try_from(n).unwrap_or(i64::MAX)),
            Value::String(Some(s)) => Self::Str(s),
            Value::Char(Some(c)) => Self::Str(c.to_string()),
            Value::ChronoDateTimeUtc(Some(t)) => Self::Time(t.to_rfc3339()),
            Value::ChronoDateTimeWithTimeZone(Some(t)) => {
                Self::Time(t.naive_utc().and_utc().to_rfc3339())
            }
            Value::ChronoDateTime(Some(t)) => Self::Time(t.and_utc().to_rfc3339()),
            Value::ChronoDateTimeLocal(Some(t)) => Self::Time(t.naive_utc().and_utc().to_rfc3339()),
            _ => Self::Null,
        }
    }

    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// The declared columns of one row (what an engine indexes).
pub(crate) type Document = BTreeMap<String, DocValue>;

/// Turns a row seen by a model listener into its key and document (`None`: `only_when` is false).
pub(crate) type DocumentOf =
    fn(&(dyn Any + Send + Sync), &Resolved) -> Option<(i64, Option<Document>)>;

/// A checked spec: every name is a column of the entity, with its kind.
#[derive(Debug)]
pub(crate) struct Resolved {
    pub(crate) table: &'static str,
    pub(crate) index: String,
    pub(crate) key: String,
    pub(crate) texts: Vec<(String, Weight)>,
    pub(crate) filters: Vec<(String, Kind)>,
    pub(crate) sorts: Vec<(String, Kind)>,
    pub(crate) only_when: Option<String>,
    pub(crate) scope: Option<String>,
    pub(crate) language: Language,
    pub(crate) document_of: DocumentOf,
}

impl Resolved {
    pub(crate) fn filter_kind(&self, column: &str) -> Option<Kind> {
        self.filters
            .iter()
            .find(|(c, _)| c == column)
            .map(|(_, k)| *k)
    }

    pub(crate) fn sort_kind(&self, column: &str) -> Option<Kind> {
        self.sorts
            .iter()
            .find(|(c, _)| c == column)
            .map(|(_, k)| *k)
    }

    pub(crate) fn is_text(&self, column: &str) -> bool {
        self.texts.iter().any(|(c, _)| c == column)
    }

    /// Every declared column (key, texts, filters, sorts, only_when), each once.
    pub(crate) fn declared(&self) -> Vec<&str> {
        let mut out: Vec<&str> = vec![self.key.as_str()];
        let all = self
            .texts
            .iter()
            .map(|(c, _)| c.as_str())
            .chain(self.filters.iter().map(|(c, _)| c.as_str()))
            .chain(self.sorts.iter().map(|(c, _)| c.as_str()))
            .chain(self.only_when.as_deref());
        for c in all {
            if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }
}

/// A name the drivers can quote without escaping: `[A-Za-z_][A-Za-z0-9_]*`, 1 to 48 characters (so
/// `<table>_search_index` fits PostgreSQL's 63).
pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 48
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The column of entity `E` named `name`.
pub(crate) fn column<E: EntityTrait>(name: &str) -> Option<E::Column> {
    E::Column::iter().find(|c| IdenStatic::as_str(c) == name)
}

/// Check `M`'s spec against its entity.
pub(crate) fn resolve<M: Searchable>() -> Result<Resolved, ProspectError> {
    type E<M> = <M as smeltery_core::db::Record>::Entity;
    let table = EntityName::table_name(&E::<M>::default());
    let fail = |reason: String| ProspectError::Spec { table, reason };
    let mut spec = IndexSpec::default();
    M::index(&mut spec);

    let keys: Vec<_> = <E<M> as EntityTrait>::PrimaryKey::iter().collect();
    let [key] = keys.as_slice() else {
        return Err(fail("the primary key must be one column".to_owned()));
    };
    let key_column = key.into_column();
    let key = IdenStatic::as_str(&key_column).to_owned();
    if Kind::of(&<M as ModelTrait>::get_value_type(key_column)) != Kind::Integer {
        return Err(fail(format!(
            "the primary key `{key}` must be an integer (the database index addresses rows by it)"
        )));
    }
    if !valid_name(table) || !valid_name(&key) {
        return Err(fail(
            "table and column names must be ASCII letters, digits and `_` (at most 48)".to_owned(),
        ));
    }
    let kind_of = |name: &str| -> Result<Kind, ProspectError> {
        if !valid_name(name) {
            return Err(fail(format!("`{name}` is not a valid column name")));
        }
        let col = column::<E<M>>(name)
            .ok_or_else(|| fail(format!("`{name}` is not a column of the model")))?;
        Ok(Kind::of(&<M as ModelTrait>::get_value_type(col)))
    };

    if spec.texts.is_empty() {
        return Err(fail("declare at least one `text` column".to_owned()));
    }
    let mut texts = Vec::new();
    for (name, weight) in &spec.texts {
        if kind_of(name)? != Kind::String {
            return Err(fail(format!(
                "the text column `{name}` must be a string column"
            )));
        }
        if texts.iter().any(|(c, _): &(String, Weight)| c == name) {
            return Err(fail(format!("`{name}` is declared as text twice")));
        }
        texts.push((name.clone(), *weight));
    }
    let mut filters: Vec<(String, Kind)> = Vec::new();
    for name in spec.filters.iter().chain(spec.scope.iter()) {
        let kind = kind_of(name)?;
        if kind == Kind::Other {
            return Err(fail(format!(
                "the filter column `{name}` must be an integer, bool, string or date-time column"
            )));
        }
        if !filters.iter().any(|(c, _)| c == name) {
            filters.push((name.clone(), kind));
        }
    }
    let mut sorts: Vec<(String, Kind)> = Vec::new();
    for name in &spec.sorts {
        let kind = kind_of(name)?;
        if kind == Kind::Other {
            return Err(fail(format!(
                "the sort column `{name}` must be an integer, bool, string or date-time column"
            )));
        }
        if !sorts.iter().any(|(c, _)| c == name) {
            sorts.push((name.clone(), kind));
        }
    }
    if let Some(name) = &spec.only_when
        && kind_of(name)? != Kind::Bool
    {
        return Err(fail(format!(
            "the only_when column `{name}` must be a bool column"
        )));
    }
    let index = spec.name.clone().unwrap_or_else(|| table.to_owned());
    if !valid_name(&index) {
        return Err(fail(format!("the index name `{index}` is invalid")));
    }
    Ok(Resolved {
        table,
        index,
        key,
        texts,
        filters,
        sorts,
        only_when: spec.only_when,
        scope: spec.scope,
        language: spec.language,
        document_of: document_of::<M>,
    })
}

/// The key and the declared columns of a row of `M` (the document an engine indexes); `None` for another model.
fn document_of<M: Searchable>(
    row: &(dyn Any + Send + Sync),
    spec: &Resolved,
) -> Option<(i64, Option<Document>)> {
    let model = row.downcast_ref::<M>()?;
    Some(document(model, spec))
}

/// The key of `model` and its document (`None` when `only_when` is false).
pub(crate) fn document<M: Searchable>(model: &M, spec: &Resolved) -> (i64, Option<Document>) {
    type E<M> = <M as smeltery_core::db::Record>::Entity;
    let value = |name: &str| {
        column::<E<M>>(name).map_or(DocValue::Null, |c| DocValue::of(ModelTrait::get(model, c)))
    };
    let key = match value(&spec.key) {
        DocValue::Int(k) => k,
        _ => 0,
    };
    if let Some(flag) = &spec.only_when
        && value(flag) != DocValue::Bool(true)
    {
        return (key, None);
    }
    let doc = spec
        .declared()
        .into_iter()
        .map(|name| (name.to_owned(), value(name)))
        .collect();
    (key, Some(doc))
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    #[test]
    fn names_are_plain_identifiers() {
        for good in ["posts", "_x", "a1", &"a".repeat(48)] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "",
            "1a",
            "a-b",
            "a\"b",
            "a b",
            "ä",
            &"a".repeat(49),
            "posts;--",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
