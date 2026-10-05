# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- `#[derive(Mold)]` with `#[mold("name")]`, `crate = "…"` and `dir = "…"`: implements `Template` with
  `render_runtime` (runtime engine, fields converted with `to_value`) and `render_compiled` (code generated from
  the resolved template, byte-identical output).
- `render_runtime_with`, and an `IntoResponse` impl (`::smeltery::view::view`) when the crate path is the default
  `::smeltery::mold`, so handlers return the struct.
- A hidden function-like `template!` macro: the `#[derive(Mold)]` expansion for a given struct (used by
  `#[derive(Spark)]`).
- Compiled `session("key")` calls (`Host::session`) and the compiled `json` and `url` filters, byte-identical to the
  runtime interpreter. A `rt::Safe` value echoed with `{{ }}` renders unescaped in both modes.
- Compile errors for template syntax errors, missing templates and template variables without a struct field,
  naming the `.mold.html` file (written with `/` on every OS), line and column.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
