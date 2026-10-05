//! The `memory` engine: documents in process memory, written by model events, searched with the same
//! [`SearchText`] rules. Its hits are keys; the records are loaded from the database with the search's scope,
//! `only_when` and filters applied again in SQL, so a stale document never surfaces a record the database would not
//! return. It records every write so tests can assert what was indexed.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use sea_orm::sea_query::{Alias, Expr, ExprTrait};
use sea_orm::{EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use smeltery_core::Result;
use smeltery_core::db::{Db, Record};

use crate::highlight::{Highlight, Highlights};
use crate::hit::Hit;
use crate::search::{Direction, Filter, FilterOp, FilterValue, Plan};
use crate::spec::{DocValue, Document, Resolved, document};
use crate::text::{SearchText, words};
use crate::{Prospect, Searchable, lock};

/// One write the engine received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Write {
    pub(crate) index: String,
    pub(crate) key: i64,
    /// `true`: indexed (upsert); `false`: removed.
    pub(crate) upsert: bool,
}

#[derive(Default)]
struct State {
    indexes: HashMap<String, BTreeMap<i64, Document>>,
    writes: Vec<Write>,
}

/// The in-process engine.
#[derive(Default)]
pub(crate) struct MemoryEngine {
    state: Mutex<State>,
}

impl MemoryEngine {
    pub(crate) fn upsert(&self, index: &str, key: i64, document: Document) {
        let mut state = lock(&self.state);
        state
            .indexes
            .entry(index.to_owned())
            .or_default()
            .insert(key, document);
        state.writes.push(Write {
            index: index.to_owned(),
            key,
            upsert: true,
        });
    }

    pub(crate) fn delete(&self, index: &str, key: i64) {
        let mut state = lock(&self.state);
        if let Some(docs) = state.indexes.get_mut(index) {
            docs.remove(&key);
        }
        state.writes.push(Write {
            index: index.to_owned(),
            key,
            upsert: false,
        });
    }

    pub(crate) fn flush(&self, index: &str) {
        lock(&self.state).indexes.remove(index);
    }

    pub(crate) fn document(&self, index: &str, key: i64) -> Option<Document> {
        lock(&self.state)
            .indexes
            .get(index)
            .and_then(|d| d.get(&key))
            .cloned()
    }

    pub(crate) fn len(&self, index: &str) -> usize {
        lock(&self.state)
            .indexes
            .get(index)
            .map_or(0, BTreeMap::len)
    }

    pub(crate) fn writes(&self) -> Vec<Write> {
        lock(&self.state).writes.clone()
    }

    /// The matching keys, best first, with their scores (all of them; the caller pages).
    fn matches(
        &self,
        spec: &Resolved,
        plan_text: &SearchText,
        filters: &[Filter],
        order: Option<&(String, Direction)>,
    ) -> Vec<(i64, f64)> {
        let state = lock(&self.state);
        let Some(docs) = state.indexes.get(&spec.index) else {
            return Vec::new();
        };
        let mut out: Vec<(i64, f64, Option<DocValue>)> = Vec::new();
        for (key, doc) in docs {
            if !filters.iter().all(|f| filter_matches(doc, f)) {
                continue;
            }
            let score = match score(doc, spec, plan_text) {
                Some(s) => s,
                None => continue,
            };
            let sort = order.map(|(c, _)| doc.get(c).cloned().unwrap_or(DocValue::Null));
            out.push((*key, score, sort));
        }
        out.sort_by(|a, b| {
            let primary = match order {
                Some((_, direction)) => {
                    let o = a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal);
                    if *direction == Direction::Desc {
                        o.reverse()
                    } else {
                        o
                    }
                }
                None => b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal),
            };
            primary.then(b.0.cmp(&a.0))
        });
        out.into_iter().map(|(k, s, _)| (k, s)).collect()
    }
}

/// The deterministic score of a document: for every term, the weight of each text column it matches in. `None` when
/// a term matches nowhere (every term must match). Without terms every document scores 0.
fn score(doc: &Document, spec: &Resolved, text: &SearchText) -> Option<f64> {
    let mut total = 0.0;
    for i in 0..text.terms().len() {
        let mut found = false;
        for (column, weight) in &spec.texts {
            let value = doc.get(column).and_then(DocValue::text).unwrap_or("");
            if words(value).any(|w| text.term_matches(i, &w)) {
                found = true;
                total += weight.bm25();
            }
        }
        if !found {
            return None;
        }
    }
    Some(total)
}

fn doc_value(value: &FilterValue) -> DocValue {
    match value {
        FilterValue::Int(n) => DocValue::Int(*n),
        FilterValue::Bool(b) => DocValue::Bool(*b),
        FilterValue::Str(s) => DocValue::Str(s.clone()),
        FilterValue::DateTime(t) => DocValue::Time(t.to_rfc3339()),
    }
}

fn filter_matches(doc: &Document, filter: &Filter) -> bool {
    let value = doc.get(&filter.column).unwrap_or(&DocValue::Null);
    match &filter.op {
        FilterOp::Eq(v) => *value == doc_value(v),
        FilterOp::In(vs) => vs.iter().any(|v| *value == doc_value(v)),
        FilterOp::NotIn(vs) => !vs.iter().any(|v| *value == doc_value(v)),
        FilterOp::Between(a, b) => {
            let (a, b) = (doc_value(a), doc_value(b));
            matches!(a.partial_cmp(value), Some(o) if o.is_le())
                && matches!(value.partial_cmp(&b), Some(o) if o.is_le())
        }
    }
}

type EntityOf<M> = <M as Record>::Entity;

/// Search the memory engine, then load the page's records from the database with the scope, `only_when` and the
/// filters as SQL (a key whose row is gone or no longer matches is dropped; the order is the engine's).
pub(crate) async fn search<M: Searchable>(
    prospect: &Prospect,
    db: &Db,
    spec: &Resolved,
    plan: &Plan<EntityOf<M>>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Hit<M>>, u64)> {
    let engine = prospect.memory();
    let matches = engine.matches(spec, &plan.text, &plan.filters, plan.order.as_ref());
    let total = u64::try_from(matches.len()).unwrap_or(u64::MAX);
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    let page: Vec<(i64, f64)> = matches.into_iter().skip(start).take(take).collect();
    if page.is_empty() {
        return Ok((Vec::new(), total));
    }
    let keys: Vec<i64> = page.iter().map(|(k, _)| *k).collect();
    let mut rows = hydrate::<M>(db, spec, &plan.filters, &keys).await?;
    let mut hits = Vec::with_capacity(page.len());
    for (key, score) in page {
        let Some(model) = rows.remove(&key) else {
            continue;
        };
        let mut highlights = Highlights::default();
        if !plan.text.is_empty() {
            let (_, doc) = document(&model, spec);
            for column in &plan.highlight {
                if let Some(value) = doc
                    .as_ref()
                    .and_then(|d| d.get(column))
                    .and_then(DocValue::text)
                {
                    highlights.insert(column, Highlight::of_text(value, &plan.text));
                }
            }
        }
        let score = (!plan.text.is_empty()).then_some(score);
        hits.push(Hit::new(model, score, highlights));
    }
    Ok((hits, total))
}

/// The rows of `keys` that still match the scope, `only_when` and the filters, by key.
async fn hydrate<M: Searchable>(
    db: &Db,
    spec: &Resolved,
    filters: &[Filter],
    keys: &[i64],
) -> Result<HashMap<i64, M>> {
    let table = spec.table;
    let mut select = EntityOf::<M>::find().filter(
        Expr::col((Alias::new(table), Alias::new(spec.key.as_str()))).is_in(keys.iter().copied()),
    );
    if let Some(flag) = &spec.only_when {
        select = select.filter(Expr::col((Alias::new(table), Alias::new(flag.as_str()))).eq(true));
    }
    for filter in filters {
        select = select.filter(super::database::condition::<M>(table, filter));
    }
    let rows: Vec<M> = select.all(db.conn()).await?;
    Ok(rows
        .into_iter()
        .map(|m| (document(&m, spec).0, m))
        .collect())
}

/// Index every row of `M` (by key, in chunks of `PROSPECT_BATCH`); the number indexed.
pub(crate) async fn import<M: Searchable>(
    prospect: &Prospect,
    db: &Db,
    spec: &Resolved,
) -> Result<u64> {
    let engine = prospect.memory();
    let batch = prospect.settings().batch();
    let key = Expr::col((Alias::new(spec.table), Alias::new(spec.key.as_str())));
    let mut after: Option<i64> = None;
    let mut indexed = 0;
    loop {
        let mut select = EntityOf::<M>::find()
            .order_by(key.clone(), sea_orm::Order::Asc)
            .limit(batch);
        if let Some(after) = after {
            select = select.filter(key.clone().gt(after));
        }
        let rows: Vec<M> = select.all(db.conn()).await?;
        if rows.is_empty() {
            return Ok(indexed);
        }
        for row in &rows {
            let (k, doc) = document(row, spec);
            after = Some(k);
            match doc {
                Some(doc) => {
                    engine.upsert(&spec.index, k, doc);
                    indexed += 1;
                }
                None => engine.delete(&spec.index, k),
            }
        }
    }
}

/// Re-read these keys: indexed when the row exists and `only_when` is true, removed otherwise.
pub(crate) async fn sync<M: Searchable>(
    prospect: &Prospect,
    db: &Db,
    spec: &Resolved,
    keys: &[i64],
) -> Result<()> {
    let engine = prospect.memory();
    for chunk in keys.chunks(500) {
        let key = Expr::col((Alias::new(spec.table), Alias::new(spec.key.as_str())));
        let rows: Vec<M> = EntityOf::<M>::find()
            .filter(key.is_in(chunk.iter().copied()))
            .all(db.conn())
            .await?;
        let mut found: HashMap<i64, M> = rows
            .into_iter()
            .map(|m| (document(&m, spec).0, m))
            .collect();
        for k in chunk {
            match found.remove(k).map(|m| document(&m, spec).1) {
                Some(Some(doc)) => engine.upsert(&spec.index, *k, doc),
                _ => engine.delete(&spec.index, *k),
            }
        }
    }
    Ok(())
}
