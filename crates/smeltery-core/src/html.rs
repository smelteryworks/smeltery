//! HTML helpers.

use std::borrow::Cow;

/// Escape `&`, `<`, `>`, `"` and `'` for HTML text and attribute values.
///
/// ```
/// assert_eq!(
///     smeltery_core::html::escape("<a href=\"x\">Tom & 'Jerry'</a>"),
///     "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;"
/// );
/// ```
pub fn escape(input: &str) -> Cow<'_, str> {
    if !input
        .bytes()
        .any(|b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\''))
    {
        return Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len() + 16);
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}
