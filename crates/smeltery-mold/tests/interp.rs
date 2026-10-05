//! Runtime-mode behaviour: errors with positions and excerpts, scoping, data conversion.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use smeltery_mold::{Engine, Error, NoHost, Value, to_value};
use std::fs;

fn engine(files: &[(&str, &str)]) -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in files {
        let path = dir.path().join(format!("{name}.mold.html"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let engine = Engine::new(dir.path());
    (dir, engine)
}

fn data(json: &str) -> Value {
    to_value(&serde_json::from_str::<serde_json::Value>(json).unwrap()).unwrap()
}

fn render_err(files: &[(&str, &str)], json: &str) -> Error {
    let (_dir, e) = engine(files);
    e.render("page", &data(json), &NoHost).unwrap_err()
}

#[test]
fn unknown_variable_points_at_file_line_col() {
    let e = render_err(
        &[("page", "<h1>\n  {{ titel }}\n</h1>")],
        r#"{"title": "x"}"#,
    );
    assert!(e.file.ends_with("page.mold.html"), "{}", e.file);
    assert_eq!((e.line, e.col), (2, 6));
    assert_eq!(e.message, "unknown variable `titel`");
    assert!(
        e.to_string()
            .ends_with("page.mold.html:2:6: unknown variable `titel`")
    );
    assert_eq!(
        e.excerpt,
        "1 | <h1>\n2 |   {{ titel }}\n  |      ^\n3 | </h1>\n"
    );
}

#[test]
fn runtime_errors() {
    let e = render_err(
        &[("page", "{{ post.titel }}")],
        r#"{"post": {"title": "x"}}"#,
    );
    assert_eq!(
        (e.line, e.col, e.message.as_str()),
        (1, 8, "no field `titel`")
    );
    let e = render_err(&[("page", "@for(x in n){{ x }}@endfor")], r#"{"n": 3}"#);
    assert_eq!(e.message, "cannot loop over an integer");
    let e = render_err(&[("page", "{{ a / b }}")], r#"{"a": 1, "b": 0}"#);
    assert_eq!(e.message, "division by zero");
    let e = render_err(&[("page", "{{ list }}")], r#"{"list": [1]}"#);
    assert_eq!(e.message, "cannot display a list");
    let e = render_err(&[("page", "x\n@csrf")], "{}");
    assert_eq!((e.line, e.col), (2, 1));
    assert!(e.message.contains("CSRF"));
    let e = render_err(&[("page", "{{ route(\"home\") }}")], "{}");
    assert!(e.message.contains("route `home`"));
    let e = render_err(&[("page", "@spark(\"c\")")], "{}");
    assert_eq!(e.message, "Sparks are not enabled");
    let off = "Alloy is not enabled: call `.alloy(…)` in bootstrap/app.rs";
    let e = render_err(&[("page", "<head>\n  @alloy(\"root\")")], "{}");
    assert_eq!((e.line, e.col, e.message.as_str()), (2, 3, off));
    let e = render_err(&[("page", "@alloyHead @vite")], "{}");
    assert_eq!((e.line, e.col, e.message.as_str()), (1, 12, off));
    let e = render_err(&[("page", "\n\n@vite(\"a.ts\")")], "{}");
    assert_eq!((e.line, e.col, e.message.as_str()), (3, 1, off));
    let e = render_err(&[("page", "{{ items[5] }}")], r#"{"items": [1]}"#);
    assert_eq!(e.message, "index 5 is out of range for a list of 1 items");
}

#[test]
fn include_sees_parent_scope_component_does_not() {
    let (_d, e) = engine(&[
        (
            "page",
            "@for(p in ps)@include(\"row\", { n: loop.index })@endfor",
        ),
        ("row", "[{{ n }}:{{ p }}:{{ site }}]"),
    ]);
    let out = e
        .render("page", &data(r#"{"ps": ["a", "b"], "site": "S"}"#), &NoHost)
        .unwrap();
    assert_eq!(out, "[1:a:S][2:b:S]");
}

#[test]
fn parse_errors_surface_from_render() {
    let e = render_err(&[("page", "@if(x)\nno end")], "{}");
    assert_eq!((e.line, e.col), (1, 1));
    assert_eq!(e.message, "unclosed `@if` (missing `@endif`)");
}

#[test]
fn global_views_dir_defaults() {
    let dir = smeltery_mold::views_dir();
    assert!(dir.ends_with("resources/views"));
    assert!(!smeltery_mold::set_global_views_dir("elsewhere"));
}
