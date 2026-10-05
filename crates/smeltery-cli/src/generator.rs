//! Helpers for generators that edit existing files without overwriting user code.

use std::path::Path;

use anyhow::{Context, bail};

/// What [`insert_before_marker`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inserted {
    /// The line was added.
    Added,
    /// The line was already in the file; nothing changed.
    AlreadyThere,
}

/// Inserts `line` above the first line containing `marker` in `file`.
///
/// - Idempotent: when the file already holds the line (its first line, ignoring surrounding whitespace) nothing
///   changes.
/// - The inserted lines take the marker line's indentation; later lines of a multi-line `line` keep their own
///   indentation on top of it.
/// - `pub mod …;` and `pub use …;` lines are kept sorted within the run of such lines directly above the marker.
/// - A missing marker is an error that shows the line to add by hand; the file is then left untouched.
pub(crate) fn insert_before_marker(
    file: &Path,
    marker: &str,
    line: &str,
) -> anyhow::Result<Inserted> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    match insert_in_text(&text, marker, line) {
        Some(Some(updated)) => {
            let updated = drop_placeholder(&updated, marker);
            // Through a temporary file and a rename: a crash or a full disk never leaves the user's file half
            // written (D-352).
            crate::files::replace(file, updated.as_bytes())
                .with_context(|| format!("cannot write {}", file.display()))?;
            Ok(Inserted::Added)
        }
        Some(None) => Ok(Inserted::AlreadyThere),
        None => bail!(
            "marker `{marker}` not found in {}; add this line by hand:\n    {}",
            file.display(),
            line.trim()
        ),
    }
}

/// Lines a new app keeps only while nothing is registered above a marker (so the function's parameter counts as
/// used); the first registration at that marker removes them. Matched as whole trimmed lines, so an edited line stays.
const PLACEHOLDERS: &[(&str, &str)] = &[
    (
        "// smeltery:agents",
        "let _ = &w; // keeps `w` used while nothing is registered",
    ),
    (
        "// smeltery:searchables",
        "let _ = &p; // keeps `p` used while nothing is registered",
    ),
];

/// `text` without the placeholder line of `marker`, if it has one.
fn drop_placeholder(text: &str, marker: &str) -> String {
    match PLACEHOLDERS.iter().find(|(m, _)| *m == marker) {
        Some((_, placeholder)) => text
            .split_inclusive('\n')
            .filter(|l| l.trim() != *placeholder)
            .collect(),
        None => text.to_owned(),
    }
}

/// rustfmt's layout of a one-argument call statement (`m.add(path);`) that does not fit in 100 columns at
/// `indent`: the argument on its own line (`m.add(\n    path,\n);`). `None` when it fits or is not such a call, so
/// lines added to the user's files are what `cargo fmt` would write, without reformatting the user's code.
pub(crate) fn wrap_call(line: &str, indent: usize) -> Option<String> {
    if line.contains('\n') || indent + line.chars().count() <= 100 {
        return None;
    }
    let body = line.strip_suffix(");")?;
    let open = body.find('(')?;
    let (callee, arg) = (body.get(..open)?, body.get(open + 1..)?);
    if arg.is_empty() || arg.contains(['(', ',', '"']) || callee.contains(' ') {
        return None;
    }
    Some(format!("{callee}(\n    {arg},\n);"))
}

/// `None`: marker missing. `Some(None)`: line already present. `Some(Some(text))`: the updated text.
fn insert_in_text(text: &str, marker: &str, line: &str) -> Option<Option<String>> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let at = lines.iter().position(|l| l.contains(marker))?;
    let marker_line = lines.get(at).copied().unwrap_or_default();
    let indent: String = marker_line
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let wrapped = wrap_call(line.trim(), indent.len());
    let line = wrapped.as_deref().unwrap_or(line);
    let first = line.lines().next().unwrap_or_default().trim();
    // The line that tells whether the entry is there: the first one, or the first argument when the call is laid
    // out vertically (`m.add(` alone matches every other registration).
    let key = line
        .lines()
        .map(str::trim)
        .find(|l| !l.ends_with('('))
        .unwrap_or(first);
    if lines.iter().any(|l| l.trim() == key) {
        return Some(None);
    }

    // Sorted position among `pub mod` / `pub use` lines directly above the marker.
    let mut pos = at;
    if let Some(prefix) = ["pub mod ", "pub use "]
        .into_iter()
        .find(|p| first.starts_with(p))
    {
        let mut start = at;
        while start > 0
            && lines
                .get(start - 1)
                .is_some_and(|l| l.trim_start().starts_with(prefix))
        {
            start -= 1;
        }
        pos = (start..at)
            .find(|&i| lines.get(i).is_some_and(|l| l.trim() > first))
            .unwrap_or(at);
    }

    let mut block = String::new();
    for (n, l) in line.lines().enumerate() {
        block.push_str(&indent);
        block.push_str(if n == 0 { first } else { l.trim_end() });
        block.push('\n');
    }
    let mut out = String::with_capacity(text.len() + block.len());
    for (i, l) in lines.iter().enumerate() {
        if i == pos {
            out.push_str(&block);
        }
        out.push_str(l);
    }
    Some(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_above_marker_with_its_indent() {
        let dir = tempfile::tempdir().ok();
        let dir = dir.as_ref().map(|d| d.path()).unwrap_or(Path::new("."));
        let file = dir.join("web.rs");
        let src =
            "pub fn routes(r: &mut Router) {\n    r.get(\"/\", home);\n    // smeltery:routes\n}\n";
        assert!(std::fs::write(&file, src).is_ok());
        let changed =
            insert_before_marker(&file, "// smeltery:routes", "r.get(\"/posts\", posts);");
        assert!(matches!(changed, Ok(Inserted::Added)));
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        assert_eq!(
            text,
            "pub fn routes(r: &mut Router) {\n    r.get(\"/\", home);\n    r.get(\"/posts\", posts);\n    // smeltery:routes\n}\n"
        );
    }

    /// A registration too long for 100 columns goes in rustfmt's vertical layout, and a second long one is not taken
    /// for the first (their first lines are both `m.add(`).
    #[test]
    fn long_registrations_are_wrapped_like_rustfmt() {
        let text = "pub fn register(m: &mut Migrator) {\n    // smeltery:migrations\n}\n";
        let a = "m.add(m2026_10_03_120000_create_customer_support_tickets_table::CreateCustomerSupportTicketsTable);";
        let b = "m.add(m2026_10_03_120001_create_warehouse_stock_movements_table::CreateWarehouseStockMovementsTable);";
        let once = insert_in_text(text, "// smeltery:migrations", a)
            .flatten()
            .unwrap_or_default();
        assert_eq!(
            once,
            "pub fn register(m: &mut Migrator) {\n    m.add(\n        m2026_10_03_120000_create_customer_support_tickets_table::CreateCustomerSupportTicketsTable,\n    );\n    // smeltery:migrations\n}\n"
        );
        assert_eq!(
            insert_in_text(&once, "// smeltery:migrations", a),
            Some(None)
        );
        let twice = insert_in_text(&once, "// smeltery:migrations", b)
            .flatten()
            .unwrap_or_default();
        assert!(
            twice.contains("CreateWarehouseStockMovementsTable,\n    );"),
            "{twice}"
        );
        // Short lines and calls with several arguments stay as they are.
        assert_eq!(wrap_call("m.add(m1::Create);", 4), None);
        assert_eq!(
            wrap_call(&format!("r.get(\"/{}\", x);", "a".repeat(100)), 4),
            None
        );
    }

    #[test]
    fn is_idempotent() {
        let text = "pub mod home;\n// smeltery:mods\n";
        assert_eq!(
            insert_in_text(text, "// smeltery:mods", "pub mod home;"),
            Some(None)
        );
        let once = insert_in_text(text, "// smeltery:mods", "pub mod posts;")
            .flatten()
            .unwrap_or_default();
        assert_eq!(once, "pub mod home;\npub mod posts;\n// smeltery:mods\n");
        assert_eq!(
            insert_in_text(&once, "// smeltery:mods", "pub mod posts;"),
            Some(None)
        );
    }

    #[test]
    fn missing_marker_is_an_error_and_leaves_the_file() {
        let dir = tempfile::tempdir().ok();
        let dir = dir.as_ref().map(|d| d.path()).unwrap_or(Path::new("."));
        let file = dir.join("mod.rs");
        assert!(std::fs::write(&file, "pub mod home;\n").is_ok());
        let err = insert_before_marker(&file, "// smeltery:mods", "pub mod posts;");
        let msg = err.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(msg.contains("add this line by hand"), "{msg}");
        assert!(msg.contains("pub mod posts;"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap_or_default(),
            "pub mod home;\n"
        );
        // A line already present but no marker: still an error, the file is not in the expected shape.
        assert!(insert_before_marker(&file, "// smeltery:mods", "pub mod home;").is_err());
        assert!(insert_in_text("pub mod home;\n", "// m", "pub mod home;").is_none());
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(
            insert_before_marker(Path::new("/nonexistent/smeltery/mod.rs"), "// m", "x").is_err()
        );
    }

    #[test]
    fn mod_and_use_lines_stay_sorted() {
        let text = "pub mod b;\npub mod d;\n// smeltery:mods\n\npub use b::Model as B;\n// smeltery:models\n";
        let t = insert_in_text(text, "// smeltery:mods", "pub mod c;")
            .flatten()
            .unwrap_or_default();
        let t = insert_in_text(&t, "// smeltery:mods", "pub mod a;")
            .flatten()
            .unwrap_or_default();
        let t = insert_in_text(&t, "// smeltery:mods", "pub mod e;")
            .flatten()
            .unwrap_or_default();
        let t = insert_in_text(&t, "// smeltery:models", "pub use a::Model as A;")
            .flatten()
            .unwrap_or_default();
        assert_eq!(
            t,
            "pub mod a;\npub mod b;\npub mod c;\npub mod d;\npub mod e;\n// smeltery:mods\n\n\
             pub use a::Model as A;\npub use b::Model as B;\n// smeltery:models\n"
        );
    }

    #[test]
    fn multi_line_blocks_take_the_marker_indent() {
        let text = "fn routes(r: &mut Router) {\n    // smeltery:routes\n}\n";
        let block = "r.resource(\"/posts\")\n    .index(posts::index);";
        let t = insert_in_text(text, "// smeltery:routes", block)
            .flatten()
            .unwrap_or_default();
        assert_eq!(
            t,
            "fn routes(r: &mut Router) {\n    r.resource(\"/posts\")\n        .index(posts::index);\n    // smeltery:routes\n}\n"
        );
        assert_eq!(insert_in_text(&t, "// smeltery:routes", block), Some(None));
    }
}
