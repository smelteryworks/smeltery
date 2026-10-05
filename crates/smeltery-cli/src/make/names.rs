//! Name handling: `PostComment`, `post_comment` and `post-comment` are the same name.

use anyhow::bail;

/// The forms of one name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Name {
    /// Lowercase words: `["post", "comment"]`.
    pub(crate) words: Vec<String>,
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type",
    "unsafe", "use", "where", "while", "yield", "abstract", "become", "box", "do", "final",
    "macro", "override", "priv", "typeof", "unsized", "virtual",
];

/// True when `word` is a Rust keyword (it cannot be a module, field or function name).
pub(crate) fn is_keyword(word: &str) -> bool {
    KEYWORDS.contains(&word)
}

impl Name {
    /// Parses `input` (`PostComment`, `post_comment`, `post-comment`, `postComment`).
    pub(crate) fn parse(input: &str) -> anyhow::Result<Self> {
        let valid = input
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && input
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            bail!(
                "invalid name `{input}`: use letters, digits, `_` or `-`, starting with a letter \
                 (e.g. `PostComment`, `post_comment`)"
            );
        }
        let mut words = Vec::new();
        let mut current = String::new();
        let mut prev: Option<char> = None;
        for c in input.chars() {
            if c == '_' || c == '-' {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                prev = None;
                continue;
            }
            let boundary = c.is_ascii_uppercase()
                && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit());
            if boundary && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.push(c.to_ascii_lowercase());
            prev = Some(c);
        }
        if !current.is_empty() {
            words.push(current);
        }
        let name = Name { words };
        if is_keyword(&name.snake()) {
            bail!(
                "invalid name `{input}`: `{}` is a Rust keyword",
                name.snake()
            );
        }
        Ok(name)
    }

    /// Drops a trailing word such as `controller` (`PostController` → `Post`) unless it is the only one.
    pub(crate) fn without_suffix(mut self, suffix: &str) -> Self {
        if self.words.len() > 1 && self.words.last().is_some_and(|w| w == suffix) {
            self.words.pop();
        }
        self
    }

    /// `post_comment`.
    pub(crate) fn snake(&self) -> String {
        self.words.join("_")
    }

    /// `PostComment`.
    pub(crate) fn pascal(&self) -> String {
        self.words.iter().map(|w| capitalize(w)).collect()
    }

    /// `Post comment` (for labels and headings).
    pub(crate) fn sentence(&self) -> String {
        capitalize(&self.words.join(" "))
    }

    /// The plural form: the last word pluralized.
    pub(crate) fn plural(&self) -> Name {
        let mut words = self.words.clone();
        if let Some(last) = words.last_mut() {
            *last = plural(last);
        }
        Name { words }
    }

    /// `post-comment`.
    pub(crate) fn kebab(&self) -> String {
        self.words.join("-")
    }

    /// `postComment`, a TypeScript variable name; a JavaScript reserved word gets `Record` added (`classRecord`).
    pub(crate) fn camel(&self) -> String {
        let mut out = String::new();
        for (i, word) in self.words.iter().enumerate() {
            if i == 0 {
                out.push_str(word);
            } else {
                out.push_str(&capitalize(word));
            }
        }
        if JS_RESERVED.contains(&out.as_str()) {
            out.push_str("Record");
        }
        out
    }
}

/// JavaScript's reserved words that are not Rust keywords (those never get this far).
const JS_RESERVED: &[&str] = &[
    "case",
    "catch",
    "class",
    "debugger",
    "default",
    "delete",
    "export",
    "extends",
    "finally",
    "function",
    "implements",
    "import",
    "instanceof",
    "interface",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "switch",
    "this",
    "throw",
    "var",
    "void",
    "with",
];

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

/// English plural, the simple rules only: `post` → `posts`, `category` → `categories`, `box` → `boxes`.
pub(crate) fn plural(word: &str) -> String {
    let consonant_y = word
        .strip_suffix('y')
        .filter(|stem| stem.chars().last().is_some_and(|c| !"aeiou".contains(c)));
    if let Some(stem) = consonant_y {
        format!("{stem}ies")
    } else if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|s| word.ends_with(s))
    {
        format!("{word}es")
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_normalise_to_one_name() {
        for input in [
            "PostComment",
            "post_comment",
            "post-comment",
            "postComment",
            "Post_Comment",
        ] {
            let name = Name::parse(input).unwrap_or_else(|e| unreachable!("{input}: {e}"));
            assert_eq!(name.snake(), "post_comment", "{input}");
            assert_eq!(name.pascal(), "PostComment", "{input}");
            assert_eq!(name.kebab(), "post-comment", "{input}");
            assert_eq!(name.sentence(), "Post comment", "{input}");
            assert_eq!(name.plural().snake(), "post_comments", "{input}");
        }
        let n = Name::parse("HTTPClient").unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(n.snake(), "httpclient");
        let n = Name::parse("Report2Pdf").unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(n.snake(), "report2_pdf");
    }

    #[test]
    fn invalid_names_are_rejected() {
        for bad in ["", "1post", "post comment", "post!", "_post", "type", "Fn"] {
            assert!(Name::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn suffixes_are_dropped() {
        let n = Name::parse("PostController").map(|n| n.without_suffix("controller"));
        assert_eq!(n.map(|n| n.snake()).ok().as_deref(), Some("post"));
        let n = Name::parse("Controller").map(|n| n.without_suffix("controller"));
        assert_eq!(n.map(|n| n.snake()).ok().as_deref(), Some("controller"));
    }

    #[test]
    fn plurals() {
        for (one, many) in [
            ("post", "posts"),
            ("category", "categories"),
            ("day", "days"),
            ("box", "boxes"),
            ("class", "classes"),
            ("match", "matches"),
            ("wish", "wishes"),
        ] {
            assert_eq!(plural(one), many);
        }
    }
}
