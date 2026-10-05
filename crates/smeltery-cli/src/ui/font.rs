//! Hand-drawn block letters for the `smeltery new` banner.
//!
//! Every glyph is five rows of full blocks `█` and spaces, with strokes two columns wide (D-235); all rows of one
//! glyph have the same width. The banner spells SMELTERY; the table covers `a-z`, `0-9`, `-` and `_`.

/// Rows per glyph.
pub(crate) const HEIGHT: usize = 5;

/// Columns between two glyphs.
pub(crate) const GAP: usize = 1;

/// The glyph for `c` (case-insensitive), or `None` for a character names never contain.
pub(crate) fn glyph(c: char) -> Option<[&'static str; HEIGHT]> {
    Some(match c.to_ascii_lowercase() {
        'a' => [" █████ ", "██   ██", "███████", "██   ██", "██   ██"],
        'b' => ["██████ ", "██   ██", "██████ ", "██   ██", "██████ "],
        'c' => [" ██████", "██     ", "██     ", "██     ", " ██████"],
        'd' => ["██████ ", "██   ██", "██   ██", "██   ██", "██████ "],
        'e' => ["███████", "██     ", "█████  ", "██     ", "███████"],
        'f' => ["███████", "██     ", "█████  ", "██     ", "██     "],
        'g' => [" ██████", "██     ", "██  ███", "██   ██", " ██████"],
        'h' => ["██   ██", "██   ██", "███████", "██   ██", "██   ██"],
        'i' => ["██████", "  ██  ", "  ██  ", "  ██  ", "██████"],
        'j' => ["     ██", "     ██", "     ██", "██   ██", " █████ "],
        'k' => ["██   ██", "██  ██ ", "█████  ", "██  ██ ", "██   ██"],
        'l' => ["██     ", "██     ", "██     ", "██     ", "███████"],
        'm' => [
            "███    ███",
            "████  ████",
            "██ ████ ██",
            "██  ██  ██",
            "██      ██",
        ],
        'n' => [
            "███    ██",
            "████   ██",
            "██ ██  ██",
            "██  ██ ██",
            "██   ████",
        ],
        'o' => [" █████ ", "██   ██", "██   ██", "██   ██", " █████ "],
        'p' => ["██████ ", "██   ██", "██████ ", "██     ", "██     "],
        'q' => [" █████ ", "██   ██", "██   ██", "██  ██ ", " ███ ██"],
        'r' => ["██████ ", "██   ██", "██████ ", "██   ██", "██   ██"],
        's' => ["███████", "██     ", "███████", "     ██", "███████"],
        't' => ["████████", "   ██   ", "   ██   ", "   ██   ", "   ██   "],
        'u' => ["██   ██", "██   ██", "██   ██", "██   ██", " █████ "],
        'v' => ["██    ██", "██    ██", " ██  ██ ", "  ████  ", "   ██   "],
        'w' => [
            "██      ██",
            "██      ██",
            "██  ██  ██",
            "██ ████ ██",
            " ███  ███ ",
        ],
        'x' => ["██    ██", " ██  ██ ", "  ████  ", " ██  ██ ", "██    ██"],
        'y' => ["██    ██", " ██  ██ ", "  ████  ", "   ██   ", "   ██   "],
        'z' => ["███████", "    ██ ", "   ██  ", "  ██   ", "███████"],
        '0' => [" █████ ", "██  ███", "██ █ ██", "███  ██", " █████ "],
        '1' => ["  ██  ", " ███  ", "  ██  ", "  ██  ", "██████"],
        '2' => ["██████ ", "     ██", " █████ ", "██     ", "███████"],
        '3' => ["██████ ", "     ██", " █████ ", "     ██", "██████ "],
        '4' => ["██   ██", "██   ██", "███████", "     ██", "     ██"],
        '5' => ["███████", "██     ", "██████ ", "     ██", "██████ "],
        '6' => [" ██████", "██     ", "██████ ", "██   ██", " █████ "],
        '7' => ["███████", "     ██", "    ██ ", "   ██  ", "   ██  "],
        '8' => [" █████ ", "██   ██", " █████ ", "██   ██", " █████ "],
        '9' => [" █████ ", "██   ██", " ██████", "     ██", " █████ "],
        '-' => ["     ", "     ", "█████", "     ", "     "],
        '_' => ["       ", "       ", "       ", "       ", "███████"],
        _ => return None,
    })
}

/// Width of a glyph in columns.
pub(crate) fn width(glyph: &[&str; HEIGHT]) -> usize {
    glyph[0].chars().count()
}

/// The rows of `text` in block letters, wrapped so no row is wider than `max_width` columns. Characters
/// without a glyph are skipped. Each wrapped line of text gives [`HEIGHT`] rows.
pub(crate) fn render(text: &str, max_width: usize) -> Vec<String> {
    let glyphs: Vec<[&str; HEIGHT]> = text.chars().filter_map(glyph).collect();
    let mut lines: Vec<Vec<[&str; HEIGHT]>> = vec![Vec::new()];
    let mut used = 0;
    for g in glyphs {
        let w = width(&g);
        let need = if used == 0 { w } else { used + GAP + w };
        if need > max_width && used > 0 {
            lines.push(Vec::new());
            used = w;
        } else {
            used = need;
        }
        if let Some(line) = lines.last_mut() {
            line.push(g);
        }
    }
    let mut rows = Vec::new();
    for line in lines.iter().filter(|l| !l.is_empty()) {
        for r in 0..HEIGHT {
            let row: Vec<&str> = line
                .iter()
                .map(|g| g.get(r).copied().unwrap_or_default())
                .collect();
            rows.push(row.join(&" ".repeat(GAP)).trim_end().to_owned());
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_character_has_a_well_formed_glyph() {
        for c in ('a'..='z').chain('0'..='9').chain(['-', '_']) {
            let g = glyph(c).unwrap_or_else(|| panic!("no glyph for {c:?}"));
            let w = width(&g);
            for row in g {
                assert_eq!(row.chars().count(), w, "ragged glyph {c:?}");
                assert!(
                    row.chars().all(|ch| matches!(ch, '█' | '▀' | '▄' | ' ')),
                    "{c:?}"
                );
            }
        }
    }

    #[test]
    fn long_names_wrap_within_the_width() {
        let rows = render("a-very-long-application-name-2026", 80);
        assert!(rows.len() > HEIGHT && rows.len().is_multiple_of(HEIGHT));
        assert!(rows.iter().all(|r| r.chars().count() <= 80));
        assert_eq!(render("blog", 80).len(), HEIGHT);
    }
}
