//! [`Hit`]: one search result.

use serde::Serialize;

use crate::highlight::Highlights;

/// One search result: the model loaded from the database, its relevance score and its highlights.
///
/// It derefs to the model (`hit.title`), and serializes as the model's fields plus `_score` and `_highlights` (each
/// highlight a list of `{"text", "matched"}` segments), which is what a Mold template or an Alloy page reads.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Hit<M> {
    /// The record.
    #[serde(flatten)]
    pub model: M,
    /// How well it matched (higher is better); `None` without search terms. Scores compare hits of one search,
    /// not across searches or drivers.
    #[serde(rename = "_score")]
    pub score: Option<f64>,
    /// The highlighted text columns that were asked for with `highlight(…)`.
    #[serde(rename = "_highlights")]
    pub highlights: Highlights,
}

impl<M> Hit<M> {
    pub(crate) fn new(model: M, score: Option<f64>, highlights: Highlights) -> Self {
        Self {
            model,
            score,
            highlights,
        }
    }

    /// The record.
    pub fn into_model(self) -> M {
        self.model
    }
}

impl<M> std::ops::Deref for Hit<M> {
    type Target = M;

    fn deref(&self) -> &M {
        &self.model
    }
}
