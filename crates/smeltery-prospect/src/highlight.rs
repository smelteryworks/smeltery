//! Highlights as text segments, never HTML.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::text::SearchText;

/// The marker a driver puts before a match (U+E000, a private-use character).
pub(crate) const START: char = '\u{E000}';
/// The marker a driver puts after a match (U+E001).
pub(crate) const END: char = '\u{E001}';

/// The characters of a value the Rust highlighter reads.
pub(crate) const RUST_HIGHLIGHT_CHARS: usize = 20_000;

/// How many words a highlight from the Rust highlighter shows around the first match of a long text.
const WINDOW_WORDS: usize = 30;

/// One piece of a highlighted text: plain text, or a match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Segment {
    /// The text, as it is in the record: escape it when you render it (Mold's `{{ }}` does, so do React and Vue
    /// text nodes).
    pub text: String,
    /// Whether it matched a search term.
    pub matched: bool,
}

/// A highlighted text: segments in order. It never holds markup; a template renders each segment escaped and wraps
/// the matched ones in `<mark>`. A Mold view gets the segments in a field of its struct (`h.segments().to_vec()`,
/// a `Vec<Segment>` named `title` here), since release builds compile field access:
///
/// ```text
/// @for(s in row.title)@if(s.matched)<mark>{{ s.text }}</mark>@else{{ s.text }}@endif{{-- --}}@endfor
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Highlight(Vec<Segment>);

impl Highlight {
    /// The segments.
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    /// The text without markers.
    pub fn plain(&self) -> String {
        self.0.iter().map(|s| s.text.as_str()).collect()
    }

    /// Whether any segment matched.
    pub fn has_match(&self) -> bool {
        self.0.iter().any(|s| s.matched)
    }

    fn push(&mut self, text: &str, matched: bool) {
        if text.is_empty() {
            return;
        }
        match self.0.last_mut() {
            Some(last) if last.matched == matched => last.text.push_str(text),
            _ => self.0.push(Segment {
                text: text.to_owned(),
                matched,
            }),
        }
    }

    /// Split a driver's output on the markers. Marker characters never stay in the text. A start marker while a
    /// match is open means the earlier one was the record's own (a stored U+E000): the text after it is plain, and
    /// the new one opens the match. An end marker outside a match is the record's own and dropped; an unclosed match
    /// is plain text. Nothing in a segment is markup. A stored pair cannot be told from a real match here, so the
    /// database driver highlights a value that holds a marker character in Rust ([`Highlight::of_text`]) instead.
    pub(crate) fn from_marked(marked: &str) -> Self {
        let mut out = Highlight::default();
        let mut buffer = String::new();
        let mut open = false;
        for c in marked.chars() {
            match c {
                START => {
                    out.push(&buffer, false);
                    buffer.clear();
                    open = true;
                }
                END if open => {
                    out.push(&buffer, true);
                    buffer.clear();
                    open = false;
                }
                END => {}
                _ => buffer.push(c),
            }
        }
        out.push(&buffer, false);
        out
    }

    /// Highlight `text` in Rust: words (runs of letters and digits, compared lower-cased) that match a term are
    /// matches. A text of more than 60 words shows a window of 30 words around the first match, with `…` around.
    pub(crate) fn of_text(text: &str, terms: &SearchText) -> Self {
        // A bounded amount of work per value, like PostgreSQL's headline; marker characters are never text.
        let text: String = text
            .chars()
            .filter(|c| *c != START && *c != END)
            .take(RUST_HIGHLIGHT_CHARS)
            .collect();
        let text = text.as_str();
        // Split into alternating word / gap pieces, keeping every character.
        let mut pieces: Vec<(String, bool)> = Vec::new();
        for c in text.chars() {
            let word = c.is_alphanumeric();
            match pieces.last_mut() {
                Some((piece, is_word)) if *is_word == word => piece.push(c),
                _ => pieces.push((c.to_string(), word)),
            }
        }
        let word_count = pieces.iter().filter(|(_, w)| *w).count();
        let mut range = 0..pieces.len();
        if word_count > 2 * WINDOW_WORDS {
            let first = pieces
                .iter()
                .position(|(p, w)| *w && terms.matches_any(&p.to_lowercase()))
                .unwrap_or(0);
            let start = first.saturating_sub(WINDOW_WORDS / 3 * 2);
            let end = (start + WINDOW_WORDS * 2).min(pieces.len());
            range = start..end;
        }
        let mut out = Highlight::default();
        if range.start > 0 {
            out.push("…", false);
        }
        let end = range.end;
        for (piece, is_word) in pieces.get(range).unwrap_or_default() {
            let matched = *is_word && terms.matches_any(&piece.to_lowercase());
            out.push(piece, matched);
        }
        if end < pieces.len() {
            out.push("…", false);
        }
        out
    }
}

/// The highlights of one hit, by column.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Highlights(BTreeMap<String, Highlight>);

impl Highlights {
    /// The highlight of `column`, when it was asked for and the record has a value there.
    pub fn get(&self, column: &str) -> Option<&Highlight> {
        self.0.get(column)
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The columns and their highlights.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Highlight)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub(crate) fn insert(&mut self, column: &str, highlight: Highlight) {
        self.0.insert(column.to_owned(), highlight);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str, matched: bool) -> Segment {
        Segment {
            text: text.to_owned(),
            matched,
        }
    }

    #[test]
    fn markers_split_into_segments() {
        let h = Highlight::from_marked("a \u{E000}rust\u{E001} forge");
        assert_eq!(
            h.segments(),
            [seg("a ", false), seg("rust", true), seg(" forge", false)]
        );
        assert_eq!(h.plain(), "a rust forge");
    }

    #[test]
    fn stray_markers_cannot_open_tags() {
        // A record holding markers and markup: markup stays text, markers never stay in the text, and a stored
        // start marker does not mark the words after it.
        let h = Highlight::from_marked("<b>\u{E001}x\u{E000}<script>\u{E000}y\u{E001}");
        assert_eq!(h.segments(), [seg("<b>x<script>", false), seg("y", true)]);
        let h = Highlight::from_marked("x\u{E000}y \u{E000}forge\u{E001} z");
        assert_eq!(
            h.segments(),
            [seg("xy ", false), seg("forge", true), seg(" z", false)]
        );
        let unclosed = Highlight::from_marked("a\u{E000}b");
        assert_eq!(unclosed.segments(), [seg("ab", false)]);
        let json = serde_json::to_string(&h).unwrap();
        assert!(
            !json.contains('\u{E000}') && !json.contains('\u{E001}'),
            "{json}"
        );
        let rust = Highlight::of_text("a\u{E000}forge", &SearchText::parse("forge", 200));
        assert_eq!(rust.segments(), [seg("aforge", false)]);
        let long = Highlight::of_text(&"forge ".repeat(10_000), &SearchText::parse("forge", 200));
        assert!(long.plain().chars().count() < 1_000);
    }

    #[test]
    fn rust_highlights_match_words_and_window_long_texts() {
        let terms = SearchText::parse("rust fo", 200);
        let h = Highlight::of_text("Rust, the <forge>!", &terms);
        assert_eq!(
            h.segments(),
            [
                seg("Rust", true),
                seg(", the <", false),
                seg("forge", true),
                seg(">!", false)
            ]
        );
        let long = format!("{} rust {}", "word ".repeat(100), "tail ".repeat(100));
        let h = Highlight::of_text(&long, &terms);
        assert!(h.plain().starts_with('…') && h.plain().ends_with('…'));
        assert!(h.has_match());
        assert!(h.plain().split_whitespace().count() <= 64);
    }
}
