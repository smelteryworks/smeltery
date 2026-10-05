# smeltery-alloy

Alloy is the React and Vue bridge of [Smeltery](https://github.com/smelteryworks/smeltery): the server side of the
[Inertia](https://inertiajs.com) protocol, version 3. A controller returns a page (a component name and its props)
instead of a Mold view; Inertia's official client packages (`@inertiajs/react`, `@inertiajs/vue3`) render it in the
browser. The first visit gets an HTML page from a Mold root template with the page object embedded, every later
visit gets JSON. Node is a build tool only: the server runs the Rust binary and serves Vite's build from
`public/build/`.

Apps use it through the `smeltery` crate as `smeltery::alloy` and `#[derive(smeltery::Alloy)]`.

## Setup

```rust
use smeltery::alloy::{self, Alloy, AlloyExt as _, Page, Props, SharedCtx};

/// The root template, `resources/views/app.mold.html`: `@vite`, `@alloyHead` in the head, `@alloy` in the body.
#[derive(smeltery::Mold, Default)]
#[mold("app")]
struct Root {}

/// Props every page gets. A page prop with the same key wins.
async fn shared(ctx: SharedCtx) -> smeltery::Result<Props> {
    Ok(Props::new().with("app", serde_json::json!({ "name": ctx.app().settings().name })))
}

async fn dashboard() -> Page {
    alloy::render("dashboard")
        .with("greeting", "Welcome back")
        .optional("stats", || async { Ok(42) })
        .defer("activity", || async { Ok(vec!["Signed in"]) })
}

fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.alloy(
        Alloy::new()
            .root::<Root>()
            .entries(["resources/js/app.tsx"])
            .share(shared),
    )
    .routes(|r| {
        r.get("/dashboard", dashboard);
    })
}
```

`.alloy(…)` adds a middleware to every web route (inside the session and CSRF stack), renders `@alloy`,
`@alloyHead` and `@vite` in views, sets the `XSRF-TOKEN` cookie that Inertia's client sends back as
`X-XSRF-TOKEN`, and names `X-Inertia` in the `Vary` header of every web response.

## Props

| Method | Sent |
|---|---|
| `.with(key, value)` | on full visits and on partial reloads that name it (serialized at once) |
| `.with_lazy(key, f)` | like `.with`, computed only when sent |
| `.optional(key, f)` | only on partial reloads that name it |
| `.defer(key, f)`, `.defer_in(group, key, f)` | listed in `deferredProps`; the client loads it right after the page |
| `.merge`, `.prepend`, `.deep_merge` (+ `.match_on`) | like `.with`, with merge metadata for the client |
| `.always(key, value)` | on every response, partial reloads included |

`.status(code)`, `.encrypt_history(bool)` and `.clear_history()` set page options. `props.errors` holds the
validation errors of the previous request (the first message per field; `Alloy::all_errors` for arrays), and the
page's `flash` holds every value the previous request flashed with `session.flash(key, value)` whose key does not
start with `_`. Everything in a page is public: it is readable in the HTML and in the JSON, so never put secrets,
whole models or the CSRF token into props or flash values.

A typed page:

```rust
#[derive(serde::Serialize, smeltery::Alloy)]
#[alloy("posts/index")]
struct PostsIndex {
    titles: Vec<String>,
}

async fn index() -> PostsIndex {
    PostsIndex { titles: vec!["Hello".into()] }
}
```

## The protocol

- An Inertia visit gets the page object as JSON with `X-Inertia: true`; a first visit gets the root template with
  `<script data-page="app" type="application/json">…</script><div id="app"></div>` (the JSON escaped so no prop can
  close the script element).
- An Inertia `GET` / `HEAD` whose `X-Inertia-Version` differs from the asset version gets `409` with
  `X-Inertia-Location` (the request's own path and query) before the handler runs; the flash survives.
- `X-Inertia-Location` for the request itself and the page object's `url` start with exactly one `/`: leading slashes
  and backslashes become one `/` (`//evil.example/x` becomes `/evil.example/x`), so they always stay on this site.
- For Inertia requests: a Mold page answers `409` `X-Inertia-Location` (a full page load), a `302` after
  `PUT` / `PATCH` / `DELETE` becomes `303`, a redirect to a URL with `#` becomes `409` `X-Inertia-Redirect`, an empty
  `200` becomes a redirect back. A failed validation and a CSRF failure redirect back (`303`).
- `alloy::location(url)` leaves the app: `409` `X-Inertia-Location` for Inertia visits, `303` otherwise.
  `alloy::clear_history(&session)` sets `clearHistory` on the next page.
- Headers a handler sets on a page's response stay on the finished page (both the first visit and the JSON), every
  value of a repeated one (two `Set-Cookie` stay two), except the headers the protocol sets itself (`Content-Type`,
  `X-Inertia`, …), which win, and the headers that describe the handler's body or the connection (`Content-Length`,
  `Content-Encoding`, `Content-Range`, `Content-Disposition`, `ETag`, `Last-Modified`, `Connection`, `Keep-Alive`,
  `Transfer-Encoding`, `Upgrade`, `TE`, `Trailer`), which are dropped: `let mut res = alloy::render("settings/two-factor").into_response();` then
  `res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))` keeps a page with secrets
  out of the browser's cache.

## Vite

`@vite` renders the configured entries, `@vite("a", "b")` the named ones:

- in debug builds, while `storage/framework/vite.hot` holds the dev server's URL: the dev server's client and
  entry scripts. The file is ignored under `APP_ENV=production` and when it holds anything other than an `http(s)`
  URL on `127.0.0.1`, `localhost` or `[::1]` (logged once as a warning);
- with `public/build/manifest.json`: the entries' CSS, `modulepreload` links for their imported chunks, and their
  scripts;
- with neither: a template error in debug builds, nothing under `APP_ENV=testing`, nothing (and an error in the log
  when the server starts and on the first page) in release builds.

When the server starts (`serve`, not console commands or `work`), Alloy logs where the assets come from.
`Alloy::build_dir` takes a folder under `public/` of letters, digits and `_ . / @ -` without `..`; any other value
stops the app at boot.

The asset version is the first 32 hex characters of SHA-256 over the manifest, `""` without one or while the dev
server runs (`Alloy::version` overrides it). Release builds read the manifest once; debug builds re-read it when it
changes.

## Testing

`smeltery::alloy::testing` adds `get_alloy` / `reload_alloy` / `post_alloy` (a JSON form post with `X-Inertia`,
as `form.post` sends it) to `TestApp` and `assert_component`, `assert_prop`, `assert_missing`, `assert_deferred`,
`prop` and `alloy_page` to its responses (JSON answers and first-visit HTML alike), and
`assert_page_file_exists(component)`.

## Licence

MIT OR Apache-2.0.
