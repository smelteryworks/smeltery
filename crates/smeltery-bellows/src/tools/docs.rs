//! `docs_search`: the Smeltery guide (embedded) and the app's own agent docs, by section.

use std::path::Path;

use super::Outcome;

/// The Smeltery README, embedded at build time (a copy kept equal to the repository README by a test).
const GUIDE: &str = include_str!("../../guide/smeltery.md");

/// How many sections an answer holds, and how long each may be.
const MAX_SECTIONS: usize = 5;
const MAX_SECTION_CHARS: usize = 4000;

#[derive(Debug)]
struct Section {
    source: String,
    heading: String,
    body: String,
}

/// Splits Markdown into sections at `#`, `##` and `###` headings (not inside code fences).
fn sections(source: &str, text: &str) -> Vec<Section> {
    let mut out = Vec::new();
    let mut heading = String::from("(top)");
    let mut body = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let is_heading = !fenced
            && (line.starts_with("# ") || line.starts_with("## ") || line.starts_with("### "));
        if is_heading {
            if !body.trim().is_empty() {
                out.push(Section {
                    source: source.to_owned(),
                    heading: heading.clone(),
                    body: body.clone(),
                });
            }
            heading = line.trim_start_matches('#').trim().to_owned();
            body.clear();
        }
        body.push_str(line);
        body.push('\n');
    }
    if !body.trim().is_empty() {
        out.push(Section {
            source: source.to_owned(),
            heading,
            body,
        });
    }
    out
}

fn app_docs(root: &Path) -> Vec<(String, String)> {
    let mut docs = Vec::new();
    for rel in ["CLAUDE.md", ".bellows/guidelines.md"] {
        if let Ok(text) = std::fs::read_to_string(root.join(rel)) {
            docs.push((rel.to_owned(), text));
        }
    }
    let mut skills: Vec<_> = std::fs::read_dir(root.join(".bellows/skills"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    skills.sort();
    for path in skills {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            docs.push((format!(".bellows/skills/{name}"), text));
        }
    }
    docs
}

pub(super) fn search(root: &Path, query: &str) -> Outcome {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|t| t.len() > 1)
        .collect();
    if terms.is_empty() {
        return Outcome::error("`query` needs at least one word");
    }
    let mut all = sections("Smeltery README", GUIDE);
    for (source, text) in app_docs(root) {
        all.extend(sections(&source, &text));
    }
    let mut scored: Vec<(usize, usize, &Section)> = all
        .iter()
        .filter_map(|s| {
            let heading = s.heading.to_lowercase();
            let body = s.body.to_lowercase();
            let matched = terms.iter().filter(|t| body.contains(t.as_str())).count();
            if matched == 0 {
                return None;
            }
            let hits: usize = terms.iter().map(|t| body.matches(t.as_str()).count()).sum();
            let in_heading = terms
                .iter()
                .filter(|t| heading.contains(t.as_str()))
                .count();
            // Sections matching more of the words win, then words in the heading, then frequency.
            Some((
                matched * 1000 + in_heading * 100 + hits.min(99),
                body.len(),
                s,
            ))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    if scored.is_empty() {
        return Outcome::ok(format!("no section mentions: {}", terms.join(" ")));
    }
    let answer: Vec<String> = scored
        .iter()
        .take(MAX_SECTIONS)
        .map(|(_, _, s)| {
            let mut body: String = s.body.chars().take(MAX_SECTION_CHARS).collect();
            if body.len() < s.body.len() {
                body.push_str("\n…");
            }
            format!("## {} — {}\n\n{}", s.source, s.heading, body.trim_end())
        })
        .collect();
    Outcome::ok(answer.join("\n\n---\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_headings_outside_code() {
        let text = "intro\n# A\none\n```\n# not a heading\n```\n## B\ntwo\n";
        let s = sections("x", text);
        let headings: Vec<&str> = s.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(headings, ["(top)", "A", "B"]);
        assert!(s.get(1).is_some_and(|a| a.body.contains("# not a heading")));
    }

    #[test]
    fn the_embedded_guide_is_the_repository_readme() {
        assert_eq!(GUIDE, include_str!("../../../../README.md"));
    }

    #[test]
    fn the_guide_answers_about_sparks() {
        let dir = std::env::temp_dir();
        let out = search(&dir, "wire:click actions");
        assert!(!out.is_error);
        assert!(out.text.contains("Sparks"), "{}", out.text);
    }
}
