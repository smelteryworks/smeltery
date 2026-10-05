# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- Mold template syntax: escaped and raw output, comments, `@if`/`@elseif`/`@else`, `@unless`, `@for` with `@empty`
  and `loop`, `@extends`/`@section`/`@yield`, `@include` with variables, `<x-…>` components with props and named
  slots, `@csrf`, `@method`, `@error`, `@auth`, `@guest`, `@spark`, `@sparksScripts`, filters and functions.
- An `@` right after an ASCII letter, digit or `_` never starts a directive, so e-mail addresses such as
  `me@auth.example` stay text; write `x @endif`, not `x@endif`. An unclosed block whose closing directive is glued to
  a word is reported at that spot with the reason. Inside a block that takes it, a glued `x@else`, `x@elseif(…)` or
  `x@empty` followed by a space, a line break or `(` is an error, so a typo never merges a block's branches.
- Directives `@alloy` / `@alloy("id")`, `@alloyHead` and `@vite` / `@vite("a", …)`, rendered (unescaped) by the
  `Host` default methods `alloy_page`, `alloy_head` and `vite`. Without a host that implements them, `@alloy` and
  `@vite` are template errors at the directive and `@alloyHead` renders nothing.
- `session("key")` function and `Host::session` (default `None`): a session value as text, `""` when missing.
- Parser and resolver producing one flat tree with `file:line:col` spans (file paths written with `/` on every OS);
  include, component and layout cycles are reported with the chain.
- `Engine` (runtime interpreter with mtime-based hot reload), `Engine::global`, `views_dir`,
  `set_global_views_dir`.
- `Value` and `to_value` (any `T: Serialize`), the `Host` trait and `NoHost`, the `Template` trait with
  `render_runtime_with(engine, host)`.
- `Error` with file, line, column, excerpt and an HTML error page.
- Shared value semantics in `rt` used by the interpreter and by `#[derive(Mold)]`. `rt::Safe` holds component slot
  HTML and is not exported at the crate root; `to_value` turns it into `Value::Safe`, so both modes write it
  unescaped.

### Security
- The `json` filter: the value as a JavaScript literal with `<`, `>`, `&`, `'`, U+2028 and U+2029 escaped, for
  attributes that run JavaScript (`{{ v | json }}`) and `<script>` elements (`{!! v | json !!}`).
- The `url` filter: a relative or `http`/`https`/`mailto`/`tel` URL as it is, `#` for any other, and `#` when an
  `&` (an HTML entity such as `&#58;`), a byte-order mark or a control character comes before the first `/`, `?`
  or `#`, so the result is safe also when echoed raw.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
