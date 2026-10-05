# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- The server side of the Inertia protocol (v3): `render`, `Page`, `Props` (`with`, `with_lazy`, `optional`,
  `defer`, `defer_in`, `merge`, `prepend`, `deep_merge`, `match_on`, `always`), `Component` (with
  `#[derive(Alloy)]` in `smeltery-macros`), shared props (`Alloy::share`, `SharedCtx`), `props.errors` with error
  bags, the page-level `flash`, `clearHistory` / `encryptHistory`, partial reloads with dot paths, deferred groups,
  merge metadata, asset-version `409`s, `X-Inertia-Location` / `X-Inertia-Redirect`, `302` → `303` after
  `PUT` / `PATCH` / `DELETE`, empty `200` → back. Headers a handler sets on a page's response
  (`page.into_response()`, e.g. `Cache-Control: no-store`) are kept on the finished page with every value (two `Set-Cookie` stay two), except names the protocol sets itself (its values win), the headers that describe the handler's body (`Content-Type`, `Content-Length`, `Content-Encoding`, `Content-Range`, `Content-Disposition`, `ETag`, `Last-Modified`) and the hop-by-hop ones (`Connection`, `Keep-Alive`, `Transfer-Encoding`, `Upgrade`, `TE`, `Trailer`). The request's own `X-Inertia-Location` and the page's `url` start
  with exactly one `/` (`//evil.example/x` → `/evil.example/x`); partial-reload header lists keep at most 256 entries.
- `Alloy` and `AlloyExt::alloy`: the web middleware, the `XSRF-TOKEN` cookie, `Vary: X-Inertia`, and the
  `AlloyRenderer` behind Mold's `@alloy`, `@alloyHead` and `@vite`.
- Vite tags from the dev server's hot file (debug builds only, never under `APP_ENV=production`, only an `http(s)`
  URL on `127.0.0.1`, `localhost` or `[::1]`) or `public/build/manifest.json` (`Alloy::build_dir` checked at boot,
  escaped in the tags); the asset version
  (SHA-256 of the manifest, `version`); an asset check when the server starts (`serve` only, through
  `AppBuilder::on_serve`; console commands such as `migrate` log nothing about assets).
- `location`, `clear_history`.
- `testing`: `AlloyRequests` (`get_alloy`, `reload_alloy`, `post_alloy`), `AlloyAssertions`,
  `assert_page_file_exists`.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
