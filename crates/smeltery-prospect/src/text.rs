//! [`SearchText`]: the only way user text reaches a driver.

/// The most terms a search uses; the rest are dropped.
pub const MAX_TERMS: usize = 16;

/// The shortest last term that matches as a prefix (search-as-you-type).
pub const MIN_PREFIX: usize = 2;

/// Search text parsed into terms.
///
/// [`SearchText::parse`] trims the text, cuts it at `max_chars` characters, splits it into terms on everything that
/// is not a Unicode letter or digit (so no operator, quote, colon, asterisk, parenthesis or minus reaches a search
/// grammar), lower-cases them and keeps at most 16. Every term must match; the last one matches as a prefix when it
/// has at least two characters. No terms (an empty or punctuation-only text) means no text condition.
///
/// ```
/// use smeltery::prospect::SearchText;
///
/// let text = SearchText::parse("  Rust: \"forge\" -ana*", 200);
/// assert_eq!(text.terms(), ["rust", "forge", "ana"]);
/// assert!(text.prefix_last());
/// assert!(SearchText::parse("(*)", 200).is_empty());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchText {
    terms: Vec<String>,
}

impl SearchText {
    /// Parse `raw`, keeping at most `max_chars` characters of it.
    pub fn parse(raw: &str, max_chars: usize) -> Self {
        let trimmed = raw.trim();
        let cut = match trimmed.char_indices().nth(max_chars) {
            Some((at, _)) => trimmed.get(..at).unwrap_or(trimmed),
            None => trimmed,
        };
        let terms = cut
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .take(MAX_TERMS)
            .map(str::to_lowercase)
            // Lower-casing can add characters that are not letters or digits (e.g. a combining dot): drop them, so
            // every term holds letters and digits only.
            .map(|t| {
                t.chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
            })
            .filter(|t| !t.is_empty())
            .collect();
        Self { terms }
    }

    /// The terms, lower-cased, letters and digits only.
    pub fn terms(&self) -> &[String] {
        &self.terms
    }

    /// Whether there are no terms (no text condition).
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether the last term matches as a prefix (it has at least two characters).
    pub fn prefix_last(&self) -> bool {
        self.terms
            .last()
            .is_some_and(|t| t.chars().count() >= MIN_PREFIX)
    }

    /// The FTS5 query: every term double-quoted (a string, never an operator), joined by spaces (AND), the last
    /// followed by `*` when it is a prefix.
    pub(crate) fn fts5(&self) -> String {
        let last = self.terms.len().saturating_sub(1);
        self.terms
            .iter()
            .enumerate()
            .map(|(i, t)| {
                // Terms hold no `"`; doubling is the FTS5 escape all the same.
                let quoted = format!("\"{}\"", t.replace('"', "\"\""));
                if i == last && self.prefix_last() {
                    format!("{quoted}*")
                } else {
                    quoted
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The PostgreSQL `tsquery` text: quoted lexemes joined by `&`, the last with `:*` when it is a prefix.
    pub(crate) fn tsquery(&self) -> String {
        let last = self.terms.len().saturating_sub(1);
        self.terms
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let quoted = format!("'{}'", t.replace('\'', "''"));
                if i == last && self.prefix_last() {
                    format!("{quoted}:*")
                } else {
                    quoted
                }
            })
            .collect::<Vec<_>>()
            .join(" & ")
    }

    /// The MySQL boolean-mode query: `+term` for every term that InnoDB indexes, `+term*` for a prefix last term.
    /// Terms shorter than `min_len` characters (the server's `innodb_ft_min_token_size`) and InnoDB's default
    /// stopwords ([`MYSQL_STOPWORDS`]) are dropped first: InnoDB does not index them, so a required one would make the
    /// search find nothing. `None` when no term is left (the search then has no text condition).
    pub(crate) fn mysql_boolean(&self, min_len: usize) -> Option<String> {
        let last = self.terms.len().saturating_sub(1);
        let parts: Vec<String> = self
            .terms
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.chars().count() >= min_len.max(1) && !MYSQL_STOPWORDS.contains(&t.as_str())
            })
            .map(|(i, t)| {
                if i == last && self.prefix_last() {
                    format!("+{t}*")
                } else {
                    format!("+{t}")
                }
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    /// Whether `word` (lower-cased) matches term `i`.
    pub(crate) fn term_matches(&self, i: usize, word: &str) -> bool {
        let Some(term) = self.terms.get(i) else {
            return false;
        };
        if i + 1 == self.terms.len() && self.prefix_last() {
            word.starts_with(term.as_str())
        } else {
            word == term
        }
    }

    /// Whether `word` (lower-cased) matches any term.
    pub(crate) fn matches_any(&self, word: &str) -> bool {
        (0..self.terms.len()).any(|i| self.term_matches(i, word))
    }
}

/// InnoDB's default full-text stopwords (`INFORMATION_SCHEMA.INNODB_FT_DEFAULT_STOPWORD`, MySQL 8.4 "Full-Text
/// Stopwords"): the MySQL driver drops these terms from a search.
pub const MYSQL_STOPWORDS: [&str; 36] = [
    "a", "about", "an", "are", "as", "at", "be", "by", "com", "de", "en", "for", "from", "how",
    "i", "in", "is", "it", "la", "of", "on", "or", "that", "the", "this", "to", "was", "what",
    "when", "where", "who", "will", "with", "und", "the", "www",
];

/// The words of `text`, lower-cased (runs of letters and digits), as the memory engine and the Rust highlighter see
/// them.
pub(crate) fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_are_letters_and_digits_and_bounded() {
        let t = SearchText::parse("Ünïcode 東京 2026 näive", 200);
        assert_eq!(t.terms(), ["ünïcode", "東京", "2026", "näive"]);
        assert!(SearchText::parse("", 200).is_empty());
        assert!(SearchText::parse(" \t\n ", 200).is_empty());
        assert!(SearchText::parse("\"*()-:^", 200).is_empty());
        let many = (0..100)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(SearchText::parse(&many, 1000).terms().len(), MAX_TERMS);
        let long = "a".repeat(1000);
        let t = SearchText::parse(&long, 200);
        assert_eq!(t.terms(), ["a".repeat(200)]);
        // Cut at a character boundary, never inside one.
        let t = SearchText::parse(&"é".repeat(300), 200);
        assert_eq!(t.terms()[0].chars().count(), 200);
        // The prefix rule: two characters or more.
        assert!(SearchText::parse("rust f", 200).terms().len() == 2);
        assert!(!SearchText::parse("rust f", 200).prefix_last());
        assert!(SearchText::parse("rust fo", 200).prefix_last());
    }

    #[test]
    fn driver_strings_hold_only_quoted_terms() {
        let t = SearchText::parse("title:x NEAR( \"a\" b* ^c -d AND OR NOT", 200);
        assert_eq!(
            t.terms(),
            ["title", "x", "near", "a", "b", "c", "d", "and", "or", "not"]
        );
        assert_eq!(
            t.fts5(),
            r#""title" "x" "near" "a" "b" "c" "d" "and" "or" "not"*"#
        );
        assert_eq!(
            t.tsquery(),
            "'title' & 'x' & 'near' & 'a' & 'b' & 'c' & 'd' & 'and' & 'or' & 'not':*"
        );
        assert_eq!(
            t.mysql_boolean(3).as_deref(),
            Some("+title +near +and +not*")
        );
        assert_eq!(SearchText::parse("a b", 200).mysql_boolean(3), None);
        // Stopwords and words under the token size are dropped, a prefix included.
        assert_eq!(
            SearchText::parse("the forge of rust", 200)
                .mysql_boolean(3)
                .as_deref(),
            Some("+forge +rust*")
        );
        assert_eq!(
            SearchText::parse("forge fo", 200)
                .mysql_boolean(3)
                .as_deref(),
            Some("+forge")
        );
        assert_eq!(SearchText::parse("the with", 200).mysql_boolean(3), None);
        assert_eq!(
            SearchText::parse("go", 200).mysql_boolean(2).as_deref(),
            Some("+go*")
        );
        assert_eq!(
            SearchText::parse("rust forge", 200).fts5(),
            r#""rust" "forge"*"#
        );
        assert_eq!(SearchText::parse("x", 200).fts5(), r#""x""#);
    }

    #[test]
    fn matching_follows_the_prefix_rule() {
        let t = SearchText::parse("rust fo", 200);
        assert!(t.matches_any("rust") && t.matches_any("forge") && !t.matches_any("rusty"));
        assert_eq!(
            words("Hello, World-2!").collect::<Vec<_>>(),
            ["hello", "world", "2"]
        );
    }
}
