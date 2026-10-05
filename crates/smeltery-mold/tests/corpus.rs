//! Golden tests: every `tests/corpus/<case>/` renders `views/page.mold.html` with `data.json` through the runtime
//! interpreter and must equal `expected.html` byte for byte. `MOLD_BLESS=1` rewrites the expected files.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use smeltery_mold::{Engine, Host, Value, to_value};
use std::path::Path;

/// The request data every corpus case renders with (the macro crate's equality test uses the same one).
struct TestHost {
    title_errors: Vec<String>,
}

impl Host for TestHost {
    fn csrf_token(&self) -> Option<&str> {
        Some("tok<&>")
    }
    fn authenticated(&self) -> bool {
        true
    }
    fn errors(&self, field: &str) -> &[String] {
        if field == "title" {
            &self.title_errors
        } else {
            &[]
        }
    }
    fn old(&self, field: &str) -> Option<&str> {
        (field == "title").then_some("Old <title>")
    }
    fn session(&self, key: &str) -> Option<String> {
        (key == "status").then(|| "Saved <ok>".to_owned())
    }
    fn route(&self, name: &str, params: &[(String, String)]) -> Result<String, String> {
        let q: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
        Ok(if q.is_empty() {
            format!("/{name}")
        } else {
            format!("/{name}?{}", q.join("&"))
        })
    }
    fn spark(&self, name: &str, props: &Value) -> Result<String, String> {
        let json = serde_json::to_string(props).map_err(|e| e.to_string())?;
        Ok(format!("<div data-spark=\"{name}\">{json}</div>"))
    }
    fn sparks_scripts(&self) -> String {
        "<script src=\"/sparks.js\"></script>".to_owned()
    }
    fn alloy_page(&self, id: &str) -> Result<String, String> {
        Ok(format!(
            "<script data-page=\"{id}\" type=\"application/json\">{{\"component\":\"welcome\"}}</script><div id=\"{id}\"></div>"
        ))
    }
    fn alloy_head(&self) -> String {
        "<meta name=\"alloy-head\">".to_owned()
    }
    fn vite(&self, entries: &[&str]) -> Result<String, String> {
        let entries = if entries.is_empty() {
            &["default.js"][..]
        } else {
            entries
        };
        Ok(entries
            .iter()
            .map(|e| format!("<script type=\"module\" src=\"/build/{e}\"></script>"))
            .collect())
    }
}

#[test]
fn corpus_renders_expected_html() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let host = TestHost {
        title_errors: vec!["Title is required".to_owned(), "second".to_owned()],
    };
    let mut cases: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    cases.sort();
    assert!(cases.len() >= 7, "corpus cases missing");
    for case in cases {
        let data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(case.join("data.json")).unwrap())
                .unwrap();
        let engine = Engine::new(case.join("views"));
        let html = engine
            .render("page", &to_value(&data).unwrap(), &host)
            .unwrap_or_else(|e| panic!("{}: {e}\n{}", case.display(), e.excerpt));
        let expected_path = case.join("expected.html");
        if std::env::var_os("MOLD_BLESS").is_some() {
            std::fs::write(&expected_path, &html).unwrap();
        }
        let expected = std::fs::read_to_string(&expected_path).unwrap();
        assert_eq!(html, expected, "case {}", case.display());
    }
}
