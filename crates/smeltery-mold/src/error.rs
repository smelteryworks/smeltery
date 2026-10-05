//! The one error type of Mold: parse, resolve and render errors, all pointing at a `.mold.html` file.

use std::fmt;

/// A Mold error: where it happened (`file:line:col`), what went wrong and a source excerpt.
///
/// `Display` prints `resources/views/posts/index.mold.html:3:7: unknown variable `titel``.
/// [`Error::to_html`] renders a readable development error page.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Error {
    /// The template file as shown to people (relative to the working directory when possible).
    /// Empty when the error is not tied to a file.
    pub file: String,
    /// 1-based line, 0 when the error is not tied to a position.
    pub line: u32,
    /// 1-based column (in characters), 0 when the error is not tied to a position.
    pub col: u32,
    /// What went wrong.
    pub message: String,
    /// The lines around the position with a caret under the column; empty when unavailable.
    pub excerpt: String,
}

impl Error {
    /// An error at `file:line:col`, with the excerpt cut from `source` (the file's text).
    pub fn at(file: &str, source: &str, line: u32, col: u32, message: impl Into<String>) -> Self {
        Self {
            file: file.to_owned(),
            line,
            col,
            message: message.into(),
            excerpt: excerpt(source, line, col),
        }
    }

    /// An error that is not tied to a template position.
    pub fn msg(message: impl Into<String>) -> Self {
        Self {
            file: String::new(),
            line: 0,
            col: 0,
            message: message.into(),
            excerpt: String::new(),
        }
    }

    /// An error about a whole file (for example a template that does not exist).
    pub fn in_file(file: &str, message: impl Into<String>) -> Self {
        Self {
            file: file.to_owned(),
            ..Self::msg(message)
        }
    }

    /// A small, self-contained HTML page describing the error (everything escaped), for development.
    pub fn to_html(&self) -> String {
        let mut out = String::from(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Mold error</title>\n\
             <style>body{font-family:system-ui,sans-serif;margin:2rem;background:#fff;color:#1a1a1a}\
             h1{color:#b42318;font-size:1.4rem}pre{background:#f4f4f5;padding:1rem;overflow:auto}\
             @media (prefers-color-scheme: dark){body{background:#18181b;color:#e4e4e7}pre{background:#27272a}}\
             </style>\n</head>\n<body>\n<h1>Mold error</h1>\n",
        );
        out.push_str("<p><code>");
        crate::rt::escape_into(&mut out, &self.location());
        out.push_str("</code></p>\n<pre>");
        crate::rt::escape_into(&mut out, &self.message);
        out.push_str("</pre>\n");
        if !self.excerpt.is_empty() {
            out.push_str("<pre>");
            crate::rt::escape_into(&mut out, &self.excerpt);
            out.push_str("</pre>\n");
        }
        out.push_str("</body>\n</html>\n");
        out
    }

    fn location(&self) -> String {
        if self.line == 0 {
            self.file.clone()
        } else {
            format!("{}:{}:{}", self.file, self.line, self.col)
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.file.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.location(), self.message)
        }
    }
}

impl std::error::Error for Error {}

impl From<crate::value::ValueError> for Error {
    fn from(e: crate::value::ValueError) -> Self {
        Self::msg(e.to_string())
    }
}

/// Up to three lines (the one before, the line itself, the one after) with a caret under `col`.
fn excerpt(source: &str, line: u32, col: u32) -> String {
    if line == 0 {
        return String::new();
    }
    let lines: Vec<&str> = source.lines().collect();
    let idx = line as usize - 1;
    if idx >= lines.len() {
        return String::new();
    }
    let first = idx.saturating_sub(1);
    let last = (idx + 1).min(lines.len() - 1);
    let width = (last + 1).to_string().len();
    let mut out = String::new();
    for (n, text) in lines.iter().enumerate().take(last + 1).skip(first) {
        out.push_str(&format!("{:>width$} | {}\n", n + 1, text));
        if n == idx {
            // Keep tabs so the caret lines up with what an editor shows.
            let pad: String = text
                .chars()
                .take((col as usize).saturating_sub(1))
                .map(|c| if c == '\t' { '\t' } else { ' ' })
                .collect();
            out.push_str(&format!("{:>width$} | {}^\n", "", pad));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_excerpt() {
        let e = Error::at(
            "v/a.mold.html",
            "one\n  {{ x }}\nthree\nfour",
            2,
            6,
            "unknown variable `x`",
        );
        assert_eq!(e.to_string(), "v/a.mold.html:2:6: unknown variable `x`");
        assert_eq!(e.excerpt, "1 | one\n2 |   {{ x }}\n  |      ^\n3 | three\n");
        assert!(e.to_html().contains("unknown variable `x`"));
        assert_eq!(Error::msg("boom").to_string(), "boom");
        assert_eq!(Error::in_file("f", "missing").to_string(), "f: missing");
    }

    #[test]
    fn html_page_is_escaped() {
        let e = Error::at("a", "<b>", 1, 1, "<script>");
        let html = e.to_html();
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }
}
