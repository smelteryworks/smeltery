<p align="center">
  <img src="https://raw.githubusercontent.com/smelteryworks/smeltery/main/smeltery-github-cover.png"
       alt="Smeltery: batteries-included full-stack Rust" width="100%">
</p>

<p align="center">
  <a href="https://crates.io/crates/smeltery"><img alt="crates.io" src="https://img.shields.io/crates/v/smeltery.svg"></a>
  <a href="https://docs.rs/smeltery"><img alt="docs.rs" src="https://img.shields.io/docsrs/smeltery"></a>
  <a href="https://github.com/smelteryworks/smeltery/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/smelteryworks/smeltery/actions/workflows/ci.yml/badge.svg"></a>
  <a href="#licence"><img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg"></a>
  <a href="https://www.rust-lang.org"><img alt="Rust 1.94+" src="https://img.shields.io/badge/rust-1.94%2B-orange.svg"></a>
  <a href="https://smeltery.org"><img alt="Website: smeltery.org" src="https://img.shields.io/badge/website-smeltery.org-e8590c.svg"></a>
</p>

<p align="center">
  <a href="https://smeltery.org"><b>smeltery.org</b></a> ·
  <a href="https://docs.rs/smeltery">Docs</a> ·
  <a href="https://crates.io/crates/smeltery">crates.io</a> ·
  <a href="https://github.com/smelteryworks/smeltery">GitHub</a>
</p>

# Smeltery

Batteries-included full-stack Rust: routing, typed configuration from `.env`, an application container, middleware,
error pages, the Mold template engine, Sparks live components (zero hand-written JavaScript), Anvil for WebSockets and
broadcasting (the Pusher Channels protocol), models and migrations for SQLite, PostgreSQL and MySQL, seeders and
factories, sessions, CSRF, validation and authentication, a cache with atomic locks (database, Redis, memcached, file
and memory stores), Watchfire for supervised long-running agents, queued jobs and the scheduler, mail, console
commands, a test client, Bellows for coding agents, and the `smeltery` command-line tool with an app generator and
`make:*` generators.
Fully async on Tokio, Axum and SeaORM.

```text
cargo install smeltery
smeltery new my-app
cd my-app && smeltery serve
```

Minimum supported Rust version: 1.94.

## Install

```text
cargo install smeltery
```

This installs the `smeltery` command. Apps depend on the library (the generator writes this line for you):

```toml
[dependencies]
smeltery = { version = "0.1.0", default-features = false, features = ["sqlite"] }
```

## Create an app

```text
smeltery new my-app
```

`smeltery new` asks a few questions. Every question is also a flag; with any flag given, or when the terminal is not
interactive, no question is asked and the defaults apply. At the end it prints the equivalent one-line command.

| Question | Flag | Choices (default first) |
|---|---|---|
| What are you building? | `--kind` | `web` (a web app), `headless` (Watchfire agents, jobs and the scheduler, no web pages) |
| Database | `--db` | `sqlite`, `postgres`, `mysql` |
| Starter kit (not asked for `headless`) | `--frontend` | `mold` (Mold + Sparks), `react` (React, Inertia, TypeScript), `vue` (Vue, Inertia, TypeScript) |
| Install Tailwind? / Tailwind CSS? (not asked for `headless`) | `--tailwind` / `--no-tailwind` | yes |
| Alpine.js? (asked for the Mold frontend) | `--alpine` / `--no-alpine` | no |
| Which building blocks should we smelt into your stack? (not asked for `headless`; `--kind headless` with a block in `--smelt` is refused, `--smelt none` is accepted) | `--smelt` | `watchfire,temper`; any of `watchfire`, `temper`, `hallmark`, `anvil`, `prospect` (comma-separated), or `none` |
| Bellows (AI-agent files) | `--bellows` | `none`, or any of `mcp`, `skills`, `guidelines` (comma-separated), or `all` |
| Install the npm packages now? (asked for `react` and `vue` when Node.js 20.19+ or 22.12+ is found) | `--npm` / `--no-npm` | yes |
| Run the migrations now? (`smeltery migrate` in the new app) | `--migrate` / `--no-migrate` | yes |
| Seed the basic data? (`smeltery db:seed`, after the migrations) | `--seed` / `--no-seed` | yes |
| Initialise git? | `--git` / `--no-git` | yes |

The building blocks of a web app:

- `watchfire`: Watchfire agents, jobs and the scheduler in the app (`app/agents/`, the Watchfire tables, the
  `/_watchfire` dashboard). A headless app always has Watchfire.
- `temper`: authentication through Temper, described below.
- `hallmark`: API tokens for mobile apps, desktop apps and other clients ([API tokens](#api-tokens)). It needs user
  accounts, so it brings `temper` with it: the checklist and `--smelt hallmark` add Temper and say so
  (`Hallmark needs Temper: added`).
- `anvil`: WebSockets and broadcasting ([Broadcasting](#broadcasting)).
- `prospect`: full-text search for models ([Search](#search)).

Under the checklist (and with `--smelt`), a chosen block that works well with one that is not chosen gets a hint,
and nothing is added for it: Anvil works with Temper (private channels for signed-in users), with Hallmark
(private channels for mobile apps and other clients) and with Watchfire (broadcasting from agents and jobs).

`.env` gets a fresh `APP_KEY` and `SESSION_DRIVER=database` (the `sessions` table is one of every new app's
migrations); `.env.example` lists the same keys without secrets. Every new app's `users` table has the nullable
`credentials_epoch` column and its `User` model returns it from `Authenticatable::credentials_epoch`, so
`auth::end_credentials` signs a user out everywhere.

With authentication (the `temper` building block, on by default), a web app has registration, log-in, log-out,
password reset by mail and a dashboard behind the `auth` and `verified` middleware, and its seeder creates a demo
user under `APP_ENV` `local` or `testing`. Email verification is scaffolded and off: the `email_verified_at` column,
`MustVerifyEmail` on `User`, the notice page, the signed link's route and "send it again"; uncommenting
`.verify_email::<app::models::User>()` in `bootstrap/app.rs` turns it on.

The authentication is [Temper](#temper-the-authentication-routes) in every starter kit: `bootstrap/app.rs` calls
`.temper(app::providers::temper::temper())`. `app/providers/temper.rs` turns on registration, password reset, e-mail
verification, profile and password updates and two-factor authentication and names the page of each `GET` route
(Mold views in `resources/views/auth/`, React or Vue pages in `resources/js/pages/auth/` through `alloy::render`,
with the password confirmation and two-factor challenge pages); in a React or Vue app it also clears the browser's
history state on logout. `app/actions/temper/` holds the forms and what they do, with the password rules in one place
(`password_rules.rs`). The app adds the settings pages `/settings/profile`, `/settings/password` and
`/settings/two-factor` (`app/controllers/settings.rs`; the two-factor page sits behind `password.confirm` and shows
the QR code as an `<img>`, the key, the confirmation form and the recovery codes once; all three answer
`Cache-Control: no-store`; the React and Vue page fetches the QR code and the key from Temper's JSON routes, so they
never become props, and drops the recovery codes from its props as soon as it shows them, so the back button does not
show them again), the `add_two_factor_columns_to_users_table` migration, the four `two_factor_*` fields and `TwoFactorAuthenticatable` on
`User`, `AUTH_PASSWORD_TIMEOUT=10800` in `.env`, and tests for the profile, the password change, two-factor log-in,
recovery codes, the password confirmation and signing a user out everywhere. A React or Vue app's shared `auth.user`
has `two_factor_enabled`.

The forms that create an account or ask for and use a password reset (`POST /register`, `/forgot-password`,
`/reset-password/{token}`) carry `.middleware("throttle:6,1")`: at most 6 requests a minute per client on each of
those routes, counted in the cache; past that the form shows "Too many attempts. Please try again in N seconds."
(429 for JSON clients); the login form (`POST /login`) carries `.middleware("throttle:30,1")`. The auth forms read the e-mail
address trimmed and in lower case (`smeltery::auth::deserialize_email`), so `Ada@Example.com` and
`ada@example.com` are one account. Registering an address that has an account answers "The email has already been taken.", which tells the visitor that the address is registered; the
throttle limits how fast addresses can be tried.
Without `auth` the app has none of these pages, no `password_reset_tokens` migration and no demo user; the
`users` table and the `User` model stay, and `.temper(…)` turns authentication on in such an app (its `CLAUDE.md` lists the steps).

With `hallmark`, `bootstrap/app.rs` calls `.hallmark(Hallmark::new())`, and the app has the
`create_personal_access_tokens_table` migration, `routes/api.rs` with `POST /api/tokens` (`throttle:10,1`: a token
for `email`, `password`, `device_name` and, for an account with two-factor authentication on, `code`, through
`issue_for_credentials`), `DELETE /api/tokens/current` and `GET /api/user` (both `auth:hallmark`), their
controllers in `app/controllers/api/`, the `HALLMARK_*` settings as comments in `.env`, and `tests/api_tokens.rs`,
whose tests create their own users with `UserFactory`. With Watchfire too, `app/agents/mod.rs` schedules
`hallmark-prune` daily (tokens expired a day or more). `smeltery hallmark:install` adds the same files to an app
made without the block.

With `anvil`, `bootstrap/app.rs` calls `.anvil(routes::channels::channels)`, and the app has `routes/channels.rs`
(the public channel `announcements`; with `auth` also `users.{user}`, which only that user may join),
`app/events/announcement_posted.rs` (an event on `announcements`), the `pubsub_messages` migration (also without
Watchfire), the `ANVIL_*` settings as comments in `.env` (the key and the secret are derived from `APP_KEY` until
set; the CORS settings too) and `tests/broadcasting.rs`. With `auth` it also has `app/events/user_notified.rs` (an
event on `private-users.<id>`). A Mold app listens without JavaScript: the Spark `announcements`
(`app/sparks/announcements.rs`, on the home page) shows the newest announcements, and with `auth` the Spark
`notifications` on the dashboard listens to the user's private channel and its "Notify me" button sends one. A React
or Vue app gets Echo: `package.json` pins `laravel-echo` 2.5.0, `@laravel/echo-react` or `@laravel/echo-vue` 2.5.0 and
`pusher-js` 8.6.0; `resources/js/echo.ts` connects to the page's own host and port with the key the pages get as the
shared prop `app.anvil_key`, authorizes private and presence channels at `/broadcasting/auth` with the session
cookie and the `XSRF-TOKEN` cookie as `X-XSRF-TOKEN`, and adds `X-Socket-ID` to Inertia's requests; the entry
calls it before the first page. The welcome page shows the announcements card; with `auth` the dashboard's "Live"
card listens to the user's private channel, sends itself an event through `POST /notify-me` (`auth`,
`throttle:10,1`) and shows how many people are online through the presence channel `presence-dashboard`, with the
viewer's own name ("3 people online · you: Ada"). The channel shares each member's id only, so no other user's name
reaches the browser; `routes/channels.rs` shows the one line that shares names. The `create_presence_tables`
migration holds the members under `PUBSUB_DRIVER=database`.

With `prospect`, `bootstrap/app.rs` calls `.prospect(app::providers::search::register)`, `app/providers/search.rs`
registers the searchable models above `// smeltery:searchables`, and `.env` has `PROSPECT_DRIVER=database`; no
migration is written until a model is made searchable. `smeltery make:model Post title:string body:text --searchable
--all` then writes `impl Searchable` (every `string` / `text` field searched, the first with `Weight::A`, `foreign`
fields as filters), the `SearchIndex` in the create-table migration (without `-m`, a separate
`add_search_index_to_posts_table` migration), the registration, and a list page that searches: `GET /posts?q=…&page=…`
behind `throttle:60,1`, ranked and paginated, the label highlighted (Mold: a search form and Previous / Next buttons,
no JavaScript; React and Vue: a search box that reloads the page 300 ms after the last key). `--searchable` is
refused in an app without the block and for a model without a `string` or `text` field. `smeltery prospect:install`
adds the provider to a web app made without the block.

With Alpine.js, the app gets Alpine.js 3.17.4 (the official `dist/cdn.min.js` of the npm package, with its MIT
licence header) as `public/assets/js/alpine.min.js`, embedded in the CLI, so nothing is downloaded. The layout loads
it with `defer` after `@sparksScripts`, the welcome page shows an Alpine toggle, and the counter Spark reads its count
through `$spark`.

With `--frontend react` or `--frontend vue` the pages are React or Vue components in TypeScript under
`resources/js/pages/`, rendered in the browser by Inertia's client packages; the controllers return them through
`smeltery::alloy` (`alloy::render("welcome")`), and `resources/views/app.mold.html` is the HTML of a first visit.
The app has the same routes, authentication and settings pages and tests as a Mold app, a welcome page whose "Ask the forge"
button loads an optional prop with a partial reload, and a dashboard with a deferred prop. `package.json` pins exact
npm versions (Vite 8.3.2, Inertia 3.8.0, React 19.3.0 or Vue 3.5.43, TypeScript 6.0.3); `npm install` runs in the new
app unless `--no-npm` is given or no usable Node.js is found, for at most 5 minutes (`SMELTERY_NPM_TIMEOUT` sets
the seconds), and a failed or stopped install leaves the app complete, with `npm install` in the next steps. The
first install writes `package-lock.json`, which pins every package the kit pulls in, its dependencies' dependencies
included, with integrity hashes; with it in the repository, `smeltery build` (and `npm ci` on a CI runner or a
server) installs exactly those versions. With Tailwind the kit uses the `tailwindcss` npm packages through
`@tailwindcss/vite` (no binary download); without it, `resources/css/app.css` is a prebuilt stylesheet of the kit's
pages. Node.js builds the assets only: `smeltery serve` starts the Vite dev server
(`node node_modules/vite/bin/vite.js`, on `127.0.0.1:5173`) next to the app, and `smeltery build` runs `npm run build`
into `public/build/` before the release build. `.env` gets `VITE_APP_NAME`; every `VITE_` value is compiled into
the JavaScript and is public. [Alloy: React and Vue](#alloy-react-and-vue) describes the pages, the generators and
the tests of these apps.

### Pages and CSS

A new web app opens on a welcome page and has styled log-in, register, password-reset and dashboard pages, in light
and dark (following the system setting), with no JavaScript besides Sparks (and Alpine.js when chosen). They use a compiled
`public/assets/css/app.css` that ships with the app, so they look the same without any tool installed. It is built
from `resources/css/app.css` (the theme colours and the component classes `btn-primary`, `btn-secondary`,
`form-input`, `form-label`, `form-error`, `panel`, `link`, `alert-success`, `alert-error`) and from the classes in the
view templates, including the views the `make:*` generators write. In an app, Tailwind reads the classes in
`resources/views/` and `app/` (the `@source` lines of `resources/css/app.css`); other folders are not scanned.

To change the CSS, Smeltery runs the Tailwind CSS standalone binary. `smeltery new` downloads it (Tailwind v4.3.3,
about 110 MB, over HTTPS from GitHub, checked against its SHA-256) into a per-user folder:
`%LOCALAPPDATA%\smeltery\bin` on Windows, `~/Library/Application Support/smeltery/bin` on macOS, and
`$XDG_DATA_HOME/smeltery/bin` or `~/.local/share/smeltery/bin` elsewhere; `smeltery tailwind:install` does the same for an existing app. The binaries
exist for Linux (x64 and arm64, glibc and musl), macOS (x64 and arm64) and Windows (x64). `smeltery serve` and
`smeltery build` use `TAILWIND_BIN` when it is set, then that binary, then `tailwindcss` in a folder on `PATH` given
as an absolute path (empty, `.` and other relative entries are skipped). Before they run the per-user binary they
check it again: its SHA-256 (a `.verified` stamp file next to it skips the hashing while the binary's size and
modification time stay the same) and, on Linux and macOS, that the binary, its folder and `smeltery/` belong to the
user and are not writable by the group or others, and that the data folder above them is not writable by others. A binary that fails is not run (a warning names the reason), and `smeltery tailwind:install`
replaces it. When the download fails, the app is created anyway with its prebuilt CSS, and the summary shows
`smeltery tailwind:install` to retry.

## The app layout

```text
my-app/
├── app/
│   ├── controllers/  models/  middleware/  providers/  services/
│   ├── commands/  jobs/  agents/  mail/  sparks/  helpers/
├── bootstrap/
│   ├── app.rs        wires the app: config, routes, middleware (the library root)
│   └── main.rs       the binary: hands the command line to smeltery::run
├── config/           typed config structs read from .env
├── routes/           web.rs (browser routes), api.rs (under /api)
├── database/         migrations/  seeders/  factories/  database.sqlite (SQLite apps)
├── resources/        views/ (pages, layouts/, components/, sparks/)  css/
├── public/           served as-is (assets/images, assets/css, assets/js)
├── storage/          app/public  app/private (the app's files)  framework (Smeltery's)  logs
├── tests/
├── .env  .env.example
├── CLAUDE.md  AGENTS.md
└── Cargo.toml
```

The app is one crate: `bootstrap/app.rs` is the library root and pulls each folder in as a module, so
`crate::app::controllers::home` is `app/controllers/home.rs`. Every folder has a `mod.rs`.

`storage/app/` holds only the app's own files (`public/` is served at `/storage` after `smeltery storage:link`,
`private/` never is). `storage/framework/` holds Smeltery's working files, created when first needed: `cache/`
(`CACHE_STORE=file`), `sessions/` (`SESSION_DRIVER=file`), `uploads/` and `sparks/` (upload temp files). With `--db sqlite` the database is
`database/database.sqlite` (`?mode=rwc` creates it on first connect). Git ignores the database file and everything
under `storage/`.

## Routes and controllers

```rust
use smeltery::prelude::*;

async fn home() -> Html<&'static str> {
    Html("<h1>Welcome</h1>")
}

async fn show(Path(id): Path<u64>) -> Result<String> {
    if id == 0 {
        return Err(Error::not_found());
    }
    Ok(format!("post {id}"))
}

async fn index() -> &'static str { "all posts" }
async fn dashboard() -> &'static str { "admin" }

pub fn routes(r: &mut Router) {
    r.get("/", home).name("home");
    r.resource("/posts").index(index).show(show);
    r.group("/admin", |r| {
        r.get("/", dashboard).name("dashboard");
    })
    .name("admin.")
    .middleware("auth");
}
```

- `get`, `post`, `put`, `patch`, `delete`, `any`; `.name("…")`; `.middleware("alias")`.
- Paths use `{param}` placeholders and `{*rest}` for a catch-all tail.
- `group(prefix, …)` with `.name("prefix.")` and `.middleware("alias")` for every route in it.
- `resource("/posts")` declares the actions you pick:

| Action | Method | Path | Name |
|---|---|---|---|
| `index` | GET | `/posts` | `posts.index` |
| `create` | GET | `/posts/create` | `posts.create` |
| `store` | POST | `/posts` | `posts.store` |
| `show` | GET | `/posts/{post}` | `posts.show` |
| `edit` | GET | `/posts/{post}/edit` | `posts.edit` |
| `update` | PUT, PATCH | `/posts/{post}` | `posts.update` |
| `destroy` | DELETE | `/posts/{post}` | `posts.destroy` |

- `app.url("posts.show", &[("post", "7")])` builds `/posts/7`; extra parameters become the query string.
- HTML forms send `PUT`, `PATCH` and `DELETE` as a `POST` with a `_method` field (or the `X-HTTP-Method-Override`
  header).
- Routes from `routes/api.rs` are registered with `api_routes` and live under `/api`.
- Files in `public/` are served as-is when no route matches, with `ETag`, `Last-Modified` and the
  `Cache-Control` of `STATIC_CACHE_CONTROL` (default `no-cache`: browsers revalidate and get a 304 while the file is
  unchanged). Apps whose asset URLs change with their content can set `public, max-age=31536000, immutable`.
- Files under `/storage` (uploads, after `smeltery storage:link`) also get `Content-Security-Policy: sandbox`, so a
  stored HTML or SVG file never runs as a page of the site, `X-Content-Type-Options: nosniff`, and
  `Content-Disposition: attachment` unless they are a PNG, JPEG, GIF, WebP, AVIF, BMP or icon image, a PDF, plain
  text, video or audio. PDFs get no `sandbox` (browsers do not show a sandboxed PDF). The headers follow the file
  that is served: any file from `public/` whose real path (links followed) lies in `storage/app/public` or
  `public/storage` gets them, whatever URL reached it. A static-file path with a segment that ends in `.` or a
  space, or holds `:` or `\` (`/storage./…`, `/storage%2e/…`, `/storage::$INDEX_ALLOCATION/…`), answers 404 on
  every OS, because Windows opens such names as another file.
- The framework adds `GET /up`, a health check for load balancers and uptime monitors: `200` with the text `OK` and
  `Cache-Control: no-store`, without a session or CSRF check. `route:list` shows it. An app route at `GET /up`
  replaces it; `AppBuilder::without_health_route()` leaves it out.
- `.middleware("throttle:5,1")` allows a route 5 requests a minute per client (`throttle:<max>,<minutes>`;
  `throttle:<max>` counts per minute). The client is the authenticated user (the request's principal: a signed-in
  session on web routes, or the principal an `auth:` alias listed before `throttle:` stored, see
  [Guards and the principal](#guards-and-the-principal)), else the address after `TRUSTED_PROXIES` (an IPv6 client
  by its /64). Each route counts on its own, by its path pattern, so
  `/reset-password/{token}` has one counter for every token. The count lives in the app's cache store
  (`CACHE_STORE`), so the processes of an app (`serve --no-agents` beside `work`, several web processes) share
  it on the `database`, `redis`, `memcached` and `file` stores; with `CACHE_STORE=null` it lives in the process's memory. A cache error answers 500 rather than letting the request
  through. Responses carry `X-RateLimit-Limit` and `X-RateLimit-Remaining`. Past the limit a form on a web route is
  sent back (303, its input flashed) with "Too many attempts. Please try again in N seconds." on the field `email`
  (`throttle:5,1,name` puts it on `name`), and JSON clients and API routes get 429 "Too Many Requests" with
  `Retry-After`. An invalid value (`throttle:0`, `throttle:x`) stops the app at boot. The windows are fixed and
  follow the clock (`throttle:5,1` counts per calendar minute): a client that sends 5 requests at the
  end of one minute and 5 at the start of the next gets all 10 through within a few seconds.
- Aliases with arguments (`throttle:5,1`, `auth:web`) come from middleware families: a prefix and a function that
  builds the middleware for each route from the arguments and the route (`POST /login`), while the app builds. An
  error from it stops the app at boot. Core has the families `throttle` and `auth`; `AppBuilder::middleware_family`
  adds others:

```rust
use smeltery::middleware::{BoxedMiddleware, Next, Request};
use smeltery::{AppBuilder, Error};

fn build(app: AppBuilder) -> AppBuilder {
    // `.middleware("tag:beta")` adds `X-Tag: beta` to the route's responses.
    app.middleware_family("tag", |args, _route| {
        let value = smeltery::http::HeaderValue::from_str(args)
            .map_err(|_| Error::internal(format!("invalid tag `{args}`")))?;
        Ok(BoxedMiddleware::new(move |req: Request, next: Next| {
            let value = value.clone();
            async move {
                let mut res = next.run(req).await;
                res.headers_mut().insert("x-tag", value);
                res
            }
        }))
    })
}
# fn main() {}
```

  Two families with one prefix, a prefix that is empty or holds `:`, and a plain alias that starts with a family's
  `<prefix>:` stop the app at boot. `route:list` shows the alias as written.

Handlers are async functions. Their arguments are extractors: `Path`, `Query`, `Form`, `Json` (Axum's, re-exported in
`smeltery::http`), `App`, `ClientInfo` (the client's IP, scheme and host, see [The server](#the-server)), and
`Config<T>` for your config structs. They return anything that implements
`IntoResponse`, usually `smeltery::Result<T>`, or a Mold template struct (below).
`smeltery::http::wants_json(&headers)` tells whether a client asks for JSON (its `Accept` names `application/json` or
a `+json` type: error pages, `auth` and `verified` answer JSON then) and `smeltery::http::is_inertia(&headers)`
whether a request is an Inertia visit (`X-Inertia: true`).

## Mold templates

Views are `resources/views/<name>.mold.html` files with an `@`-directive syntax. A struct with `#[derive(Mold)]` names
its template; its fields are the template's variables. A handler returns the struct (or `Result<TheStruct>`):

```rust,no_run
use smeltery::prelude::*;
# #[derive(serde::Serialize)]
# pub struct Post { pub title: String }
# async fn load_posts() -> Result<Vec<Post>> { Ok(Vec::new()) }

#[derive(Mold)]
#[mold("posts/index")]          // resources/views/posts/index.mold.html
pub struct PostsIndex {
    pub title: String,
    pub posts: Vec<Post>,       // fields implement serde::Serialize
}

async fn index() -> Result<PostsIndex> {
    Ok(PostsIndex { title: "Posts".into(), posts: load_posts().await? })
}
```

```text
@extends("layouts/app")

@section("title", title)

@section("content")
<h1>{{ title }}</h1>
@for(post in posts)
  <x-card :title="post.title">
    <a href="{{ route("posts.show", { post: post.id }) }}">Read</a>
  </x-card>
@empty
  <p>No posts yet.</p>
@endfor
@endsection
```

| Syntax | Does |
|---|---|
| `{{ expr }}` / `{!! expr !!}` | output, HTML-escaped / raw |
| `{{-- … --}}` | comment, not rendered |
| `@{{` / `@@` | a literal `{{` / `@` |
| `@if(expr)` `@elseif(expr)` `@else` `@endif`, `@unless(expr)` `@endunless` | conditions |
| `@for(x in items)` `@empty` `@endfor`, `@for(key, value in map)`, `@for x in items` | loops; `loop.index`, `loop.index0`, `loop.first`, `loop.last`, `loop.count` |
| `@extends("layouts/app")`, `@section("name")` … `@endsection`, `@section("name", expr)`, `@yield("name", "default")` | layouts |
| `@include("partials/nav", { key: expr })` | include another template; it sees the current variables plus the given ones |
| `<x-alert type="error" :count="n">body</x-alert>`, `<x-forms.input />` | components from `resources/views/components/` (`forms/input`); string attributes, `:attr` expressions, the body as `{{ slot }}`, named slots with `@slot("title")` … `@endslot` |
| `@csrf`, `@method("PUT")` | hidden `_token` and `_method` form fields |
| `@error("field")` … `@enderror` | the body with `{{ message }}` when the field has a validation error |
| `@auth` … `@else` … `@endauth`, `@guest` … `@endguest` | by sign-in state |
| `@spark("counter", { start: 5 })`, `@sparksScripts` | a live component and the Sparks runtime (see [Sparks](#sparks-live-components)) |

Expressions have `||`, `&&`, `== != < <= > >=`, `+ - * / %`, `!`, field access `a.b`, indexing `a[i]`, string,
number, `true`/`false`/`null` literals, the filters `upper`, `lower`, `trim`, `title`, `len`, `default(x)`,
`join(sep)`, `json` and `url` (`{{ name | upper }}`), and the functions `route("name", { param: expr })`, `old("field")`,
`session("key")` (a session value as text, `""` when missing) and `csrf_token()`. `false`, `0`, `""`, empty lists, and `None` are falsy. A line holding only a block directive leaves
no blank line in the output. An `@word` that is not a directive (`@media`) stays text, and an `@` right after an
ASCII letter, digit or `_` never starts a directive, so e-mail addresses (`me@auth.example`) stay text too. A
closing or branching directive glued to a word (`x@endif`, `x@else`) is a parse error that names the spot and says to
put a space before the `@` (`x @endif`).

`{{ }}` escapes `& < > " '`, which is right for text and for quoted attribute values. A value that becomes
JavaScript or a link takes a filter:

- `json` writes the value (text, a number, a list, a struct) as a JavaScript literal in which `<`, `>`, `&`, `'`,
  U+2028 and U+2029 are `\u` escapes. In an attribute whose value runs as JavaScript (`x-data`, `@click`, `x-init`,
  `onclick`, the arguments of `wire:click`) write `{{ value | json }}` without quotes around it:
  `x-data="{ name: {{ user.name | json }} }"`, `wire:click="remove({{ post.id | json }}, {{ post.slug | json }})"`.
  Inside a `<script>` element write `{!! value | json !!}`, because the browser does not decode `&quot;` there:
  `<script>const post = {!! post | json !!};</script>`. Text echoed between quotes in JavaScript (`'{{ name }}'`)
  is not safe: the browser turns `&#39;` back into `'` before the code runs.
- `url` keeps a relative URL and an `http:`, `https:`, `mailto:` or `tel:` URL, and writes `#` for any other
  (`javascript:`, `data:`), and for a value with an `&`, a byte-order mark or a control character before its first
  `/`, `?` or `#` (an entity such as `&#58;` hides a scheme): `<a href="{{ link | url }}">`.
- Attribute values are quoted (`class="{{ name }}"`): an unquoted value ends at the first space of the text.
- `{!! !!}` never goes into an attribute: it writes `"` as it is. In attributes use `{{ }}` (with `json` or `url`);
  `{!! v | json !!}` belongs inside `<script>` only.

Two modes, same output:

- **Debug builds** read the templates at runtime from `<root>/resources/views` and re-read a file when it changes,
  so an edit shows on the next request without recompiling.
- **Release builds** run Rust code generated from the templates at compile time. A template variable without a
  matching field, a syntax error and a missing template are compile errors naming the `.mold.html` file and line.

A template error while rendering (for example a division by zero) answers 500. With `APP_DEBUG=true` the page shows
the file, line, column and the lines around it; otherwise it is the normal error page and the details go to the
log.

The values `@csrf`, `@auth`, `@guest`, `@error`, `old()` and `session()` read come from `smeltery::view::ViewData`,
which the session middleware of web routes fills (see [Sessions](#sessions)). On API routes there is no session:
`@csrf` is a render error there and `@auth` renders its `@else` branch. `route()` uses the app's named routes.

## Wiring: `bootstrap/app.rs`

```rust
use smeltery::AppBuilder;
use smeltery::middleware::{Next, Request};
use smeltery::Response;

#[derive(Clone)]
pub struct AppConfig {
    pub name: String,
}

async fn log_requests(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let res = next.run(req).await;
    println!("{path} -> {}", res.status());
    res
}

async fn home() -> &'static str { "home" }

pub fn build(app: AppBuilder) -> AppBuilder {
    app.config(AppConfig { name: "Demo".into() })
        .middleware("log", log_requests)
        .routes(|r| {
            r.get("/", home).name("home").middleware("log");
        })
}
```

- `config(value)` / `service(value)` register a value by type; handlers take `Config<T>`, other code calls
  `app.config::<T>()` / `app.service::<T>()`.
- `middleware(alias, f)` registers route middleware; `global_middleware(f)` runs on every request;
  `middleware_family(prefix, make)` registers aliases with arguments (see [Routes and controllers](#routes-and-controllers)).
- `on_boot(|app| async move { … })` runs async setup (open connections, register services) before the server starts.
- `migrations(register)`, `seeders(register)` and `commands(register)` take the `register` functions of
  `database/migrations`, `database/seeders` and `app/commands`.
- `auth::<User>()` (one model per app: two different models stop the app at boot) makes `User` the model `Auth`
  signs in and registers the `web` guard and the `auth`, `guest`, `verified` and `password.confirm` middleware;
  `credential_listener(l)`, `second_factor(f)`, `login_completion(c)` and `login_policy(p)` register the
  password-change hooks, a second factor, the sign-in completion and a rule every new sign-in meets; `guard(g)` registers another guard (see
  [Guards and the principal](#guards-and-the-principal)).
- `hallmark(Hallmark::new())` (`smeltery::hallmark::HallmarkExt`) adds API tokens and the `auth:hallmark` guard (see
  [API tokens](#api-tokens)).
- `bellows()` (`smeltery::bellows::BellowsExt`) adds the `bellows:mcp` command (see Bellows below).

## Configuration

Config lives in typed structs under `config/`, read from `.env` with `env(key, default)`:

```rust
use smeltery::config::env;

pub struct AppConfig {
    pub name: String,
    pub debug: bool,
    pub port: u16,
}

pub fn app() -> AppConfig {
    AppConfig {
        name: env("APP_NAME", "Smeltery"),
        debug: env("APP_DEBUG", false),
        port: env("SERVER_PORT", 8000),
    }
}
```

`env` reads `String`, `bool` (`true/false`, `1/0`, `yes/no`, `on/off`), every integer type, `f32`, `f64` and `Option`
of them. A value in the real environment wins over `.env`. A value that does not parse falls back to the default and
logs a warning with the key name only. Smeltery parses `.env` itself and never modifies the process environment.

**Logging:** the app logs with `tracing` to stderr, in colour only when stderr is a terminal and `NO_COLOR` is not
set (redirected output is plain text). With `LOG_FILE` set, every line also goes to that file as plain
text (timestamp, level, target, message and fields), appended; the directory is created (on Unix the file is `0640`
and new folders `0750`). A line break inside a logged value is written as `\n` (or `\r`), so a value never starts a
line of its own, on the console or in the file. A dedicated thread writes
the file through a bounded queue, so a request never waits for the disk: when the queue is full the line is dropped
and the writer then notes in the file how many lines it dropped. The file is flushed when the app exits. Secrets are
never logged. `key:generate` and `storage:link` log to stderr only. A process running as root (`sudo`) opens
`LOG_FILE` only when every folder on its path belongs to root and no other user can write to it, and never through a
symlink at the file; otherwise it logs to stderr and says why.

The framework reads these keys:

| Key | Default | Meaning |
|---|---|---|
| `APP_NAME` | `Smeltery` | app name |
| `APP_ENV` | `production` | environment name: `local` (new apps' `.env`), `testing` (`TestApp`), `production`; a missing key means `production` |
| `APP_DEBUG` | `false` | show error details in error pages |
| `APP_URL` | `http://127.0.0.1:8000` | public URL |
| `APP_KEY` | empty | secret key (`smeltery key:generate`) |
| `SERVER_HOST` | `127.0.0.1` | bind address |
| `SERVER_PORT` | `8000` | port |
| `LOG_LEVEL` | `info` | `trace`, `debug`, `info`, `warn`, `error` |
| `LOG_FILE` | empty | a log file (relative to the app root); empty logs to stderr only. New apps set `storage/logs/smeltery.log` |
| `LOG_MAX_BYTES` | `10485760` | size at which the log file is renamed to `<file>.1` (one backup) and a new file starts |
| `REQUEST_TIMEOUT` | `30` | seconds before a request gets a 408 (at least 1) |
| `BODY_LIMIT` | `2097152` | largest request body in bytes |
| `UPLOAD_MAX_BYTES` | `10485760` | largest `multipart/form-data` request `Valid` reads (files and fields together) |
| `SHUTDOWN_TIMEOUT` | `30` | seconds to drain requests on shutdown |
| `SERVER_HEADER_TIMEOUT` | `30` | seconds a client has to send a request's headers; a connection (HTTP/1 or HTTP/2) with no request running for that long is closed |
| `SERVER_MAX_CONNECTIONS` | `4096` | most connections open at once; further clients wait to be accepted |
| `SERVER_MAX_CONNECTIONS_PER_IP` | `128` | most connections one client address holds open at once (an IPv6 client by its /64), and most requests it runs at once across them; a further connection is closed at once, a further request gets 429; peers in `TRUSTED_PROXIES` are not counted; `0` turns the limit off |
| `SERVER_MAX_STREAMS` | `32` | most requests one HTTP/2 connection runs at once (at least 1); the client waits for a free one |
| `TRUSTED_PROXIES` | empty | proxies whose `X-Forwarded-*` headers are believed: IPs and CIDR ranges, comma-separated, or `*` (see [The server](#the-server)); empty trusts nobody; a range of every address (`0.0.0.0/0`, `::/0`) stops the app at boot |
| `STATIC_CACHE_CONTROL` | `no-cache` | the `Cache-Control` header of files served from `public/` |
| `SECURITY_HEADERS` | `true` | send `X-Content-Type-Options`, `Referrer-Policy` and the frame headers with every response (see [The server](#the-server)) |
| `FRAME_OPTIONS` | `SAMEORIGIN` | who may show the app's pages in a frame: `SAMEORIGIN`, `DENY` or `off`; any other value stops the app at boot |
| `HSTS_MAX_AGE` | `0` | seconds of `Strict-Transport-Security`, sent only when `APP_URL` starts with `https://`; `0` sends none |
| `CORS_ALLOWED_ORIGINS` | empty | origins (exact `scheme://host[:port]`, comma-separated) whose pages and hybrid apps may call `CORS_PATHS` cross-origin, without credentials (see [The server](#the-server)); empty sends no CORS headers; `*` stops the app at boot |
| `CORS_PATHS` | `/api/` | path prefixes (comma-separated, each ending in `/`) the CORS rules cover |
| `PUBSUB_DRIVER` | `auto` | how messages (Sparks pushes) reach the app's other processes: `auto`, `local`, `database` or `redis` (see [PubSub](#pubsub-messages-between-processes)); any other value stops the app at boot |
| `PUBSUB_POLL_MS` | `250` | milliseconds between the `database` driver's reads (at least 10) |
| `SMELTERY_ROOT` | current directory | the app root (`public/`, `.env`, `storage/`, a relative SQLite path) |
| `DATABASE_URL` | empty | the database (see below); empty means the app has none |
| `DB_POOL_MAX` | `10` | most connections in the database pool |
| `DB_CONNECT_TIMEOUT` | `5` | seconds to open a connection or wait for a free one |
| `SESSION_DRIVER` | `cookie` | where sessions live: `cookie`, `database` or `file` |
| `SESSION_LIFETIME` | `120` | minutes an idle session lives |
| `SESSION_ABSOLUTE_LIFETIME` | `10080` | minutes a session lives at most after it started or its user signed in, however active it is (7 days); `0` = no limit |
| `SESSION_COOKIE` | `<app name in snake case>_session` | the session cookie's name (with `__Host-` in front when `APP_URL` is https) |
| `AUTH_HOME` | `/dashboard` | where the `guest` middleware sends signed-in users |
| `AUTH_VERIFICATION_EXPIRE` | `60` | minutes an email verification link is valid (at least 1) |
| `AUTH_PASSWORD_TIMEOUT` | `10800` | seconds a password confirmation lasts for the `password.confirm` middleware (at least 1) |
| `HASH_CONCURRENCY` | the number of CPUs | argon2 password hashes and checks running at once in the process |
| `HASH_QUEUE` | `64` | password hashes waiting for a turn; further ones answer 503 at once |
| `CACHE_STORE` | `database` | the default cache store: `database`, `redis`, `memcached`, `file`, `memory`, `array` or `null` |
| `CACHE_PREFIX` | `<app name in snake case>_cache_` | put in front of every cache key and lock name |
| `CACHE_PATH` | `storage/framework/cache` | the `file` store's directory (relative to the app root) |
| `CACHE_TABLE` | `cache` | the `database` store's table; locks live in `<table>_locks` |
| `CACHE_MEMORY_CAPACITY` | `10000` | most entries the `memory` store keeps |
| `CACHE_TIMEOUT` | `5` | seconds one call to the `database`, `redis` or `memcached` store may take |
| `CACHE_MAX_VALUE_BYTES` | `16777216` (16 MiB) | the largest cached value read back; a larger one is an error for `get` and a miss for `remember` (the `file`, `database` and `redis` stores check the size before reading the value) |
| `REDIS_URL` | `redis://127.0.0.1:6379` | the Redis server of the `redis` cache store, the `redis` PubSub driver and the `redis` queue driver (`rediss://` for TLS) |
| `MEMCACHED_SERVERS` | `127.0.0.1:11211` | the `memcached` store's servers, comma-separated `host:port`; the memcached protocol is not encrypted, so the servers belong on this machine or a private network |
| `TEST_CACHE_STORE` | `array` | the cache store of `TestApp` |

## Database and models

The database is set by `DATABASE_URL` in `.env`. Each backend is a Cargo feature of `smeltery`:

| Backend | Feature | `DATABASE_URL` |
|---|---|---|
| SQLite | `sqlite` | `sqlite://database/database.sqlite` (relative to the app root; the file is created when missing, its directory must exist), `sqlite:///srv/app/database.sqlite`, `sqlite::memory:` |
| PostgreSQL | `postgres` | `postgres://user:password@localhost/my_app` |
| MySQL / MariaDB | `mysql` | `mysql://user:password@localhost/my_app` (`mariadb://…` works the same) |

The app connects while it boots; when it cannot, boot fails with the reason. A URL whose backend feature is off is
an error naming the feature. Every connection and every wait for a pooled connection is bounded by
`DB_CONNECT_TIMEOUT`. Connection errors show the URL with `***` in place of the user name and password. A `/`, `?`,
`#` or `@` in the password is written percent-encoded (`%2F`, `%3F`, `%23`, `%40`).

SQLite database files run in WAL journal mode with `synchronous=NORMAL`, and a connection waits up to 5 seconds for
another one's lock before it reports "database is locked". Readers do not block the writer, so `serve` and a
`migrate` run can share the file. WAL keeps two more files next to the database (`database.sqlite-wal` and
`database.sqlite-shm`): copy all three in a backup, or use SQLite's `.backup` command. WAL needs a local file system
(not a network share). In-memory databases and `?mode=ro` URLs keep their own journal mode. Every SQLite connection
runs with `recursive_triggers` on, so `INSERT OR REPLACE` fires the delete triggers of the row it replaces. A trigger
of the app's own that writes to the table it fires on fires itself again: give it a guard, `AFTER UPDATE OF <column>`
on a column it does not write, or `WHEN new.<column> IS NOT old.<column>`. On Unix a new database
file is created readable by its owner only (`0600`), and SQLite gives the `-wal` / `-shm` files the same mode.

`smeltery::db::Db` is the handle: a pool, cheap to clone. Handlers take it as an argument (`db: Db`), other code
calls `app.db()?`. `db.execute(sql)` runs one SQL statement exactly as written, so it never holds values from a
request; `db.execute_with(sql, values)` (affected rows) and `db.query_with(sql, values)` (the rows) bind values as
parameters: `?` on SQLite and MySQL, `$1`, `$2` … on PostgreSQL. `db.backend()` names the engine, and `db.conn()` is
SeaORM's `DatabaseConnection` for anything else.

```rust,no_run
# async fn demo(db: smeltery::db::Db, email: &str) -> smeltery::Result<()> {
db.execute_with("UPDATE users SET active = ? WHERE email = ?", [true.into(), email.into()])
    .await?;
let rows = db.query_with("SELECT name FROM users WHERE id > ?", [10.into()]).await?;
for row in rows {
    let name: String = row.try_get("", "name")?;
    println!("{name}");
}
# Ok(())
# }
```

Models are [SeaORM 2.0](https://www.sea-ql.org/SeaORM/) entities. `app/models/post.rs`:

```rust
# mod post {
use smeltery::db::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "posts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub title: String,
    pub body: String,
    pub created_at: Option<DateTimeUtc>,
    pub updated_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
# }
# fn main() {}
```

`app/models/mod.rs` names it: `pub use post::Model as Post;`. `smeltery::db::prelude::*` brings the `sea_orm` crate
itself (for the SeaORM macros), SeaORM's entity prelude and query traits, `Set` / `NotSet` / `Unchanged`, the column
types (`DateTimeUtc`, `Date`, `Json`, `Uuid` …), serde's `Serialize` / `Deserialize`, and Smeltery's `Db`, `Record`
and `Found`. The serde derives expand to code that names the `serde` crate, so an app that uses them depends on it
(`cargo add serde --features derive`).

Smeltery's `Record` trait gives every model the everyday calls:

| Call | Returns |
|---|---|
| `Post::all(&db)` | every row: `Result<Vec<Post>>` |
| `Post::find(&db, id)` | `Result<Option<Post>>` |
| `Post::find_or_404(&db, id)` | the row, or a 404 `Error` |
| `Post::count(&db)` | `Result<u64>` |
| `Post::create(&db, post::ActiveModel { title: Set(..), ..Default::default() })` | the inserted row |
| `post.update(&db, \|m\| { m.title = Set(..); })` | the updated row (the closure edits the `ActiveModel`) |
| `post.delete(&db)` | `Result<()>` |
| `Post::paginate(&db, page)` | one `Page<Post>` of every row, by primary key (see [Pagination](#pagination)) |
| `Post::query()` | a SeaORM `Select` to filter, order and run |

When the model has `created_at` / `updated_at` columns, `create` sets both (unless the values set them) and `update`
sets `updated_at`, to the current UTC time.

For everything else, SeaORM's query API is available through `smeltery::db::prelude`, and its queries run on
`db.conn()`:

```rust,no_run
# mod post {
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "posts")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub title: String,
#     pub created_at: Option<DateTimeUtc>,
#     pub updated_at: Option<DateTimeUtc>,
# }
# impl ActiveModelBehavior for ActiveModel {}
# }
# use post::Model as Post;
use smeltery::db::prelude::*;

async fn latest(db: Db) -> smeltery::Result<Vec<Post>> {
    let post = Post::create(&db, post::ActiveModel { title: Set("Hello".into()), ..Default::default() }).await?;
    post.update(&db, |m| { m.title = Set("Hello again".into()); }).await?;
    Ok(Post::query()
        .filter(post::Column::Title.contains("Hello"))
        .order_by_desc(post::Column::Id)
        .limit(10)
        .all(db.conn())
        .await?)
}
# fn main() {}
```

SeaORM's `ModelTrait` is not in the prelude because its `delete` would take the place of `Record::delete`; import it
from `sea_orm` for `find_related`. Database errors convert into `smeltery::Error` with `?` (a 500, details in the log).

**Route model binding:** `Found<Post>` loads the row whose primary key is the route's last path parameter, and
answers 404 when there is none (or the parameter is not a valid key):

```rust
# mod post {
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "posts")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub title: String,
# }
# impl ActiveModelBehavior for ActiveModel {}
# }
# use post::Model as Post;
use smeltery::prelude::*;

async fn show(Found(post): Found<Post>) -> String {
    post.title
}

pub fn routes(r: &mut Router) {
    r.get("/posts/{post}", show).name("posts.show");
}
# fn main() {}
```

### Pagination

`PageQuery` is a handler argument that reads `?page=` (default 1) and `?per_page=` (default 15) from the URL. It
never rejects a request: a missing or unreadable value means the default, `page` is kept within 1 to 10,000 and
`per_page` within 1 to 100 (`page.max(250)` raises the upper bound, `page.default_per_page(30)` changes the default).
`Post::paginate(&db, page)` returns one page of every row by primary key; `smeltery::db::paginate(&db, select, page)`
returns one page of any SeaORM select (give it an order so pages do not overlap). Each runs two statements: the page
and the count.

```rust,no_run
# mod post {
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "posts")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub title: String,
#     pub published: bool,
# }
# impl ActiveModelBehavior for ActiveModel {}
# }
# use post::Model as Post;
use smeltery::db::paginate;
use smeltery::db::prelude::{ColumnTrait, QueryFilter, QueryOrder};
use smeltery::prelude::*;

async fn index(db: Db, page: PageQuery) -> Result<Json<Page<Post>>> {
    let select = Post::query()
        .filter(post::Column::Published.eq(true))
        .order_by_desc(post::Column::Id);
    Ok(Json(paginate(&db, select, page).await?))
}
# fn main() {}
```

A `Page<T>` holds `items`, `page`, `per_page`, `total` and `last_page` (at least 1), and serializes with those names
(`{"items": [...], "page": 2, "per_page": 15, "total": 47, "last_page": 4}`), so a Mold template or an Alloy page
reads it directly. `page.map(f)` changes every item; `has_next()` and `has_previous()` tell whether there are pages
around it. A page past the last one has no items. `Page::new(items, page, per_page, total)` builds one.

### Model events

A model listener is told about every successful `create`, `update` and `delete` made through `Record`. It sees what
happened (`ModelChange::Created`, `Updated` or `Deleted`), the table and the row: the saved row after a create or an
update, the row as the caller of `delete` held it. Register it with `.model_listener(…)` in `bootstrap/app.rs`:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

use smeltery::db::{Db, ModelChange, ModelEvent, ModelListener};
use smeltery::{AppBuilder, BoxFuture};

#[derive(Default)]
struct CountDeletes(AtomicU64);

impl ModelListener for CountDeletes {
    fn changed<'a>(&'a self, _db: &'a Db, event: ModelEvent<'a>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if event.change == ModelChange::Deleted && event.table == "posts" {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        })
    }
}

pub fn build(app: AppBuilder) -> AppBuilder {
    app.model_listener(CountDeletes::default())
}
# fn main() {}
```

`event.model::<Post>()` gives the row as a `Post` when the event concerns posts (`None` otherwise), and
`event.is::<Post>()` checks the model alone. A unit test calls a listener directly with
`ModelEvent::new(ModelChange::Created, "posts", &post)`.

- Listeners run after the write has succeeded, outside any transaction, in registration order, and `create` /
  `update` / `delete` wait for them before they return: a listener does quick work (or hands slow work to a queue or
  `app.spawn_owned`). A failed write tells no listener, and a `delete` of a row that is already gone deletes nothing
  and tells no listener.
- The listeners of the app's database run on a task the app owns: when the caller is cancelled while they run (a
  request timeout, a closed connection), they still finish, and shutdown waits for them like for the app's other
  owned work.
- A listener returns nothing and logs its own errors; the write it is told about stays done. A listener that panics
  is logged and the next one still runs.
- An `update` that changes nothing writes nothing and tells no listener. With an `updated_at` column every `update`
  writes.
- Only writes through `Record` are seen: raw SQL (`db.execute…`) and SeaORM's own calls (`Entity::insert`,
  `update_many`, `delete_many`) are not.
- `smeltery::db::without_listeners(async { … })` runs its future with listeners switched off for writes made in that
  task (tasks it spawns are not covered), for bulk work such as a seeder.
- Writes a listener makes through `Record` are reported to the listeners too: wrap them in `without_listeners` when
  they must not be (a listener that writes to the table it listens to would loop).
- An app without listeners does no listener work on a write.

## Migrations

Migrations live in `database/migrations/`, one file each, registered in order in `database/migrations/mod.rs` and
wired with `.migrations(database::migrations::register)` in `bootstrap/app.rs`:

```rust
# mod m2026_10_03_120000_create_posts_table {
use smeltery::db::migration::{Migration, Schema};
use smeltery::Result;

/// Creates `posts`.
pub struct CreatePostsTable;

impl Migration for CreatePostsTable {
    fn name(&self) -> &'static str {
        "2026_10_03_120000_create_posts_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("posts", |t| {
                t.id();
                t.foreign_id("user_id").constrained("users").cascade_on_delete();
                t.string("title");
                t.text("body").nullable();
                t.boolean("published").default(false);
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("posts").await
    }
}
# }
use smeltery::db::migration::Migrator;

pub fn register(m: &mut Migrator) {
    m.add(m2026_10_03_120000_create_posts_table::CreatePostsTable);
    // smeltery:migrations
}
# fn main() {}
```

The table blueprint (`t`):

| Method | Column |
|---|---|
| `t.id()` | `id`, 64-bit auto-increment primary key (`i64`) |
| `t.string(name)`, `t.string_len(name, n)` | `varchar(255)` / `varchar(n)` (`String`) |
| `t.text(name)` | `text` (`String`) |
| `t.integer(name)`, `t.big_integer(name)` | 32-bit / 64-bit integer (`i32` / `i64`) |
| `t.boolean(name)` | boolean (`bool`) |
| `t.float(name)`, `t.double(name)` | `f32` / `f64` |
| `t.decimal(name, precision, scale)` | fixed-point decimal |
| `t.date(name)` | date (`Date`) |
| `t.datetime(name)` | timestamp, with time zone where the backend has one (`DateTimeUtc`) |
| `t.json(name)` | JSON, `jsonb` on PostgreSQL (`Json`) |
| `t.uuid(name)` | UUID (`Uuid`) |
| `t.foreign_id(name)` | 64-bit integer (`i64`); `.constrained("users")` adds a foreign key to `users.id`, `.cascade_on_delete()` makes it `ON DELETE CASCADE` |
| `t.timestamps()` | nullable `created_at` and `updated_at` |

Columns are `NOT NULL` unless `.nullable()`. Every column also takes `.unique()`, `.default(value)` and `.index()`
(an index named `<table>_<column>_index`). The schema builder writes the SQL for the connected backend.

`Schema` has `create(table, |t| …)`, `table(table, |t| …)` (add columns, and with `t.drop_column(name)` drop them),
`drop(table)`, `drop_if_exists(table)`, `rename(from, to)`, `has_table(name)` and `raw(sql)`. `drop_column` drops the
column's `<table>_<column>_index` / `_unique` index first; SQLite (the bundled one supports `DROP COLUMN`) refuses a
column that is a primary key, `UNIQUE` in its `CREATE TABLE`, part of a foreign key or used by another index, view or
trigger, and MySQL refuses a column of a foreign key.

The `migrations` table records each migration's name and batch. On SQLite and PostgreSQL each migration runs in a
transaction, so one that fails leaves neither its changes nor its record; MySQL commits schema changes as they run.

| Command | Does |
|---|---|
| `smeltery migrate` | run every pending migration, as one new batch |
| `smeltery migrate:rollback` | revert the last batch; `--step N` reverts the last N migrations |
| `smeltery migrate:fresh` | drop every table in the database (on SQLite virtual tables such as an FTS5 index first), then run every migration; `--seed` then runs the seeders |
| `smeltery migrate:status` | list each migration as `Ran` (with its batch) or `Pending` |

With `APP_ENV=production`, `migrate`, `migrate:rollback`, `migrate:fresh` and `db:seed` refuse to run unless
`--force` is given.

## Seeders and factories

A seeder fills the database; `database/seeders/mod.rs` registers them, wired with
`.seeders(database::seeders::register)`:

```rust
use smeltery::db::Db;
use smeltery::db::seed::{Seeder, Seeders};

pub struct DatabaseSeeder;

impl Seeder for DatabaseSeeder {
    async fn run(&self, db: &Db) -> smeltery::Result<()> {
        db.execute("INSERT INTO settings (name) VALUES ('site')").await?;
        Ok(())
    }
}

pub fn register(s: &mut Seeders) {
    s.add(DatabaseSeeder);
    // smeltery:seeders
}
# fn main() {}
```

`smeltery db:seed` runs every seeder in order; `--class DatabaseSeeder` runs only that one.
The `DatabaseSeeder` of a new app creates a demo user (`demo@example.com`, password `password`) only when `APP_ENV`
is `local` or `testing`; in any other environment, `production` included, it creates nothing.

A factory makes rows with made-up values:

```rust,no_run
use smeltery::db::factory::{Factory, Fake};
use smeltery::db::prelude::*;
# mod app { pub mod models { pub mod post {
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "posts")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub title: String,
#     pub body: String,
# }
# impl ActiveModelBehavior for ActiveModel {}
# } } }

pub struct PostFactory;

impl Factory for PostFactory {
    type Entity = crate::app::models::post::Entity;

    fn definition(&self, fake: &mut Fake) -> crate::app::models::post::ActiveModel {
        crate::app::models::post::ActiveModel {
            title: Set(fake.sentence(4)),
            body: Set(fake.paragraph()),
            ..Default::default()
        }
    }
}

# async fn demo(db: Db) -> smeltery::Result<()> {
let post = PostFactory.create(&db).await?;
let posts = PostFactory.count(3).create(&db).await?;
let pinned = PostFactory.create_with(&db, |m| m.title = Set("Pinned".into())).await?;
let unsaved = PostFactory.make(&mut Fake::seeded(1));
# Ok(())
# }
# fn main() {}
```

`create` inserts through `Record::create` (timestamps included). `Fake` is a small seedable generator: the same seed
gives the same values. It has `word`, `words(n)`, `sentence(n)`, `paragraph`, `first_name`, `last_name`, `name`,
`email`, `unique_email`, `int(a..=b)`, `bool`, `uuid` (as text) and `date`. Each factory `create` uses the next seed
of a process-wide sequence.

## Cache

`smeltery::cache::Cache` is the app's cache: a handler argument (or `app.cache()`, or `ctx.cache()` in Watchfire
agents and jobs) on the default store, `CACHE_STORE`. Values are any `Serialize` / `Deserialize` type, stored as JSON.

```rust
use std::time::Duration;
use smeltery::prelude::*;

async fn dashboard(cache: Cache) -> Result<String> {
    let total: u64 = cache
        .remember("posts.total", Duration::from_secs(300), || async { Ok(128) })
        .await?;
    cache.put("last.visit", "now", Duration::from_secs(60)).await?;
    let hits = cache.increment("hits", 1).await?;
    let first = cache.add("welcomed", &true, Duration::from_secs(3600)).await?;
    Ok(format!("{total} posts, {hits} hits, first visit: {first}"))
}

async fn import(cache: Cache) -> Result<&'static str> {
    let lock = cache.lock("import", Duration::from_secs(120));
    if !lock.block(Duration::from_secs(5)).await? {
        return Ok("an import is running");
    }
    // … the import …
    lock.release().await?;
    Ok("imported")
}
# fn main() {}
```

| Method | Does |
|---|---|
| `get::<T>(key)` | the value, or `None` when missing or expired (an error when it does not deserialize into `T` or is larger than `CACHE_MAX_VALUE_BYTES`; the error never quotes the value) |
| `put(key, &value, ttl)` / `forever(key, &value)` | store for `ttl` (zero removes the key) / until removed |
| `add(key, &value, ttl)` | store only when the key holds no live value; `true` when stored (atomic) |
| `remember(key, ttl, \|\| async { … })` / `remember_forever` | the value, or compute, store and return it (a value of another shape is computed again) |
| `has`, `forget`, `pull` | check, remove, read and remove |
| `increment(key, by)` / `decrement(key, by)` | atomic counter; a missing key starts at 0, an entry keeps its time to live |
| `flush()` / `flush_all()` | remove every entry and lock under this cache's prefix except Watchfire's leases and schedule claims (names starting with `watchfire:`) / everything under the prefix (memcached: both empty its servers) |
| `store("redis")?` / `with_prefix(…)` | the same app's other store / another key prefix |
| `lock(name, ttl)` | an atomic lock: `get()`, `block(timeout)`, `refresh()` (the owner holds it for `ttl` again, from now), `release()` (only by its owner), `force_release()`, `owner()`; `restore_lock(name, owner, ttl)` rebuilds it elsewhere |

| Store | Where entries live |
|---|---|
| `database` (default) | the `cache` table (`key`, `value`, `expiration` in Unix ms) and `cache_locks`; new apps have the migration `create_cache_tables` (`smeltery::cache::migrations::{up, down}`). Keys of any length work: one longer than 191 characters is stored as its first 120 characters and its SHA-256. On MySQL keys compare byte for byte (`utf8mb4_bin`), so `a` and `A` are two keys. Expired rows are deleted on about one write in a hundred |
| `redis` | a Redis server (`REDIS_URL`; feature `redis` of `smeltery`): `SET NX PX` for `add` and locks, `INCRBY`, a compare-and-delete script to release a lock, `SCAN` + `UNLINK` to flush a prefix; `rediss://` uses rustls |
| `memcached` | memcached servers (`MEMCACHED_SERVERS`; feature `memcached`): native `add` / `incr` / `decr`, lock release by `cas`; counters stop at 0 and expiry counts whole seconds |
| `file` | `storage/framework/cache/<2 hex>/<sha256 of the key>` (`CACHE_PATH`; on Unix files `0600`, folders `0700`); writes go through a temp file and a rename, `add` / counters / locks hold a per-key lock file, so processes on one machine share it; `flush` removes only files of that shape |
| `memory` | this process's memory, up to `CACHE_MEMORY_CAPACITY` entries (moka); shared by every app in the process, not between processes |
| `array` | the memory of one app; `TestApp` uses it (`TEST_CACHE_STORE` picks another) |
| `null` | nothing: reads miss, `add` answers `false`, `increment` answers its argument, every lock is granted |

Redis is the recommended store in production: one server shared by every process and machine, with native atomic
operations and locks. Memcached is the second choice; its client is synchronous and runs on blocking threads. Calls
to the database, Redis and memcached stores time out after `CACHE_TIMEOUT` seconds. `CACHE_PREFIX` keeps apps that
share a store apart. Locks on the database, Redis and memcached stores exclude every process using the store, e.g.
`serve` and `work` running the same job; locks on the file store exclude the processes of one machine (or of a disk
they share). The database and file stores remove expired entries on about one write in a hundred.
`smeltery cache:clear [store]` empties a store from the command line and keeps the leases and schedule claims
Watchfire holds there, so no singleton agent starts in a second process and no tick runs twice. `cache:clear --all`
removes those too; the processes take their agents again within `WATCHFIRE_LEASE_TTL` (see "Several processes"),
and meanwhile a singleton agent can run in two processes and a tick of the current minute can run again. Memcached
cannot keep some keys while it flushes, so `cache:clear memcached` asks for `--all`. Memcached sends everything
unencrypted: run it on the app's machine or a private network.

An app whose cache tables were created on MySQL before `mysql_statements` existed adds a migration that runs them
(byte-for-byte keys, `MEDIUMTEXT` values):

```rust
use smeltery::Result;
use smeltery::db::Backend;
use smeltery::db::migration::{Migration, Schema};

pub struct CacheKeysBinary;

impl Migration for CacheKeysBinary {
    fn name(&self) -> &'static str {
        "2026_10_05_000000_cache_keys_binary"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        if schema.backend() == Backend::MySql {
            for sql in smeltery::cache::migrations::mysql_statements("cache") {
                schema.raw(&sql).await?;
            }
        }
        Ok(())
    }

    async fn down(&self, _schema: &Schema) -> Result<()> {
        Ok(())
    }
}
```

### Rate limiting

`smeltery::cache::RateLimiter` counts hits per key in fixed windows in the app's cache store, the engine of
`throttle:`. `RateLimiter::new(name, max, window)` allows `max` hits per key per window (a window under a second
counts as one second; `max` 0 refuses every hit); limiters with different names count apart.
`limiter.hit(&app, key).await?` counts one hit and answers `RateLimit::Allowed { remaining }` or
`RateLimit::Limited { retry_after }` (seconds until the window ends; `.allowed()` tells which), one atomic `add` +
`increment`, so parallel hits never get more than `max` through. A cache error is returned, and the caller refuses
what it guards. `limiter.clear(&app, key).await?` forgets the key's hits in the current window.
`limiter.peek(&app, key).await?` reads the key's state without counting a hit (`Allowed { remaining }` or `Limited
{ retry_after }`): a read only, so parallel callers may all see `Allowed`; it refuses a blocked key early, and the
work itself is guarded by `hit`. With
`CACHE_STORE=null` the counts live in the app's process memory. Limiters with the same name, `max` and window share
their counts in every case.

```rust
use std::sync::LazyLock;
use std::time::Duration;
use smeltery::cache::RateLimiter;
use smeltery::prelude::*;

static CODES: LazyLock<RateLimiter> =
    LazyLock::new(|| RateLimiter::new("codes", 5, Duration::from_secs(300)));

async fn check_code(app: &App, user_id: i64) -> Result<bool> {
    Ok(CODES.hit(app, &format!("user:{user_id}")).await?.allowed())
}
# fn main() { let _ = check_code; }
```

## PubSub: messages between processes

`smeltery::pubsub::PubSub` carries messages between the processes of one app (Sparks' `Broadcast` sends through
it). Every app has one: `PubSub::of(&app)`, or `app.service::<PubSub>()`. A message has a topic and a JSON payload
of at most 64 KiB:

```rust,no_run
use serde_json::json;
use smeltery::pubsub::PubSub;
use smeltery::{App, Result};

async fn watch_prices(app: App) -> Result<()> {
    let pubsub = PubSub::of(&app).expect("every app has one");
    let mut prices = pubsub.subscribe("prices");
    pubsub.publish("prices", &json!({ "symbol": "ORE", "price": 7 })).await?;
    while let Ok(message) = prices.recv().await {
        println!("{} (from another process: {})", message.payload, message.remote);
    }
    Ok(())
}
# fn main() {}
```

`publish` delivers to the subscribers in this process at once and waits for the driver to take the message for the
other processes (it returns the driver's error). `forward(topic, &payload)` sends to the other processes only and
never waits: the message goes into a queue of 1024 that one task sends on; a full queue or a driver error drops it,
counts it (`dropped()`) and logs it at most once a minute. At shutdown the queue keeps taking messages while they
come (for work that stops on the same signal), then sends what is left within 2 s; a message forwarded after that,
or left unsent, is counted as dropped too. `subscribe(topic)` receives the topic's messages from
every process. Each topic has its own buffer of 1024 messages in a process, so a burst on one topic never makes the
subscribers of another fall behind; a subscriber more than 1024 messages behind its topic gets `RecvError::Lagged`
and then receives again (past 256 subscribed topics in one process, further topics share one buffer; the
framework's topics `auth`, `anvil` and `sparks` always have their own). `publish` answers an error and `forward`
drops the message for the topics `auth` and `anvil`: auth events go through `smeltery::auth::publish_event`, Anvil's
events through `Anvil`.
Delivery is at most once and in order per publishing process: a message sent while the database or Redis is down is
lost, so a page or client reloads its state after a reconnect.

`PUBSUB_DRIVER` decides how messages reach the other processes. The process logs the driver it chose at start.

| `PUBSUB_DRIVER` | Messages reach |
|---|---|
| `auto` (default) | decided by what the process runs: `serve` with its background work uses `local`; `serve --no-agents` (or `WATCHFIRE_IN_SERVE=false`, or `ANVIL_IN_SERVE=false`), `work` and Anvil's `anvil` process use `redis` when `CACHE_STORE=redis` (feature `redis`), else `database` when the app has a database, else `local` with a warning. Console commands and `TestApp` use `local` |
| `local` | this process only |
| `database` | every process on the app's database: the `pubsub_messages` table, which each process reads every `PUBSUB_POLL_MS` (default 250) while something in it subscribes (a web process with Sparks always does) |
| `redis` | every process on the Redis server of `REDIS_URL` (feature `redis`): `PUBLISH` / `SUBSCRIBE` on the channel `<CACHE_PREFIX>pubsub` |

Several `serve` processes behind a load balancer each count as one process under `auto`, and so does a `serve` that
runs its background work next to an extra `work` process: give them `PUBSUB_DRIVER=database` or `redis`. An explicit `database` or `redis` driver that lacks its database, `APP_KEY` or
feature stops `serve` and `work` from starting. `TestApp` uses `local` unless `TEST_PUBSUB_DRIVER` names another
driver. Messages that leave the process are encrypted and authenticated with AES-256-GCM under a key derived from
`APP_KEY`, so every process needs the same `APP_KEY`; a message sealed under another key, or sent more than 5
minutes earlier (by the sender's clock; a message written into the table or Redis again later), is skipped, counted
and logged, so the clocks of the app's machines must agree within 5 minutes (NTP). Each message carries a random id,
and a process delivers each id once: a message written into the table or Redis again within those 5 minutes is
skipped and logged too. A payload longer than the largest sealed message is skipped without being read. Every
process of the `database` driver deletes rows older than a minute, and rows dated more than 5 minutes ahead, every
10 s (in statements of 500 rows, for at most 2 s each time); its read position never runs ahead of the database's
clock (when the clock steps back, the position follows it and the process logs it). The `redis` driver checks its
subscriber connection with `PING` every 30 s and reconnects when a `PING` goes unanswered for 2 s, so a connection
dropped silently by a NAT or load balancer is replaced.

New apps with Watchfire (headless apps, and web apps with the `watchfire` building block) and web apps with the
`anvil` building block have the migration of the `pubsub_messages` table (`smeltery::pubsub::migrations::{up, down}`; `MEDIUMTEXT` payloads on MySQL).
`smeltery pubsub:install` adds the same migration to an app without it; run `smeltery migrate` after it.

## Encryption and random tokens

`smeltery::crypto` holds the helpers every part of Smeltery uses: `random_token(len)` (letters and digits from the
OS random source, every character equally likely), `random_bytes(len)`, `sha256_hex(text)` and
`constant_time_eq(a, b)` (compares two secrets of equal length in constant time).

`app.derive_key(purpose)` is a 32-byte key derived from `APP_KEY` for `purpose`: HMAC-SHA256 of `APP_KEY` over
`smeltery-app-key:<purpose>`, a label that no signature (`app.sign`) and no key the framework uses for its own
messages share, so none of them is ever such a key. Every purpose gives an unrelated key; Smeltery's own crates use
dotted purposes (`anvil.secret`, `anvil.key`), so plain names are free for apps.
`app.encrypt(purpose, aad, plaintext)` encrypts and authenticates a value with AES-256-GCM under a key of its own
for `purpose` (HMAC-SHA256 of `APP_KEY` over `smeltery-app-enc:<purpose>`, never a `derive_key` key, so a derived key
handed out for a purpose never opens values encrypted under it) and a random 96-bit nonce, as base64 of nonce,
ciphertext and tag. `aad` is authenticated but not stored:
`app.decrypt(purpose, aad, &sealed)` returns `Ok(Some(plaintext))` only for the same purpose, `aad` and `APP_KEY`,
and `Ok(None)` for anything edited, truncated or sealed otherwise. Both fail without a usable `APP_KEY`. Pass the
id of the record a secret belongs to as `aad`, so a value copied to another row does not open there:

```rust
use smeltery::prelude::*;

fn seal_secret(app: &App, user_id: i64, secret: &str) -> Result<String> {
    app.encrypt("api-secrets", user_id.to_string().as_bytes(), secret.as_bytes())
}

fn open_secret(app: &App, user_id: i64, sealed: &str) -> Result<Option<String>> {
    Ok(app
        .decrypt("api-secrets", user_id.to_string().as_bytes(), sealed)?
        .and_then(|bytes| String::from_utf8(bytes).ok()))
}
# fn main() { let _ = (seal_secret, open_secret); }
```

Rotating `APP_KEY` makes every encrypted value unreadable.

## Console commands

The app binary runs these commands (the `smeltery` command forwards them):

| Command | Does |
|---|---|
| `serve` | start the HTTP server (the default), with the app's Watchfire agents, queue workers and scheduler (`--no-agents`: without them) |
| `work` | run the Watchfire agents, queue workers and scheduler without the HTTP server |
| `route:list` | list every route |
| `migrate`, `migrate:rollback`, `migrate:fresh`, `migrate:status` | migrations (see above) |
| `db:seed` | run the seeders |
| `cache:clear [store]` | remove every entry of the default cache store, or of the named one, except Watchfire's leases and schedule claims (`--all`: those too) |
| `agents:list` | the running app's agents with state, health, restarts, runs, last heartbeat, next restart |
| `agents:start`, `agents:stop`, `agents:pause`, `agents:resume`, `agents:restart` `<name>` | control an agent of the running app |
| `agents:logs <name>` | an agent's last log lines, from the running app |
| `agents:token` | print the Watchfire API token (derived from `APP_KEY`) for other API clients |
| `agents:runs <name>` | an agent's latest runs from the database (`--limit N`, default 20) |
| `schedule:list` | the scheduled tasks with their next run |
| `schedule:run` | run the scheduled tasks due this minute once (for system cron) |
| `help` | list every command, the app's own included |

An app adds its own in `app/commands/`, registered in `app/commands/mod.rs` and wired with
`.commands(app::commands::register)`:

```rust
use smeltery::console::{Args, Command, Commands};
use smeltery::{App, Result};

pub struct Greet;

impl Command for Greet {
    fn name(&self) -> &'static str {
        "greet"
    }

    fn about(&self) -> &'static str {
        "Say hello"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        println!("Hello, {}!", args.get(0).unwrap_or("world"));
        Ok(())
    }
}

pub fn register(c: &mut Commands) {
    c.add(Greet);
    // smeltery:commands
}
# fn main() {}
```

`Args` gives positional words with `args.get(0)`, flags with `args.flag("force")` and values with
`args.value("step")` (`--step 2` or `--step=2`). An option followed by a word that does not start with `-` takes it
as its value, so flags go after positional words (`greet Ada --loud`). A built-in command's name always runs the
built-in.

## Sessions

Every route from `routes/web.rs` runs inside the session middleware; `/api` routes do not. A handler takes the
session as an argument:

```rust
use smeltery::prelude::*;

async fn save(session: Session) -> Redirect {
    session.insert("theme", "dark");
    session.flash("status", "Settings saved.");
    Redirect::to("/settings")
}

async fn show(session: Session) -> String {
    session.get::<String>("theme").unwrap_or_default()
}
# fn main() {}
```

| Call | Does |
|---|---|
| `get::<T>(key)` | the value, if present and of type `T` |
| `insert(key, value)`, `remove(key)`, `has(key)` | store, drop, check (any `Serialize` value) |
| `flash(key, value)` | store for the next request only; `reflash()` keeps this request's flash values one more |
| `regenerate()` | a new session id, same data (done on login) |
| `invalidate()` | a new session id, no data (done on logout) |
| `token()` | the CSRF token |
| `errors()`, `old(field)` | validation errors and input flashed by the previous request |
| `smeltery::session::run_web_stack(app, req, next)` (middleware) | the session stack of web routes around any route: the same code, CSRF check included |
| `flash_errors(&app, &errors, &input)` | flash validation errors and the form's input for the next request, as a failed validation does (the input filtered the same way: no secret-looking or `dont_flash` field, at most 100 fields); answer with a redirect to any page after it |

With `SESSION_DRIVER=cookie` (the framework's default; new apps' `.env` sets `database`) the whole session is
stored in one cookie, encrypted and authenticated with
AES-256-GCM under a key derived from `APP_KEY` (HKDF-SHA256). A browser keeps about 4 KB per cookie; a larger
session logs a warning. With `SESSION_DRIVER=database` the cookie holds only the encrypted session id and the data
lives in the `sessions` table (`id`, `payload`, `last_activity`); expired rows are deleted on about one request in a
hundred. With `SESSION_DRIVER=file` the cookie holds the encrypted session id and the data lives in
`storage/framework/sessions/<id>`, one file per session, readable by its owner only; the file's modification time is
the session's last activity, each save writes a temp file and renames it over the old one, and expired files are
deleted on about one request in a hundred. Cookies are `HttpOnly`, `SameSite=Lax` and `Path=/`. When `APP_URL`
starts with `https://` (in any letter case) they are `Secure`, and the session and remember-me cookies carry the
`__Host-` prefix (`__Host-my_app_session`), which browsers accept only from this host over HTTPS, so neither a
subdomain nor a plain-HTTP page can plant one. A missing, tampered or expired session cookie gives a new, empty
session.

A session lives `SESSION_LIFETIME` minutes after its last request, and at most `SESSION_ABSOLUTE_LIFETIME` minutes
(7 days) after it started or its user signed in, however active it is; a remember-me cookie then signs the user in
again. A visitor who came without a session and whose request stored nothing (no value, no flash message, no CSRF
token) gets none: no cookie, no row, no file. A page rendered from a view gets one, since its forms need the CSRF
token, and so does every response of an app that sends the `XSRF-TOKEN` cookie (Alloy).

`APP_KEY` must hold at least 32 bytes (`base64:…` as `smeltery key:generate` writes it, or plain text; in
production a plain-text key logs a warning at boot, since a typed phrase is far easier to guess than 32 random
bytes). Without a usable one (missing, malformed or too short), the server of an app with web routes refuses to start
and says to run `smeltery key:generate`. `APP_ENV=testing` with an empty `APP_KEY` signs with a fixed test key that
is public; `serve` and `work` refuse to run under `APP_ENV=testing` (see [The server](#the-server)), so it serves
only `TestApp` and other in-process tests.

## CSRF

On web routes, every request whose method is not `GET`, `HEAD`, `OPTIONS` or `TRACE` (`POST`, `PUT`, `PATCH`,
`DELETE`, and any other method a `Router::any` route answers) must carry the session's token in a `_token` field
(URL-encoded or multipart form) or an `X-CSRF-TOKEN` header. A multipart body is read only up to its `_token` field
(at most `BODY_LIMIT` bytes; `@csrf` comes first in a form), and the rest streams on to the handler. `@csrf` in a Mold form writes the field.
`@csrf`, `csrf_token()` and the Sparks `csrf-token` meta tag show the token masked with a fresh random pad in every
response (`session.csrf_token()`), so the page's bytes never repeat it and a compressed HTTPS page cannot leak it
(BREACH); every masked form verifies, and so does the unmasked `session.token()`. Never put `session.token()` in a
page or response: render `csrf_token()` (or `session.csrf_token()?` in Rust) instead. A missing or
wrong token answers 419 "Page Expired" (`{"error": "CSRF token mismatch"}` for JSON clients). API routes are not
checked. Under `APP_ENV=testing` the check is off; `TestApp::new(build).with_csrf()` turns it on for a test.
Signing in and signing out give the session a new token, so a token from before is refused (a form left open in
another tab answers 419 once).

## Validation

`#[derive(Validate)]` puts rules on a form struct, and the `Valid<T>` extractor reads, checks and hands it over:

```rust
use serde::Deserialize;
use smeltery::prelude::*;

#[derive(Deserialize, Validate)]
pub struct RegisterForm {
    #[validate(required, max = 255)]
    pub name: String,
    #[validate(required, email, unique(table = "users", column = "email"))]
    pub email: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: String,
    #[validate(integer, between(1, 120))]
    pub age: Option<i64>,
}

async fn store(session: Session, Valid(form): Valid<RegisterForm>) -> Redirect {
    session.flash("status", format!("Welcome, {}!", form.name));
    Redirect::to("/")
}
# fn main() {}
```

| Rule | Passes when | Message |
|---|---|---|
| `required` | present and not blank | The name field is required. |
| `email` | an e-mail address | The email field must be a valid email address. |
| `url` | an `http`/`https` URL | The site field must be a valid URL. |
| `min = n`, `max = n`, `between(a, b)` | text: characters; numbers: the value; lists: items; uploaded files: kilobytes | The password field must be at least 8 characters. / The name field must not be greater than 255 characters. |
| `mimes = "png,jpg"` | an uploaded file whose name has one of the extensions | The image field must be a file of type: png, jpg. |
| `numeric`, `integer` | a number / a whole number | The age field must be an integer. |
| `alpha`, `alpha_num`, `alpha_dash` | letters / letters and digits / also `-` and `_` | The code field must only contain letters. |
| `in_list("a", "b")` | one of the values | The selected color is invalid. |
| `confirmed` | equals `<field>_confirmation` | The password field confirmation does not match. |
| `same = "other"` | equals field `other` | The email field must match other. |
| `accepted` | `yes`, `on`, `1`, `true` | The terms field must be accepted. |
| `unique(table = "users", column = "email")` | no row has the value | The email has already been taken. |
| `exists(table = "users", column = "id")` | a row has the value | The selected manager id is invalid. |

`message = "…"` replaces the messages of that field. `unique(…, except_id)` ignores the row whose `id` is the route's
last path parameter (an edit form); `except_id = "field"` takes the id from a field of the struct. `unique` and
`exists` query the app's database. In messages, `_` in a field name becomes a space.

`Valid<T>` reads a URL-encoded or multipart form, or JSON when the request's content type is JSON. Fields holding an empty string
are dropped before deserializing, so an empty input is absent: an `Option` field is `None`, and every rule except
`required` skips an absent field. When the input does not deserialize at all, the messages of the `required`,
`email`, `url`, `numeric` and `integer` rules are reported for every field at once.

**File uploads.** A form with `enctype="multipart/form-data"` reaches the same `Valid<T>`. A file field is an
`Option<UploadedFile>` (`smeltery::http::UploadedFile`); text fields deserialize as in a URL-encoded form:

```rust
use serde::Deserialize;
use smeltery::http::UploadedFile;
use smeltery::prelude::*;

#[derive(Deserialize, Validate)]
pub struct PhotoForm {
    #[validate(required, max = 255)]
    pub title: String,
    #[validate(required, max = 2048, mimes = "png,jpg")]
    pub image: Option<UploadedFile>,
}

async fn store(Valid(form): Valid<PhotoForm>) -> Result<Redirect> {
    if let Some(image) = &form.image {
        let path = image.store("public/photos").await?; // "public/photos/<random>.png"
    }
    Ok(Redirect::to("/photos"))
}
# fn main() {}
```

On a file, `required` passes when a file was sent (a file input left empty is absent), `min`, `max` and `between`
count kilobytes, and `mimes = "png,jpg"` checks the file name's extension (`jpg` and `jpeg` are one). Each file
streams to a temp file in `storage/framework/uploads/`, which is deleted when the `UploadedFile` is dropped unless it
was stored. Before any rule runs, the first bytes are checked against the declared type and the extension: PNG,
JPEG, GIF, WebP and PDF content must be declared and named as that type, and a file declared or named as one of them
must contain it; otherwise the field fails with "The image field must be a file whose content matches its type.". A
multipart request may be `UPLOAD_MAX_BYTES` (default 10 MB) in all; a larger one fails validation on the field being
read ("must not be greater than … kilobytes"), and its text fields together stay within `BODY_LIMIT` (413 past it).
The whole body, boundaries and part headers included, may be at most 1 MB more than `UPLOAD_MAX_BYTES`, and hold at
most 1000 parts (413 past either).
`name()` is the browser's file name without directories and with unsafe characters replaced, `size()` is in bytes,
`mime()` is the type read from the content (else the declared one), and `bytes()` reads the file. `store(dir)` moves
it to `storage/app/<dir>/<40 random characters>.<extension>` and returns that path relative to `storage/app`. The
extension follows the content for PNG, JPEG, GIF, WebP and PDF files (a `photo.exe` holding a PNG is stored as
`.png`), else it is the name's own when that is on the list of safe extensions, in lower case: images (`png`, `jpg`,
`jpeg`, `gif`, `webp`, `avif`, `bmp`, `ico`, `tif`, `tiff`, `heic`, `heif` …, not `svg`), audio (`mp3`, `wav`, `ogg`,
`m4a`, `flac` …), video (`mp4`, `webm`, `mov` …), `pdf`, `txt`, `csv`, `tsv`, `rtf`, office documents (`docx`,
`xlsx`, `pptx`, `odt` …) and archives (`zip`, `gz`, `7z` …). Every other extension (`html`, `svg`, `xml`, `xsd`,
`js`, an unknown one) is stored as `.bin`. `smeltery::http::is_safe_extension(ext)` tells whether an extension is on
the list and `stored_extension(ext)` gives the one `store` uses. `mimes` checks the name
only, so an image field lists its types in `mimes`, and the content check above covers those five types.
`store_as(dir, name)` picks the name itself. `dir` starts with `public` (served at `/storage/…` after
`smeltery storage:link`) or `private`, and every segment of `dir` and the name uses only `[A-Za-z0-9._-]` (no `.`,
`..` or leading dot in the name). A file field in a URL-encoded or JSON body is invalid. Failed validation flashes
the text fields as old input, never the files.

When validation fails, JSON clients (an `Accept` or `Content-Type` of JSON) get 422
`{"message": "The given data was invalid.", "errors": {"email": ["…"]}}`. On web routes other requests are
redirected back (303 to the `Referer` when it is on this site, else `/`) with the errors and the input flashed;
fields that look like secrets are not flashed: names holding `password`, `passwd`, `passphrase`, `passcode`,
`secret`, `token`, `apikey`, `privatekey`, `accesskey`, `cardnumber`, `creditcard`, `securitycode`, `twofactor` or
`recoverycode` once everything but letters and digits is left out (`private_key`, `privateKey` and `private-key` all
count), names with one of the words `key`, `pin`, `otp`, `totp`, `mfa`, `2fa`, `tfa`, `auth`, `card`, `cvv`, `cvc`,
`cvv2`, `iban`, `ssn`, `answer` or `recovery` (words split at anything but letters and digits and at camelCase, so
`mfaCode` and `security_answer` count; letter case ignored), and the fields named with
`AppBuilder::dont_flash(&["nickname"])`. A value over 16 KB is not flashed, nor more than 100 fields or 64 KB
in all. The next page shows them with `@error("email") {{ message }} @enderror` and
`old("email")`. A handler can fail the same way with `Err(Error::validation("email", "…"))`.

`ValidationErrors` maps fields to messages: `first(field)`, `get(field)`, `has(field)`, `is_empty()`, and it
serializes as `{"field": ["message"]}`.

## Flash messages

`session.flash("status", "Post saved.")` followed by a redirect shows the message on the next page only:

```text
@if(session("status"))
  <p class="status">{{ session("status") }}</p>
@endif
```

`Back` is the previous page as a handler argument: `back.redirect()` answers 303 to the `Referer` when it is on this
site, otherwise to `/`.

## Authentication

The user model implements `smeltery::auth::Authenticatable`, and `bootstrap/app.rs` registers it with
`.auth::<User>()`:

```rust
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "users")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub password: String,
#     pub remember_token: Option<String>,
#     pub credentials_epoch: Option<i64>,
# }
# impl ActiveModelBehavior for ActiveModel {}
impl smeltery::auth::Authenticatable for Model {
    fn auth_id(&self) -> i64 { self.id }
    fn password_hash(&self) -> &str { &self.password }
    fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
    // Optional: a nullable `credentials_epoch` column lets `auth::end_credentials` end open sessions.
    fn credentials_epoch(&self) -> Option<i64> { self.credentials_epoch }
}
# fn main() {}
```

Users are found by id and by their `email` column; remember-me tokens are stored in `remember_token`. An address is
compared trimmed and with its ASCII letters in lowercase (`smeltery::auth::normalize_email`), and a row counts only
when its stored address equals the typed one apart from ASCII letter case, so a database collation that also folds
accents (MySQL's and MariaDB's defaults) never matches a look-alike address. Handlers on web routes take `Auth`:

| Call | Does |
|---|---|
| `auth.check()`, `auth.id()` | whether someone is signed in, and who |
| `auth.user::<User>().await?` | the signed-in user (loaded once per request) |
| `auth.attempt(email, password, remember).await?` | `true` and signed in when the password matches and the login policies allow it (`validate` + `app.check_login` + `login`); it never asks a second factor, so with a login completion registered (below) it refuses with an error, like `login` |
| `auth.validate(email, password).await?` | the user (`AuthUser`) when the password matches, without signing in; the same budgets as `attempt` |
| `auth.login(&user, remember).await?` | sign in a user (a new session id and CSRF token); an error while a login completion is registered (sign in through `app.login_completion()`, whose code uses `auth.login_user(&user, remember)`; `AuthUser::of(&user)` wraps a model) |
| `auth.logout().await?` | sign out this session: it is invalidated (the `database` and `file` drivers delete it), the remember-me cookie removed and the user's remember token replaced with a new random one, so no remember-me cookie of the user signs in any more; other devices stay signed in |
| `auth::cycle_remember_token(&app, user_id).await?` | replace the user's remember token with a random one (only its SHA-256 stored): no remember-me cookie of the user signs in any more; sessions stay |
| `auth.logout_other_devices(password).await?` | sign out every other session of the user: `password` (the current one) is hashed again with a fresh salt, and the remember token is replaced, so no remember-me cookie signs a device in again; `false` and nothing changes when it is wrong. It counts against `confirm_password`'s five tries a minute |
| `auth.set_password(new).await?` | store a new password for the signed-in user and keep this session signed in: every other session ends, the remember token is replaced; `false` for a guest. Check the current password first |
| `auth.confirm_password(password).await?` | `true` when the signed-in user's password matches, and the session remembers the time; five tries a minute per user, then 429 / a redirect back with the message on `password` |
| `auth.password_confirmed_within(duration)` | whether this session confirmed the password within `duration` |
| `auth.intended(default)` | a 303 to the page the `auth` middleware turned the visitor away from, else to `default`; the remembered page is forgotten |
| `auth.set_intended(path)` | remember a page for `intended`: `false` (nothing remembered) unless `path` is a path on this site of at most 2048 bytes |

With `remember`, a remember-me cookie (`remember_<session cookie>`, encrypted, five years) holds the user id and a
random 60-character token whose SHA-256 is stored in `users.remember_token`; a request without a signed-in session
but with a valid cookie is signed in from it.

A signed-in session is bound to the user's password hash. When it changes (a
password change or reset, `logout_other_devices`), every session made before is signed out on its next request, a
copied session cookie included. The check reads the user's row on each request of a signed-in user. With
`SESSION_DRIVER=cookie` nothing is stored on the server, so a copy of a session cookie made before `logout` stays
valid until the session expires (`SESSION_LIFETIME` idle, `SESSION_ABSOLUTE_LIFETIME` at most); with `database` or
`file` the logout deletes the session, which is why new apps use the `database` driver.

Every attempt counts before the account is looked up and the password checked, so parallel requests get no extra
tries: thirty a minute from one client whatever the address (`auth.ip()`, the client address after
`TRUSTED_PROXIES`; an IPv6 client by its /64), five a minute for one address from one client, and twenty per five
minutes for one address from one network (an IPv4 /24, an IPv6 /48). Addresses with and without an account count
alike, so the limit tells nothing about which addresses are registered, and guesses from other networks never lock
the account's owner out. A successful sign-in resets the address's counts and gives its attempt back to the client's.
Past a limit `attempt` returns an error that JSON clients get as 429 and web forms as a redirect back with "Too many
login attempts. Please try again in N seconds." on `email`. The counts live in the process's memory, in fixed
windows that start with the first attempt.

The `auth` middleware sends guests to the route named `login` (or `/login`) with a 303, and answers 401 to JSON
clients. The `guest` middleware sends signed-in users to `AUTH_HOME`.

Before that redirect, the `auth` middleware remembers the page in the session when the request is a `GET` page visit:
not from a JSON client, not a background `fetch` / XHR, not a subresource (an image, script, stylesheet or frame,
told apart by the browser's `Sec-Fetch-Mode` and `Sec-Fetch-Dest` headers), not a prefetch and not an event stream;
Inertia visits count. It stores the path and query, never the host, at most 2048 bytes. Signing in keeps it,
signing out drops it. A login handler answers with `auth.intended(…)` after a successful `attempt`, so the user
lands on the page they asked for:

```rust
use serde::Deserialize;
use smeltery::prelude::*;

#[derive(Deserialize)]
struct LoginForm {
    email: String,
    password: String,
}

async fn login(app: App, auth: Auth, Form(form): Form<LoginForm>) -> Result<Redirect> {
    if auth.attempt(&form.email, &form.password, false).await? {
        return Ok(auth.intended(&app.settings().auth_home));
    }
    Ok(Redirect::to("/login"))
}
# fn main() {}
```

`intended` only redirects to a path on this site: one that starts with a single `/` and holds only visible ASCII
characters other than `\` (browsers read `//host`, `/\host` and `/<tab>/host` as another host). Anything else in the
session gives `default`.

`smeltery::auth::hash_password(password)` hashes with argon2id (default parameters) and `verify_password(password,
hash)` checks one, both on a blocking thread. At most `HASH_CONCURRENCY` (default: the number of CPUs) hashes and
checks run at once in the process and up to `HASH_QUEUE` (64) more wait; further ones fail at once with 503, so a
flood of sign-ins, registrations or resets cannot exhaust the memory.

`auth::verify_credentials(&app, client_ip, email, password).await?` is the check behind `validate` and `attempt` for
code without a session (issuing an API token): the same three budgets (shared with `attempt`), the same dummy-hash
timing for unknown addresses, `Some(AuthUser)` on a match. `auth::find_by_email::<User>(&app, email).await?` finds a
user the way they do. `#[serde(deserialize_with = "smeltery::auth::deserialize_email")]` on a form's email field
reads it trimmed and in lower case. `auth::model_column::<User>("email")` is a column of the model by name, and
`app.auth_model()` the `TypeId` of the model registered with `.auth::<User>()`.

**Password changes:** sessions, remember-me cookies and other credentials bound to the password hash end when it
changes. `auth.set_password(new)` keeps the current session; app code that writes `password` itself calls
`auth::password_changed(&app, user_id, except).await?` afterwards (`except`: the principal of the request, whose
credential stays), which replaces the remember token. A password reset, `logout_other_devices`, `set_password` and
`password_changed` then run the app's credential listeners (`AppBuilder::credential_listener`, a
`smeltery::auth::CredentialListener` told the user id, why (`CredentialChange::{Reset, Changed,
OtherDevicesLoggedOut, Ended}`), the credential that stays, and for a reset whether it verified a previously unverified
address (`was_unverified`)), before the request answers. Every listener runs and the event below is published even
when one fails; then the first error fails the request, the password stays changed. `password_changed` refuses an
`except` principal of another user. Keep a credential only after checking the current password. Each also
publishes one `smeltery::auth::AuthEvent` on the
[PubSub](#pubsub-messages-between-processes) topic `auth` (`auth::EVENTS_TOPIC`): `RevokedAll { user_id, kind,
except }` (as JSON `{"type":"revoked_all",…}`), and a logout publishes `Revoked { user_id, key }` for the session
it ended (`key` as `principal.key()` names it). `auth::publish_event(&app, &event)` publishes one. Delivery is at
most once, as every PubSub message.

**Password confirmation:** the `password.confirm` middleware (registered by `.auth::<User>()`, put it after `auth`)
lets a session through that confirmed its password with `auth.confirm_password(password)` within
`AUTH_PASSWORD_TIMEOUT` seconds (default 10800). Otherwise JSON clients and API routes get 423 `{"message": "Password
confirmation required."}`, and a page visit is remembered (as `auth` does) and sent to the route named
`password.confirm` (or `/user/confirm-password`) with a 303. Every sign-in and sign-out removes the confirmation
with the other session keys a sign-in owns (names starting with `_auth.` or `_temper.`).

**Second factor:** `AppBuilder::second_factor(f)` registers a `smeltery::auth::SecondFactor` (`required(app, user)`,
`verify(app, user, code)`); `app.second_factor()` returns it. `verify` answers a `SecondFactorVerdict`: `Valid`,
`Invalid`, or `TooManyAttempts { retry_after }` when the account's code budget is spent (the code is then not
checked). `verdict.into_result("code", message)?` turns it into the endpoint's answer: `Ok(())`, 422 with `message`
on `code`, or 429 with `Retry-After` and "Too many attempts. Please try again in N seconds." on `code`.

**Login policy:** `AppBuilder::login_policy(p)` adds a `smeltery::auth::LoginPolicy`, a rule without a session that
every new sign-in meets: `check(app, user)` answers `LoginDecision::Allow` or `LoginDecision::refuse(status,
message)` (a suspended account, an IP rule). `app.check_login(&user).await?` asks every policy in the order they
were added (none: allowed); the first refusal is an error with the policy's status and its message on `email` (JSON
clients get the status, browsers are sent back with the message). `auth.attempt`, Temper's login, its two-factor
challenge and registration, and Hallmark's token endpoint ask it after the password and before the second factor.
A remember-me cookie meets it too: a refused restore stays a guest, the cookie is removed and the user's remember
token replaced. Policies gate new sign-ins, token issuance and remember-me restores only: refusing a user does not
end an open session, issued API tokens or open sockets; `auth::end_credentials` (below) ends them.

**Ending a user's credentials:** `auth::end_credentials(&app, user_id).await?` signs a user out everywhere without
a password change (a suspension, together with a login policy that refuses them): it adds one to the user's
`credentials_epoch`, so every open web session of the user is signed out on its next request; it replaces the
remember token, runs the credential listeners with `CredentialChange::Ended` (Hallmark deletes every token of the
user) and publishes `RevokedAll { kind: Every, except: None }` (Anvil closes the user's sockets). The epoch is a
nullable integer column `credentials_epoch` on the users table (`t.big_integer("credentials_epoch").nullable()`)
that the model returns from `Authenticatable::credentials_epoch` (`Option<i64>`; default `None`). Sessions are bound
to the password hash and the epoch; sign-ins and sign-outs never change the epoch, so they never end other
sessions. A model without the column keeps working (`None` binds sessions as before); `end_credentials` then still
ends the remember token, tokens and sockets, and returns an error because open sessions stay signed in. A step
that fails never skips the others: the listeners run and the event goes out, then the first error is returned. It
does not change the password: whoever holds it (or a pending two-factor login) can sign in again unless a login
policy refuses the user.

**Login completion:** `AppBuilder::login_completion(c)` registers a `smeltery::auth::LoginCompletion`:
`complete(app, auth, session, headers, user, remember)` finishes signing in a known user (an `AuthUser`) and returns
the response; `app.login_completion()` returns it, so code that proves a user another way calls it instead of
`auth.login_user(&user, remember)` (`Auth::login` without the model type).

**Password resets:** `auth::passwords::send_reset_link(&app, email)` finds the account as `attempt` does, stores a
token for that account in `password_reset_tokens` (`user_id`, the token's SHA-256, `created_at`; one row per
account, replaced by each new link) and sends the link `APP_URL` + the route named `password.reset` (its `{token}`
filled in, else `/reset-password/{token}`) + `?email=…` to the address stored on the account, never to the typed
one. It returns `Some(user id)` when a link was issued and `None` otherwise, for the caller's records (the answer
to the visitor stays the same either way). At most
one link a minute goes to one address (further requests in that minute do nothing), an address without an account
does nothing, and the mail leaves in the background (under `APP_ENV=testing` before the call returns), so neither the
answer nor its timing tells whether an account exists; a failed delivery is logged, not returned. The link goes out
with [mail](#mail) installed (`.mail()`) as the `ResetPassword` mail
(`smeltery::mail::ResetPassword`, its template part of the framework). Without mail, the link goes to the log at
`info` level in local development (`APP_ENV` `local` or `testing` and an `APP_URL` on this machine: `localhost`,
`*.localhost` or a loopback address); anywhere else only a warning that no mail is set up is logged, never the link.
`auth::passwords::reset(&app, email, token,
new_password)` finds the account the same way, through the user model, and, when the token matches and is less than
60 minutes old, uses the token up (one conditional delete: of two requests with one token one wins), sets the new
password on that row by its `id` and clears its remember token; the new password hash signs out every session of the
user. With `.verify_email::<User>()` the address is marked verified when the link was mailed to the address the
account has now (a link proves control of the address it reached; the last 16 characters of a mailed token name
that address). A token from `passwords::create_token` verifies nothing. It returns the user's id
(`Ok(Some(id))`), `Ok(None)` for an unknown address or a wrong, used or expired token, then runs the credential
listeners and publishes `RevokedAll` (above). On MySQL and MariaDB a `users.email` column with a binary collation
(`utf8mb4_bin`) also makes the database itself compare addresses byte for byte.

**Email verification:** a user model whose email must be verified implements `smeltery::auth::MustVerifyEmail`
and has a nullable `email_verified_at` column; `bootstrap/app.rs` adds `.verify_email::<User>()` after
`.auth::<User>()`:

```rust
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "users")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub email: String,
#     pub email_verified_at: Option<DateTimeUtc>,
#     pub password: String,
#     pub remember_token: Option<String>,
# }
# impl ActiveModelBehavior for ActiveModel {}
# impl smeltery::auth::Authenticatable for Model {
#     fn auth_id(&self) -> i64 { self.id }
#     fn password_hash(&self) -> &str { &self.password }
#     fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
# }
# use Model as User;
impl smeltery::auth::MustVerifyEmail for User {
    fn email(&self) -> &str { &self.email }
    fn email_verified_at(&self) -> Option<DateTimeUtc> { self.email_verified_at }
}

fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.auth::<User>().verify_email::<User>()
}
# fn main() {}
```

An existing users table gets the column with `smeltery make:migration add_email_verified_at_to_users_table`, its
column line changed to `t.datetime("email_verified_at").nullable();`.

The `verified` middleware (put it after `auth`) sends guests, and users whose `email_verified_at` is empty, to the
route named `verification.notice` (or `/email/verify`) with a 303 and answers 403 `{"error": "Your email address is
not verified."}` to JSON clients. Without `.verify_email::<User>()` it lets every signed-in user through; guests
never pass.

| Call | Does |
|---|---|
| `auth.send_verification_email().await?` | reads the signed-in user's row again and issues their link to the mailer: `true` when a link was issued; `false` (nothing issued) for a guest, a verified user, or without `.verify_email` |
| `auth.resend_verification_email().await?` | the same, six calls a minute per user (counted atomically); then 429 for JSON clients, a redirect back with "Too many verification emails. Please try again in N seconds." on `email` for forms |
| `auth.has_verified_email().await?` | for a signed-in user: `true` when their email is verified, or without `.verify_email`; `false` for a guest |
| `EmailVerificationRequest` (handler argument) | the checked link; `request.fulfill().await?` sets `email_verified_at` (and `updated_at` when the model has it) to now: `true` the first time, `false` when it was set already |
| `auth::verification::verification_url(&app, &user)`, `send_verification_link(&app, &user).await?` | the link of any user, and sending it |
| `app.verifies_email()` | whether the app requires verified addresses (`.verify_email::<User>()`) |
| `auth::mark_verified(&app, user_id).await?`, `auth::mark_unverified(&app, user_id).await?` | set `email_verified_at` to now (unless set) or empty it (after the address changed), through the user model: `true` when it changed; an error without `.verify_email` |

A link is `APP_URL` followed by the route named `verification.verify` (`/email/verify/{id}/{hash}`) and
`?expires=…&signature=…`. `hash` is the SHA-256 of the user's email and `signature` an HMAC-SHA256 of the id, the
hash and the expiry time under a key derived from `APP_KEY`; the expiry time is fixed in the link when it is made,
`AUTH_VERIFICATION_EXPIRE` minutes later (default 60, at least 1). `EmailVerificationRequest` needs a signed-in user
(put the route behind `auth`: a link opened while signed out leads to the login, and a login handler that answers
with `auth.intended(…)` brings the user back to the link) and answers 403 "This verification link is invalid or
has expired." unless the signature is valid, the expiry time has not passed, the id is the signed-in user's and the
hash matches their current email: a link stops working when the email changes. With `.verify_email`, a route named `verification.verify`
without both `{id}` and `{hash}` stops the build with an error. A handler that changes a user's email sets
`email_verified_at` to `None` in the same update and then calls `auth.send_verification_email()`, which sends the
link to the new address. The link is sent like a reset link: with mail installed as the `VerifyEmail` mail
(`smeltery::mail::VerifyEmail`, its template part of the framework); without mail it goes to the log in local
development only (as above).

A web app declares the three routes:

```rust
use smeltery::auth::EmailVerificationRequest;
use smeltery::prelude::*;

async fn notice(app: App, auth: Auth) -> Result<Response> {
    if auth.has_verified_email().await? {
        return Ok(Redirect::to(&app.settings().auth_home).into_response());
    }
    Ok("Check your inbox for the verification link.".into_response())
}

async fn verify(app: App, session: Session, request: EmailVerificationRequest) -> Result<Redirect> {
    request.fulfill().await?;
    session.flash("status", "Your email address is verified.");
    Ok(Redirect::to(&app.settings().auth_home))
}

async fn send(auth: Auth, session: Session, back: Back) -> Result<Redirect> {
    auth.resend_verification_email().await?;
    session.flash("status", "verification-link-sent");
    Ok(back.redirect())
}

fn routes(r: &mut Router) {
    r.get("/email/verify", notice).name("verification.notice").middleware("auth");
    r.get("/email/verify/{id}/{hash}", verify)
        .name("verification.verify")
        .middleware("auth");
    r.post("/email/verification-notification", send)
        .name("verification.send")
        .middleware("auth");
}
# fn main() {}
```

The register handler calls `auth.send_verification_email().await?` after `auth.login(&user, false).await?`.

### Temper: the authentication routes

Temper (`smeltery::temper`) puts the flows above behind routes: login, logout, registration, password reset,
e-mail verification, password confirmation and profile and password updates, each feature switched on in one
builder. The app keeps the pages (`TemperViews`: one closure per page returning any response, such as a
`#[derive(Mold)]` struct or an Alloy page), the actions (traits with a typed, validated `Input`:
`CreatesNewUsers`, `ResetsUserPasswords`, `UpdatesUserPasswords`, `UpdatesUserProfileInformation`) and, where it
wants, its own answers (`TemperResponses`). Every check runs through the core functions of this section.

```rust
# mod user {
#     use smeltery::db::prelude::*;
#     #[sea_orm::model]
#     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#     #[sea_orm(table_name = "users")]
#     pub struct Model {
#         #[sea_orm(primary_key)]
#         pub id: i64,
#         pub email: String,
#         pub email_verified_at: Option<DateTimeUtc>,
#         pub password: String,
#         pub remember_token: Option<String>,
#     }
#     impl ActiveModelBehavior for ActiveModel {}
#     impl smeltery::auth::Authenticatable for Model {
#         fn auth_id(&self) -> i64 { self.id }
#         fn password_hash(&self) -> &str { &self.password }
#         fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
#     }
#     impl smeltery::auth::MustVerifyEmail for Model {
#         fn email(&self) -> &str { &self.email }
#         fn email_verified_at(&self) -> Option<DateTimeUtc> { self.email_verified_at }
#     }
# }
# use user::Model as User;
use serde::Deserialize;
use smeltery::http::Html;
use smeltery::temper::{PasswordInput, ResetsUserPasswords, Temper, TemperExt as _, TemperViews};
use smeltery::Validate;

/// The reset form: the app's password rules.
#[derive(Deserialize, Validate)]
pub struct ResetForm {
    #[validate(required, email)]
    #[serde(deserialize_with = "smeltery::auth::deserialize_email")]
    pub email: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: Option<String>,
}

impl PasswordInput for ResetForm {
    fn email(&self) -> &str { &self.email }
    fn password(&self) -> &str { &self.password }
}

pub struct ResetUserPassword;

impl ResetsUserPasswords<User> for ResetUserPassword {
    type Input = ResetForm;
}

fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.temper(
        Temper::<User>::new()
            .reset_passwords(ResetUserPassword)
            .email_verification()
            .views(
                TemperViews::new()
                    .login(|_| Html("<h1>Log in</h1>"))
                    .forgot_password(|_| Html("<h1>Forgot your password?</h1>"))
                    .reset_password(|ctx| Html(format!("<h1>New password</h1><p>{} chars</p>", ctx.token().len())))
                    .verify_email(|_| Html("<h1>Check your inbox</h1>"))
                    .confirm_password(|_| Html("<h1>Confirm your password</h1>")),
            ),
    )
}
# fn main() { let _ = build; }
```

`.temper(…)` registers `User` as the user model (`.auth::<User>()`; call `.auth` with no other model), the web
routes below and its login pipeline as the login completion (`app.login_completion()`), and never flashes a field
named `code` as old input. The app stops at boot when a page route has
no view (the error names the `TemperViews` method), when `without_route` names an unknown route, when the prefix is
invalid, or when `.auth::<…>()` registers another model.

| Route | Name | Middleware | Feature |
|---|---|---|---|
| `GET` / `POST /login` | `login`, `login.store` | `guest`; `throttle:30,1` on `POST` | always |
| `POST /logout` | `logout` | `auth` | always |
| `GET` / `POST /register` | `register`, `register.store` | `guest`; `throttle:6,1` on `POST` | `registration` |
| `GET` / `POST /forgot-password` | `password.request`, `password.email` | `guest`; `throttle:6,1` on `POST` | `reset_passwords` |
| `GET` / `POST /reset-password/{token}` | `password.reset`, `password.update` | `guest`; `throttle:6,1` on `POST` | `reset_passwords` |
| `GET /email/verify` | `verification.notice` | `auth` | `email_verification` |
| `GET /email/verify/{id}/{hash}` | `verification.verify` | `auth`, `throttle:6,1` | `email_verification` |
| `POST /email/verification-notification` | `verification.send` | `auth` (six a minute per user) | `email_verification` |
| `PUT /user/profile-information` | `user-profile-information.update` | `auth`, `password.confirm`, `throttle:6,1` | `update_profile_information` |
| `PUT /user/password` | `user-password.update` | `auth`, `throttle:6,1` | `update_passwords` |
| `GET` / `POST /user/confirm-password` | `password.confirm`, `password.confirm.store` | `auth`; `throttle:6,1` on `POST` | always |
| `GET /user/confirmed-password-status` | `password.confirmation` | `auth` | always |
| `GET` / `POST /two-factor-challenge` | `two-factor.login`, `two-factor.login.store` | `guest`; `throttle:30,1` on `POST` | `two_factor` |
| `POST` / `DELETE /user/two-factor-authentication`, `POST /user/confirmed-two-factor-authentication`, `GET /user/two-factor-qr-code`, `GET /user/two-factor-secret-key`, `GET` / `POST /user/two-factor-recovery-codes` | `two-factor.enable`, `two-factor.disable`, `two-factor.confirm`, `two-factor.qr-code`, `two-factor.secret-key`, `two-factor.recovery-codes`, `two-factor.regenerate-recovery-codes` | `auth`, `password.confirm` (with `confirm_password(false)` only for a confirmed enrolment); `throttle:6,1` on `POST` and `DELETE` | `two_factor` |

A login checks the password with `auth.validate` (the login budgets above), then runs the login pipeline: the app's
login policies (`app.check_login`, see [Authentication](#authentication); `.login_policy(|app, user| …)` adds one
typed on the user model), the app's steps (`Temper::login_pipeline`), the sign-in (`auth.login_user`; Temper is the app's login completion), the `Login` event and the answer (the remembered page, else
`AUTH_HOME`; JSON clients get 200 `{"two_factor": false}`). A wrong address or password answers "These credentials
do not match our records." on `email`: the login page for browsers and Inertia visits, 422 for JSON clients. A
password update checks `current_password` through `auth.confirm_password` (five tries a minute per user) and stores
the new one with `auth.set_password`, so this device stays signed in and every other session ends. A profile update
that changes the address empties `email_verified_at` through `auth::mark_unverified` when the app requires verified
addresses, and sends a link to the new address. JSON clients (`Accept: application/json`) get status codes and JSON
bodies throughout; `.views(false)` registers no page routes (browser requests then get redirects to pages that do
not exist). The reset page's `ctx.token()` and `ctx.email()` come from the link as sent: render them escaped. Other options: `.home(path)`, `.prefix("/auth")`, `.routes(false)`, `.without_route(name)`,
`.limits(Limits::new().login("30,1").forms("6,1"))`, `.listen(f)` for `TemperEvent`s (user ids only; listeners
never change the answer). `smeltery::temper::login_pipeline(&ctx, &user, remember)` signs a user in through the same
pipeline from another sign-in path. Pipeline steps gate new sign-ins only: an open session or a remember-me restore
never meets them, so locking an account out also ends its credentials (`auth::end_credentials`). Steps run for web
sign-ins only; a rule that must also hold for API tokens and remember-me cookies (a suspended account) is a login
policy. Registration asks the login policies after creating the account: a refusal (an account that awaits
approval) keeps the account, fires `Registered`, signs nobody in and answers the refusal. When the action returns a
user that has two-factor authentication (an existing account), the request fails with 500 and nobody is signed in.
A refusal at the two-factor challenge answers JSON clients with the message on `code` and sends browsers to the
login page with it on `email`. `.two_factor(TwoFactor::new())` (a user model implementing `TwoFactorAuthenticatable` over the columns
`two_factor_secret`, `two_factor_recovery_codes`, `two_factor_confirmed_at`, `two_factor_last_step`) adds codes from
an authenticator app (RFC 6238 TOTP) and single-use recovery codes: a correct password then gives a pending login (a
new session id; five minutes; bound to the password hash; ended after five wrong codes) and the challenge page (JSON
clients: 200 `{"two_factor": true}`) instead of a sign-in. Secrets are encrypted with
`App::encrypt("temper.two-factor", <user id>, …)`, recovery codes stored as SHA-256 hashes and shown once; a code is
accepted once (one conditional `UPDATE` of the last accepted time step), a recovery code removed by compare-and-set;
every code counts against the account's budget (five per five minutes, one hundred a day, shared by processes through
the cache) before it is checked; past it the challenge answers 429 with `Retry-After` (browsers: the message on the
challenge page). The login policies are asked again before a code is checked. The QR code is an SVG data URI for an `<img>`. Turning two-factor on (confirmed) or
off replaces the remember token. It also registers core's `SecondFactor`, removes an enrolment when a reset verifies
a previously unverified address, and adds `temper:two-factor-disable <email> --force`. A new `APP_KEY` makes stored
secrets unreadable: app codes are refused (two-factor stays on), recovery codes still work.
The crate's README (`smeltery-temper`) has the full reference.

### Guards and the principal

Who made a request is its principal (`smeltery::auth::Principal`): the user's id (`user_id`), the guard that proved
it (`guard`), the credential (`credential`: `Credential::Session { binding }` for a signed-in session,
`Credential::Token { id, abilities }` for a token a guard issued) and when that credential stops working
(`expires_at`, `None` for sessions). A guard (`smeltery::auth::Guard`) reads a request's head and answers with the
principal it proves, or `None`. `.auth::<User>()` registers the guard `web`: the signed-in session, whose principal
the session stack stores on every web request of a signed-in user. `AppBuilder::guard(g)` registers another; two
guards with one name, a name other than lowercase ASCII letters, digits, `_` and `-`, or a guard named `web`, stop
the app at boot. A guard returns a new principal for every request. A guard that reads only headers (a bearer
token) says so with `stateless()`: stateless guards never run on web routes, so a bearer credential never stands in
for the session and its CSRF check there.

| Call | Does |
|---|---|
| `.middleware("auth:web")` | the plain `auth` middleware |
| `.middleware("auth:<g1>,<g2>")` | runs the guards in order and stores the first principal found; without one, an API route whose list names a stateless guard answers 401 `{"error":"Unauthenticated."}` with `WWW-Authenticate: Bearer` and `Cache-Control: no-store`, and every other route what `auth` answers (on web routes the session is the only credential). An unknown guard name stops the app at boot |
| `Authenticated` (handler argument) | the principal (it derefs to `Principal`): the one the session stack or an `auth:` alias stored, else on API routes the first a stateless guard finds; 401 without one. `Option<Authenticated>` gives `None` instead |
| `principal.user::<User>(&app).await?` / `principal.auth_user(&app).await?` | the principal's user (loaded once per principal), typed or as an `AuthUser` |
| `principal.can("orders:read")` | sessions hold every ability; a token the abilities it lists, by exact name, or all with `*` |
| `principal.key()` | `<guard>:session:<binding>` or `<guard>:token:<id>` (`web:session:3f…`, `hallmark:token:12`): the name of the credential in grants and revocation events, unique across guards |
| `auth::authenticate(&app, &mut parts, GuardSet::Stateless).await?` | the first principal of the stateless guards for a request head, stored nowhere; `None` on web requests |
| `principal.with_user(user)?` | the principal with its user already loaded (a guard that read it), an error for another user's id |
| `session.binding()` | 24 hex characters naming the session (the first 12 bytes of the SHA-256 of its CSRF secret): the `<binding>` of `web:session:<binding>` |
| `auth::credential_key(guard, kind, id)` | the key format of `principal.key()` (`<guard>:<kind>:<id>`), for code that names credentials it revokes |
| `Guard::first_party(app, parts)` | a guard's answer to "does this API request come from the app's own pages?" (default `false`); when a listed guard says yes, the `auth:` alias runs the whole web session stack around the request (`smeltery::session::run_web_stack`: session, remember-me, password-hash binding, the CSRF check on every state-changing method), lets a signed-in session through and otherwise runs the guards inside it |
| `auth::unauthenticated_bearer()` | the response of a bearer endpoint without a valid credential: 401 `{"error":"Unauthenticated."}`, `WWW-Authenticate: Bearer`, `Cache-Control: no-store` (what `auth:` answers on API routes) |
| `app.has_stateless_guard()` | whether a stateless guard is registered |
| `app.find_user(id).await?` | the user with that id as an `AuthUser`: `id()`, `downcast::<User>()`, `binding(purpose)` |
| `auth.auth_user().await?` | the signed-in user of a web request as an `AuthUser` |

`AuthUser::binding(purpose)` is the SHA-256 of `smeltery-<purpose>|<password hash>` (`purpose`: lowercase ASCII
letters, digits, `.`, `_`, `-`, else an error): a credential that stores it when issued and compares it on use
stops working after any password change. Signed-in sessions are bound this way
with the purpose `session`. The `throttle:` and `verified` middleware read the principal, so put `auth:<guards>`
before them on a route. `verified` checks the principal's user (session or token) and answers 403 `{"error": "Your
email address is not verified."}` on API routes and to JSON clients, and 401 when a guard's principal has no user
row any more.

```rust
use smeltery::auth::{Authenticated, Credential, Guard, Principal};
use smeltery::http::request::Parts;
use smeltery::prelude::*;
use smeltery::BoxFuture;

/// Accepts `X-Demo-User: <id>` (a stand-in for a real token check).
struct DemoGuard;

impl Guard for DemoGuard {
    fn name(&self) -> &'static str { "demo" }
    fn stateless(&self) -> bool { true }
    fn authenticate<'a>(&'a self, _app: &'a App, parts: &'a mut Parts)
        -> BoxFuture<'a, Result<Option<Principal>>> {
        Box::pin(async move {
            let id = parts.headers.get("x-demo-user")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            Ok(id.map(|id| Principal::new(id, "demo", Credential::token(0, ["orders:read"]))))
        })
    }
}

async fn orders(who: Authenticated) -> Result<String> {
    if !who.can("orders:read") {
        return Err(Error::http(smeltery::http::StatusCode::FORBIDDEN, "Forbidden"));
    }
    Ok(format!("orders of user {}", who.user_id))
}

fn build(app: AppBuilder) -> AppBuilder {
    app.guard(DemoGuard).api_routes(|r| {
        r.get("/orders", orders).middleware("auth:demo").middleware("throttle:60,1");
    })
}
# fn main() {}
```

## API tokens

Hallmark (`smeltery::hallmark`) gives an app personal access tokens: API clients, mobile and desktop apps,
command-line tools and other backends send `Authorization: Bearer smt_…` to API routes, and each token carries a
list of abilities.

```rust
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "users")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub password: String,
#     pub remember_token: Option<String>,
# }
# impl ActiveModelBehavior for ActiveModel {}
# impl smeltery::auth::Authenticatable for Model {
#     fn auth_id(&self) -> i64 { self.id }
#     fn password_hash(&self) -> &str { &self.password }
#     fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
# }
# use Model as User;
use smeltery::auth::Authenticated;
use smeltery::hallmark::{CurrentToken, Hallmark, HallmarkExt as _, HasApiTokens as _};
use smeltery::http::{HeaderValue, StatusCode, header};
use smeltery::prelude::*;

/// `POST /api/tokens/cli`: a signed-in API client issues a second token for a command-line tool.
async fn issue(who: Authenticated, app: App) -> Result<Response> {
    let user: User = who.user(&app).await?.ok_or_else(Error::unauthorized)?;
    let new = user.create_token(&app, "cli", &["deploy"]).await?;
    let mut res = Json(new.to_json()).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

async fn orders(who: Authenticated) -> String {
    format!("the orders of user {}", who.user_id)
}

async fn sign_out(token: CurrentToken) -> Result<StatusCode> {
    token.revoke().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn build(app: AppBuilder) -> AppBuilder {
    app.auth::<User>()
        .hallmark(Hallmark::new())
        .api_routes(|r| {
            r.post("/tokens/cli", issue)
                .middleware("auth:hallmark")
                .middleware("abilities:tokens:create");
            r.get("/orders", orders)
                .middleware("auth:hallmark")
                .middleware("abilities:orders:read");
            r.delete("/tokens/current", sign_out).middleware("auth:hallmark");
        })
}
# fn main() { let _ = build; }
```

A route that issues tokens to a token holder checks an ability (`abilities:tokens:create` above): without it any
token, even one that may do nothing, could issue itself a new one with more abilities.

`.hallmark(Hallmark::new())` (after `.auth::<User>()`; without a user model the app stops at boot) registers the
guard `hallmark`, the middleware families `abilities:` and `ability:`, the `Tokens` service, a credential listener
and the console command `hallmark:prune-expired`. The table comes from a migration that calls
`smeltery::hallmark::migrations::up(schema)` / `down(schema)`; `serve` logs an error when the table is missing.

| Call | Does |
|---|---|
| `Tokens::of(&app)?` / `tokens: Tokens` (handler argument) | the token service |
| `tokens.create(user_id, "Ada's phone", &["orders:read"], None).await?` | a new token (`NewToken`): `plain_text()` is the token, shown once and never stored; `token()` its stored data; `to_json()` the answer `{"token":"smt_…","token_type":"Bearer","expires_at":…,"abilities":[…]}`. `Some(time)` sets an earlier expiry |
| `user.create_token(&app, name, abilities)` / `user.tokens(&app)` / `user.revoke_tokens(&app)` | the same on any user model (`HasApiTokens`) |
| `tokens.list(user_id).await?` | the user's tokens (`AccessToken`: `id`, `user_id`, `name`, `abilities`, `last_used_at`, `expires_at`, `created_at`), newest first; never a token or its hash |
| `tokens.find(user_id, id)` / `tokens.revoke(user_id, id)` | one token of that user: ids from a request reach only that user's tokens |
| `tokens.revoke_all(user_id)` / `tokens.revoke_all_except(user_id, keep_id)` | delete the user's tokens (all, or all but one); the number deleted |
| `tokens.prune_expired(Duration)` / `hallmark:prune-expired [--hours=24]` | delete tokens expired at least that long ago, 500 rows per statement |
| `.middleware("auth:hallmark")` | the request needs a valid token (on API routes) |
| `.middleware("abilities:a,b")` / `.middleware("ability:a,b")` | the principal needs every listed ability / at least one; 403 `{"error":"Forbidden."}`, 401 without a principal. Put `auth:hallmark` first |
| `CurrentToken` (handler argument) | the token of this request (`id()`, `name()`, `abilities()`, `can()`, `expires_at()`, `revoke()`); 401 when a token did not authenticate the request, `None` as `Option<CurrentToken>` |

- **Tokens** are `smt_` followed by 64 lowercase hex characters (32 random bytes). The table stores only their
  SHA-256; `PlainToken` has no `Display` and no `Serialize`, and its `Debug` prints `PlainToken(smt_…)`.
- **The guard** reads one `Authorization: Bearer <token>` header (the scheme in any letter case), never a query
  string, cookie, body or other header. A malformed token is refused before any database work; a found row is
  compared again in constant time. Missing, malformed, unknown, expired and revoked tokens get the same 401
  `{"error":"Unauthenticated."}` with `WWW-Authenticate: Bearer` and `Cache-Control: no-store`, and requests the
  guard looked at answer with `Vary: Authorization, Cookie`. Web routes (`routes/web.rs`) never accept a bearer
  token: they keep the session and its CSRF check.
- **Guesses:** one client (its address after `TRUSTED_PROXIES`, an IPv6 client by its /64) may send
  `HALLMARK_GUESS_LIMIT` invalid tokens a minute (malformed, unknown, expired and revoked ones alike). Past that, a
  bearer request from that address gets 429 with `Retry-After` until the minute ends, before any lookup, unless this
  process accepted its token in the last five minutes: clients sharing the address (a carrier or office network)
  keep working with the tokens they use. The count lives in the app's cache store and is shared by the app's
  processes (each bearer request reads it once); a cache error answers 500. Requests that reach the app without any
  connection information (only outside its server) are not counted. Without `TRUSTED_PROXIES` behind a proxy every
  client is the proxy's address and shares one budget.
- **Expiry:** a new token expires after `HALLMARK_TOKEN_EXPIRATION` days (an explicit later time is shortened to
  that), and every request also refuses a token older than that, so lowering the setting shortens existing tokens.
  An expired token is deleted when it is next presented; `hallmark:prune-expired` deletes the rest.
- **Passwords:** a token is bound to its user's password hash. A password written by any code ends the user's tokens
  at their next use; a password reset, `Auth::logout_other_devices`, `Auth::set_password` and
  `auth::password_changed` delete them at once (`password_changed` keeps the token of the request that called it).
  Deleting the user deletes the tokens (foreign key).
- **Abilities** are exact names (1 to 100 of `A-Z a-z 0-9 : . _ -`) or `*` alone (every ability), at most 64 per
  token; there are no patterns (`orders:*` is refused). A token created with `&[]` may do nothing; a stored list that
  breaks these rules grants nothing (logged as an error with the token id). A signed-in session holds every ability:
  abilities limit tokens, not users, so handlers still check that the user may touch a record.
- **Limits:** a user holds at most `HALLMARK_MAX_TOKENS_PER_USER` tokens; creating one more deletes the least
  recently used, in the same transaction. `last_used_at` is written at most once a minute per token, in the
  background with a 2 s timeout.
- **Events:** `revoke`, `CurrentToken::revoke`, a token deleted when presented and a token deleted by the cap publish
  `AuthEvent::Revoked { key: "hallmark:token:<id>" }`; `revoke_all` / `revoke_all_except` publish
  `AuthEvent::RevokedAll { kind: Tokens }` on the PubSub topic `auth` (see
  [PubSub](#pubsub-messages-between-processes)).

Clients without a browser session (mobile and desktop apps, command-line tools) get their token for an email and
password:

```rust
use serde::Deserialize;
use smeltery::hallmark::{issue_for_credentials, revoke_current};
use smeltery::http::ClientInfo;
use smeltery::prelude::*;

#[derive(Deserialize)]
struct TokenRequest {
    email: String,
    password: String,
    device_name: String,
    code: Option<String>,
}

async fn store(app: App, client: ClientInfo, Json(form): Json<TokenRequest>) -> Result<Response> {
    let issued = issue_for_credentials(
        &app, &client, &form.email, &form.password, &form.device_name, form.code.as_deref(), &["*"],
    )
    .await?;
    Ok(issued.created())
}

fn routes(r: &mut smeltery::routing::Router) {
    r.post("/tokens", store).middleware("throttle:10,1");
    r.delete("/tokens/current", revoke_current).middleware("auth:hallmark");
}
# fn main() { let _ = routes; }
```

- `issue_for_credentials` checks the address and password with core's `auth::verify_credentials`: the login budgets
  (shared with the web login form), the same work for unknown addresses, the password-hash gate. A wrong address or
  password answers 422 with "These credentials do not match our records." on `email`; past a budget 429; a
  `device_name` that is empty, longer than 255 characters or holds control characters 422 on `device_name`.
- The app's login policies (`AppBuilder::login_policy`, such as refusing a suspended account) decide next, as at
  the web sign-in: a refusal answers the policy's status with its message on `email`, and no token is created.
- When a second factor is registered (`AppBuilder::second_factor`) and the user needs one, `code` must be valid
  before any token exists: 422 "A two-factor code is required." or "The two-factor code is invalid." on `code`;
  past the second factor's own attempt budget, 429 with `Retry-After`.
- `issued.created()` answers 201 `{"token":"smt_…","token_type":"Bearer","expires_at":…,"abilities":[…]}` with
  `Cache-Control: no-store`. `revoke_current` deletes the token of the request and answers 204.

| `.env` | Builder | Default |
|---|---|---|
| `HALLMARK_TOKEN_EXPIRATION` (days; `0` = never) | `.expiration(Duration)` | `365` |
| `HALLMARK_MAX_TOKENS_PER_USER` | `.max_tokens_per_user(n)` | `100` |
| `HALLMARK_GUESS_LIMIT` (invalid tokens per client a minute) | `.guess_limit(n)` | `60` |
| `HALLMARK_SPA` (`true` / `false`) | `.spa()` | `false` |
| `HALLMARK_STATEFUL` (first-party origins besides `APP_URL`'s) | `.stateful(&[...])` | empty |

In tests, `smeltery::hallmark::testing::acting_as(&app, &user, &["orders:read"])` creates a real token and sends it
with every following request; `token_for(&app, &user, &["*"])` returns one without changing the app's headers.

### SPA authentication

A JavaScript frontend served from the app's own origin (from `public/`, or through a proxy or a Vite dev server
under the same origin) can call `auth:hallmark` API routes with the session cookie instead of a token:

```rust
use smeltery::hallmark::{Hallmark, HallmarkExt as _};

fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.hallmark(Hallmark::new().spa().stateful(&["http://localhost:5173"]))
}
# fn main() { let _ = build; }
```

- `.spa()` (or `HALLMARK_SPA=true`) turns it on. A request on an `auth:hallmark` API route is first-party when it
  carries `Sec-Fetch-Site: same-origin` (a page of `APP_URL`'s origin); or `Sec-Fetch-Site: same-site` /
  `cross-site` with one `Origin` listed with `.stateful(...)` / `HALLMARK_STATEFUL` (comma-separated
  `scheme://host[:port]`; scheme, host and port compared exactly); or, from a browser that sends no
  `Sec-Fetch-Site`, one `Origin` (else the `Referer`'s origin) equal to `APP_URL`'s origin or a listed one. An
  unlisted sibling subdomain or port, `Sec-Fetch-Site: none`, a `null` or foreign `Origin`, and requests without any
  of these headers (an app or server calling with a token) are not first-party. A `*`, `null` or a URL with a path in
  `HALLMARK_STATEFUL` stops the app at boot.
- A listed origin is a page of the same site on another origin, such as the Vite dev server above on another port
  of `localhost`: it calls the API with `fetch(url, { credentials: "include" })` (the session cookie is
  `SameSite=Lax`, so it is sent only to the same site). Smeltery sends no CORS headers, so such a page reads the
  answers only when the app adds them; a proxy that serves the page and the app under one origin needs neither.
- List only origins you control: any script on a listed origin acts with the signed-in user's session. A
  development origin such as `http://localhost:5173` belongs in the development `.env` only, never in production.
- A first-party request runs the web session stack: the session cookie, remember-me, the password binding, and the
  CSRF check of web routes on every method except GET, HEAD, OPTIONS and TRACE (419 without a valid
  `X-XSRF-TOKEN`, `X-CSRF-TOKEN` or `_token`). A signed-in session passes (its principal is the `web` guard's, with
  every ability); without one, a bearer token in the request is still checked. The CSRF check comes first, so a
  first-party page that sends a token instead still needs `X-XSRF-TOKEN` on state-changing calls (419 without it),
  and when a first-party request carries both a signed-in session and a token, the session decides who it is.
  Every other request is bearer-only: the session cookie is not read.
- `GET /hallmark/csrf-cookie` (route `hallmark.csrf-cookie`) answers 204 and sets the `XSRF-TOKEN` cookie, which SPA
  mode turns on for every web response. The SPA reads it, signs in through the app's login route, and sends the
  cookie's value as `X-XSRF-TOKEN` on state-changing API calls. Session cookies keep their `__Host-` prefix under
  https, so a frontend on another subdomain or site uses tokens.

## Search

Prospect (`smeltery::prospect`) is full-text search for models. A model declares which columns are searched (with
weights), filtered and sorted; a search returns a `Page` of hits that hold real models, ranked by relevance, with
highlights. The `database` driver uses the database's own full-text search: SQLite FTS5, PostgreSQL `tsvector` with a
GIN index, MySQL / MariaDB `FULLTEXT`. The database keeps the index current itself (triggers, a generated column, the
`FULLTEXT` index), so every write is searchable at once, raw SQL and bulk updates included, and no extra server runs.

### Making a model searchable

`impl Searchable` next to the model's entity, register the model in `bootstrap/app.rs`, and add the index with a
migration:

```rust,no_run
# mod post {
use smeltery::db::prelude::*;
use smeltery::prospect::{IndexSpec, Searchable, Weight};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "posts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub title: String,
    pub body: String,
    pub user_id: i64,
    pub team_id: i64,
    pub published: bool,
    pub created_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

impl Searchable for Model {
    fn index(i: &mut IndexSpec) {
        i.text("title").weight(Weight::A); // searched, ranked highest
        i.text("body"); // Weight::B
        i.filter("user_id"); // where_eq / where_in / where_not_in / where_between on it
        i.sort("created_at"); // order_by on it (relevance is the default order)
        i.only_when("published"); // rows with false are not found
        i.scoped_by("team_id"); // every search names a team
    }
}
# }
# use post::Model as Post;
use smeltery::prelude::*;

pub fn build(app: AppBuilder) -> AppBuilder {
    app.prospect(|p| {
        p.model::<Post>();
    })
}
# fn main() {}
```

| `IndexSpec` call | Means |
|---|---|
| `i.text(column)`, `.weight(Weight::A)` | a searched string column; weights `A` (highest) to `D`, default `B`; the order of the calls is the order of the index's columns |
| `i.filter(column)` | an integer, bool, string or date-time column searches may filter on |
| `i.sort(column)` | a column searches may order by |
| `i.only_when(column)` | a bool column: rows where it is false are not found |
| `i.scoped_by(column)` | every search must call `within(value)`, or `across_scopes()` explicitly; the column is also a filter column |
| `i.language(Language::English)` | English stemming on SQLite and PostgreSQL (`forging` finds `forge`); the default `Language::Simple` compares words as they are |
| `i.name(name)` | the memory engine's index name (default: the table's); the database index is always `<table>_search` |

The spec is checked when the app builds: every name must be a column of the model, `text` columns strings,
`only_when` a bool, at least one `text`, and the model needs one integer primary key (`t.id()`). A mistake, a model
registered twice or an unknown `PROSPECT_DRIVER` stops the build with the table and the column.

The migration names the same text columns, weights and language:

```rust
use smeltery::db::migration::{Migration, Schema};
use smeltery::prospect::migration::{Language, SearchIndex, Weight};
use smeltery::Result;

pub struct CreatePostsTable;

impl Migration for CreatePostsTable {
    fn name(&self) -> &'static str {
        "2026_10_05_120000_create_posts_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("posts", |t| {
                t.id();
                t.string("title");
                t.text("body");
                t.big_integer("user_id");
                t.big_integer("team_id");
                t.boolean("published").default(false);
                t.timestamps();
            })
            .await?;
        SearchIndex::on("posts")
            .text("title", Weight::A)
            .text("body", Weight::B)
            .language(Language::Simple)
            .create(schema)
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        SearchIndex::on("posts").drop(schema).await?;
        schema.drop_if_exists("posts").await
    }
}
# fn main() {}
```

| Backend | `SearchIndex::create` writes | `drop` removes |
|---|---|---|
| SQLite | the FTS5 table `posts_search` (external content over `posts`, keyed by `id`), the triggers `posts_search_ai`, `_ad`, `_au` that keep it current, and a `rebuild` that indexes the rows already there | the triggers and the table |
| PostgreSQL | the stored generated column `search_vector` (`tsvector`, weighted) and the GIN index `posts_search_index` | the index and the column |
| MySQL / MariaDB | the `FULLTEXT` index `posts_search` over the text columns | the index |

`SearchIndex::on("posts").rebuild(schema)` drops and creates it again, for a migration that changes the columns,
weights or language. `.key("post_id")` names the integer key column when the model's primary key is not `id`. Adding the generated column on PostgreSQL
rewrites the table, and the first `FULLTEXT` index of a MySQL table rebuilds it: run such a migration while the table
is small or during maintenance. The first search of a model checks its index: on SQLite the FTS5 table's text
columns in order, its table and key (`content`, `content_rowid`) and the three triggers; on PostgreSQL the columns,
weights and language of the `search_vector` expression (read from `information_schema`, so the app's database user
owns the table) and its GIN index; on MySQL the `FULLTEXT` index's columns in
order. A missing or different index is logged once (ERROR) and every search of that model answers a 500 whose
message names the migration to write; `serve` logs it at start, and `prospect:status` shows it.

### Searching

```rust,no_run
# mod post {
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "posts")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub title: String,
#     pub body: String,
#     pub user_id: i64,
#     pub team_id: i64,
#     pub created_at: Option<DateTimeUtc>,
# }
# impl ActiveModelBehavior for ActiveModel {}
# impl smeltery::prospect::Searchable for Model {
#     fn index(i: &mut smeltery::prospect::IndexSpec) {
#         i.text("title"); i.text("body"); i.filter("user_id"); i.sort("created_at"); i.scoped_by("team_id");
#     }
# }
# }
# use post::Model as Post;
use smeltery::prelude::*;
use smeltery::prospect::{Direction, Hit, Prospect};

#[derive(serde::Deserialize)]
struct SearchForm {
    #[serde(default)]
    q: String,
}

async fn index(prospect: Prospect, Query(s): Query<SearchForm>, page: PageQuery) -> Result<Json<Page<Hit<Post>>>> {
    let team_id = 1; // from the signed-in user
    let posts = Post::search(&prospect, &s.q)
        .within(team_id) // the scope: required because of scoped_by
        .where_eq("user_id", 7) // declared filter columns only
        .order_by_relevance() // the default; or .order_by("created_at", Direction::Desc)
        .highlight(["title", "body"])
        .paginate(page) // ?page=&per_page=, bounded
        .await?;
    Ok(Json(posts))
}

pub fn routes(r: &mut Router) {
    r.get("/posts", index).middleware("throttle:60,1");
}
# fn main() { let _ = Direction::Asc; }
```

`Post::search(&prospect, text)` starts a search (`smeltery::prelude` brings the `Searchable` trait into scope). The
builder:

| Call | Means |
|---|---|
| `within(value)` / `across_scopes()` | the scope value, or every scope (explicitly) |
| `where_eq(column, value)`, `where_in(column, values)`, `where_not_in(column, values)`, `where_between(column, low, high)` | conditions on `filter` columns; values of the column's type (`i64` and smaller integers, `bool`, strings, `DateTimeUtc`); `where_in` and `where_not_in` take at most 100 values |
| `order_by(column, Direction::Asc / Desc)`, `order_by_relevance()` | a `sort` column, or relevance (the default); ties by key, newest first |
| `highlight(columns)` | highlights of at most 4 `text` columns |
| `query(\|select\| select.filter(…))` | a SeaORM refinement of the same SQL statement ("published or mine"); database driver only |
| `paginate(page)` | a `Page<Hit<M>>`: one page and the total (two statements) |
| `get(limit)`, `keys(limit)`, `count()` | the first hits, their keys, the number of matches |

An undeclared column, a value of the wrong type, a bound passed, or a scoped model searched without `within` /
`across_scopes` makes the call that runs the search return an error (a 500) before any query;
`ProspectError::of(&error)` reads which.

**The search text.** User text is never search syntax. It is trimmed and cut at `PROSPECT_MAX_QUERY_LENGTH`
characters (200), split into terms on everything that is not a letter or a digit, lower-cased, and at most 16 terms are
kept. Every term must match; the last one also matches as a prefix when it has two characters or more (`forg` finds
`forge`), so a search-as-you-type box works. Quotes, `*`, `:`, `-`, parentheses and words like `AND`, `OR`, `NOT`,
`NEAR` are plain text. A text without terms (empty, or punctuation only) has no text condition: the filtered rows come
newest key first, so an index page with an empty search box keeps working.

**Results.** A `Hit<Post>` derefs to the post (`hit.title`) and has `score` (`Option<f64>`, higher is better, `None`
without terms; scores compare the hits of one search) and `highlights`. It serializes as the model's fields plus
`_score` and `_highlights`, so an Alloy page reads it directly. A highlight is a list of text segments,
`{"text": …, "matched": true / false}` (`Segment`), never HTML; render each one escaped and wrap the matched ones.
A Mold template reads struct fields, which release builds compile to Rust field access, so the handler hands it the
segments in a field (`hit.highlights.get("title").map(|h| h.segments().to_vec()).unwrap_or_default()`, a
`Vec<Segment>`, next to the record from `hit.into_model()`), and the template loops over them (the empty comment
keeps `@endfor` from touching the word before it):

```text
@for(row in posts.items)
  <h2>@for(s in row.title)@if(s.matched)<mark>{{ s.text }}</mark>@else{{ s.text }}@endif{{-- --}}@endfor</h2>
@endfor
```

In React and Vue, render segments as text nodes inside `<mark>`. A column of at most 2,000 characters comes back whole
with every match marked; above that it is a snippet around the matches (SQLite: 32 tokens; PostgreSQL: two fragments
of 10 to 30 words from the first 20,000 characters). The memory engine and MySQL highlight the first 20,000
characters in Rust and show a window of about 30 words around the first match of a text longer than 60 words, with
`…` around it. Characters U+E000 and U+E001 never appear in a highlight.

**Pages.** `paginate` takes a `PageQuery` (see [Pagination](#pagination)); `per_page` is at most
`PROSPECT_MAX_PER_PAGE` (100), and `get` takes a limit of 1 to that value. A search ranks and counts every matching
row, so its cost grows with the number of matches: on large tables keep a `throttle:` on search routes.

### Drivers

| | SQLite | PostgreSQL | MySQL / MariaDB |
|---|---|---|---|
| Index | FTS5 table + triggers | generated `tsvector` + GIN | `FULLTEXT` |
| Ranking | `bm25` with column weights 10 / 5 / 2 / 1 | `ts_rank_cd` with weights A-D | `MATCH … AGAINST` relevance, no column weights |
| Stemming | `Language::English` (porter) | `Language::English` (`english`) | none |
| Left out of a search | nothing | nothing | words shorter than the server's `innodb_ft_min_token_size` (read once; 3 when it cannot be read) and InnoDB's default stopwords (`prospect::MYSQL_STOPWORDS`: `the`, `a`, `for`, `with` …); a text with no other word searches like an empty one. The default list is assumed: a server with its own `innodb_ft_server_stopword_table` or `innodb_ft_enable_stopword=OFF` differs (a required custom stopword finds nothing) |
| Highlights | FTS5 `highlight` / `snippet` | `ts_headline`, on the page's rows only | in Rust |

`PROSPECT_DRIVER=memory` is an in-process engine for tests: documents hold the declared columns only and are written
by model events (`Record` writes; see [Model events](#model-events)), searched with the same text rules, and every hit
is loaded from the database with the scope, `only_when` and filters applied again in SQL, so a stale document never
shows a record the database would not return. It does not take `query(…)`, and its string filter values are letters,
digits and `_ . : @ -` only (1 to 128 characters).

### Settings and commands

| `.env` | Default | Meaning |
|---|---|---|
| `PROSPECT_DRIVER` | `database` | `database` or `memory`; anything else stops the build |
| `PROSPECT_MAX_QUERY_LENGTH` | `200` | characters of search text used (1 to 1,000) |
| `PROSPECT_MAX_PER_PAGE` | `100` | the largest page or `get` limit (1 to 1,000) |
| `PROSPECT_BATCH` | `500` | rows per chunk when `Prospect::import` fills the memory engine (1 to 10,000) |

Values out of range are clamped with a warning. `app.prospect_with(ProspectSettings::from_env().max_per_page(50), …)`
sets them in code.

| Command | Does |
|---|---|
| `smeltery prospect:status` | the driver; per searchable model its rows, text columns and whether its index is there |
| `smeltery prospect:import [table…]` | SQLite: rebuild the FTS5 index from the table; PostgreSQL / MySQL keep theirs current (nothing to do) |
| `smeltery prospect:flush table` | remove a model's documents from the memory engine (refused by the `database` driver: its index follows the table) |

In code: `prospect.import::<Post>()`, `prospect.sync::<Post>(keys)` (for writes outside `Record` with the memory
engine), `prospect.flush::<Post>()`, and `prospect.paused(async { … })`, which runs a block with model listeners
switched off.

The search text is never logged; at DEBUG a search logs its table and the number of terms and filters.

### Testing search

With the `database` driver, searches work in tests on in-memory SQLite (`TestApp`) like in the app: FTS5 is part of
the bundled SQLite. `prospect::testing::fake(&app)` switches a `TestApp` to the memory engine and records what is
indexed:

```rust,no_run
# mod post {
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "posts")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub title: String,
#     pub published: bool,
# }
# impl ActiveModelBehavior for ActiveModel {}
# impl smeltery::prospect::Searchable for Model {
#     fn index(i: &mut smeltery::prospect::IndexSpec) { i.text("title"); i.only_when("published"); }
# }
# }
# use post::Model as Post;
use smeltery::db::prelude::*;
use smeltery::prospect::{ProspectExt as _, testing};
use smeltery::testing::TestApp;

fn drafts_are_not_indexed() {
    let app = TestApp::new(|b| b.prospect(|p| { p.model::<Post>(); }));
    let fake = testing::fake(&app);
    let db = app.db();
    let draft = app
        .block_on(Post::create(&db, post::ActiveModel {
            title: Set("Draft".into()),
            published: Set(false),
            ..Default::default()
        }))
        .unwrap();
    fake.assert_not_indexed::<Post>(draft.id);
    fake.assert_synced_times::<Post>(1);
}
# fn main() { let _ = drafts_are_not_indexed; }
```

## Sparks: live components

Sparks (`smeltery::sparks`) are interactive components written in Rust and Mold, with no JavaScript to write. A
Spark is a struct (its state) with `#[derive(Spark)]`, a view in `resources/views/sparks/<name>.mold.html`, and
actions in an `#[actions]` impl block. The page renders it on the server; the client runtime (`sparks.js`, served
by the app at `/_sparks/sparks.js`) sends clicks and input to the server, which runs the action, re-renders the
view and answers the new HTML, which the runtime morphs into the page.

```rust
use smeltery::prelude::*;
use serde::{Deserialize, Serialize};
# use smeltery::json;

#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(name = "counter")]          // view: resources/views/sparks/counter.mold.html
pub struct Counter {
    pub count: i64,                 // server-only: the page cannot set it
    #[spark(model)]
    pub step: i64,                  // wire:model may set it
}

#[actions]
impl Counter {
    /// Runs on the first render, with the props of `@spark("counter", { start: 5 })`.
    pub async fn mount(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        self.count = ctx.prop("start").unwrap_or(0);
        self.step = 1;
        Ok(())
    }

    pub async fn increment(&mut self) -> Result<()> {
        self.count += self.step;
        Ok(())
    }

    pub async fn add(&mut self, ctx: &mut SparkCtx, n: i64) -> Result<()> {
        self.count += n;
        ctx.dispatch("counted", json!({ "count": self.count }));
        Ok(())
    }

    #[guard(auth)]                  // only for a signed-in user
    pub async fn reset(&mut self) -> Result<()> {
        self.count = 0;
        Ok(())
    }
}
# fn main() {}
```

```text
{{-- resources/views/sparks/counter.mold.html --}}
<div>
  <p>Count: {{ count }}</p>
  <input type="number" wire:model.live="step">
  <button wire:click="increment">+{{ step }}</button>
  <button wire:click="add(10)">+10</button>
  <span wire:loading>Saving…</span>
</div>
```

A page shows it with `@spark("counter", { start: 5 })` and loads the runtime once with `@sparksScripts` (in the
layout's `<head>`; it also writes the CSRF meta tag). `app/sparks/mod.rs` registers the components and
`bootstrap/app.rs` wires them with `.sparks(app::sparks::register)` (the trait is `smeltery::sparks::SparksExt`, in
the prelude):

```rust
# // `pub mod counter;` names a file; the macro swaps it for the inline module of the sample above.
# macro_rules! sparks_mod {
#     (pub mod counter; $($rest:tt)*) => {
#         pub mod counter {
#             use smeltery::prelude::*;
#             use serde::{Deserialize, Serialize};
#             #[derive(Serialize, Deserialize, Default, Spark)]
#             #[spark(name = "counter")]
#             pub struct Counter {
#                 pub count: i64,
#                 #[spark(model)]
#                 pub step: i64,
#             }
#             #[actions]
#             impl Counter {}
#         }
#         $($rest)*
#     };
# }
# sparks_mod! {
// app/sparks/mod.rs
pub mod counter;

pub fn register(s: &mut smeltery::sparks::Sparks) {
    s.add::<counter::Counter>();
    // smeltery:sparks
}
# }
# fn main() {}
```

`#[spark(...)]` takes `name` (default: the struct name in snake case), `view` (default `sparks/<name>`) and
`stream` (see below). Props whose name is a field set that field before `mount`; every prop is readable with
`ctx.prop("name")`. The view renders like any Mold view: from the template files with hot reload in debug builds,
compiled into the binary in release builds.

**Actions** are the `pub async fn` methods of the `#[actions]` block that take `&mut self`; nothing else can be
called from the page. After `&mut self` an action may take `ctx: &mut SparkCtx`, then parameters deserialized from
the call (`wire:click="add(10)"`, `remove('a')`). `#[guard(auth)]` requires a signed-in user (`#[guard(guest)]`
the opposite). Three hooks (and `can_stream` for streamed components, below):
`async fn mount(&mut self, ctx: &mut SparkCtx)`,
`async fn updated(&mut self, ctx: &mut SparkCtx, field: &str)` (after a `wire:model` update) and
`async fn rendering(&mut self, ctx: &mut SparkCtx)` (before every render, `$refresh` included: load fresh data).

| In the view | Does |
|---|---|
| `wire:click="action"`, `wire:click="action(1, 'a')"`, `.prevent` | calls the action on click |
| `wire:submit="save"` | calls on form submit (the browser's submit is prevented) |
| `wire:keydown.enter="save"` | calls on that key (`enter`, `escape`, `tab`, `space`, arrows, letters) |
| `wire:model="field"` | binds an input to a `#[spark(model)]` field; sent with the next action |
| `wire:model.live`, `.debounce.300ms`, `.blur`, `.change` | sends after typing pauses (150 ms, or the given time), on blur, on change |
| `wire:loading`, `.remove`, `.class="…"`, `.attr="disabled"`, `.flex`, `.block` | shown / hidden / classes / attribute while a request runs |
| `wire:target="save,step"` | limits `wire:loading` to those actions and fields |
| `wire:poll.5s="action"` | calls the action (or re-renders) every interval while the tab is visible |
| `wire:key="…"` | keeps list items matched across re-renders |
| `wire:click="$refresh"` | re-renders without an action |

Inputs bound with `wire:model` show the component's state; text from inputs is converted to the field's type
(`"3"` → `3`, a checkbox → `bool`), and a value that does not fit shows "The step field is invalid." through
`@error("step")`. A value for a `#[spark(model)]` field is text, a number, a boolean, `null` or a list of those,
never an object, and it never sets a struct (a list that serde would read as one is refused too), so the page
cannot fill a struct field with keys the view does not show; `null` clears an optional one. A struct field takes
the keys listed in `#[spark(model(fields = "title, body"))]`, one at a time (`wire:model="form.title"`, or
`$spark.$set('form', { title: 'Hi' })` with listed keys only); any other key is refused (403).
When a re-render adds elements with `autofocus` (which browsers ignore on elements added after the page loaded),
the runtime focuses the first of them once, after filling its `wire:model` value, unless focus is on an element
outside that Spark (a field or button of another Spark, a link in the page).

`SparkCtx` gives actions `app()`, `db()`, `auth()`, `user_id()`, `session()`, `prop(name)`, `redirect(url)`
(the browser goes there: a path on this site such as `/posts/3`, or an address on `APP_URL`), `redirect_away(url)`
(another site, an `http://` or `https://` URL; any other target given to either fails the update with 500 and a
log line, so a URL from data never runs script), `dispatch(event, payload)` (a `CustomEvent` on `window`, the payload as its `detail`),
`flash(key, value)` and `validate(self)`. With `#[derive(Validate)]` on the component, `ctx.validate(self).await?`
checks its rules; on failure the component re-renders with the messages, which `@error("field")` inside its view
shows.

**Nesting:** a component's view may use `@spark("item", { key: post.id, … })`. Each child keeps its own state and
updates on its own; when the parent re-renders, existing children (matched by `key`, or by position) stay as they
are, new keys mount new children, and children left out disappear.

**Uploads:** a field `#[spark(upload(max = 2048, mimes = "png,jpg"))] pub photo: Option<TemporaryUpload>` (`max` in
kilobytes, `mimes` as file extensions) bound with `<input type="file" wire:model="photo">` receives the file as
soon as it is chosen. The file waits in `storage/framework/sparks/`; a file that is too large or of another
type shows its message through `@error("photo")`. `photo.store(ctx, "public/avatars").await?` moves it to
`storage/app/public/avatars/<random>.png` and returns `public/avatars/<random>.png`
(`store_as(ctx, dir, name)` picks the name). Only `public/…` and `private/…` directories are accepted; files under
`storage/app/public` are served at `/storage/…` after `smeltery storage:link`. Temp files are deleted 24 hours
after the upload (by a later upload request). `mimes` checks the extension of the file's name, not its content.
`store` keeps the name's extension only when it is on the list of safe extensions that `UploadedFile::store` uses
(`smeltery::http::is_safe_extension`); any other is stored as `.bin`.
One session may send 30 uploads and 100 MiB per 10 minutes (`s.upload_quota(files, bytes, per)` in
`app/sparks/mod.rs`), and one client address 120 uploads and 400 MiB in the same window
(`s.upload_address_quota(files, bytes)`; the address is `ClientInfo::ip`, which honours `TRUSTED_PROXIES`); past
either an upload answers 429 without reading the file.
`#[validate(...)]` rules work on upload fields too (with `#[derive(Validate)]` and `ctx.validate(self)`): `required`
passes once a file is chosen, `min`, `max` and `between` count kilobytes, and `mimes = "png,jpg"` checks the name's
extension.

**Push from the server:** a component declared with `#[spark(stream)]` listens on `GET /_sparks/stream` (one
connection per page). `Broadcast` (a handler argument, or `app.service::<Broadcast>()`) sends to it by component
name or instance id: `refresh()` makes the page re-fetch the component, `emit(event, payload)` fires a browser
event. A page subscribes with the stream token each render of the component carries (`wire:stream="…"`, valid for
the snapshot time to live); the hook `async fn can_stream(&mut self, ctx: &mut SparkCtx) -> Result<bool>` in the
`#[actions]` block decides per render whether the visitor gets one (default: yes), so a visitor it refuses cannot
subscribe. A token is bound to the session and the signed-in user of the render: the stream accepts it only with
that session's cookie, and ends when that session signs out (logout, `logout_other_devices`, a password change or
reset, `end_credentials`, in any process of the app) and when the tokens expire. After a sign-out the page does not
open it again; otherwise it opens it again with the tokens of its latest render, after 5 seconds, doubling with each
failure in a row up to 5 minutes. One client address (an IPv6 client by its /64) may open 60 streams a minute
(`s.stream_opens_per_minute(n)`, counted in process memory); past that a stream answers 429. Visitors behind one NAT
or proxy address share that budget, one stream per page load: raise it for such audiences. The stream carries refresh signals (the page then sends an ordinary update with its own session and
snapshot) and `emit` payloads, which every page subscribed to that name or id receives, so a payload never holds
private data. A process keeps at most 1000 streams open (`s.max_streams(n)`); past that a stream answers 503 and the
page retries. `emit` refuses event names starting with `anvil:` (it sends nothing and returns 0): they belong to
listeners (below). `refresh()` and `emit()` deliver to the pages connected to the sending process at once and return
how many those are; they also hand the message to the app's [PubSub](#pubsub-messages-between-processes), which
carries it to the pages held by the app's other processes, so an agent in a `work` process reaches the pages of a
`serve --no-agents` process. That hand-over never blocks the caller: it goes through a queue of 1024 messages, and
a message the queue or the driver cannot take is dropped, counted and logged; delivery to other processes is at most
once. A Watchfire agent can drive a live counter:

```rust,no_run
use smeltery::sparks::Broadcast;
use smeltery::watchfire::prelude::*;

pub fn register(w: &mut Watchfire) {
    w.every(5.secs(), "live-counter", |ctx| async move {
        if let Some(broadcast) = ctx.service::<Broadcast>() {
            broadcast.to("counter").refresh();
        }
        Ok(())
    });
}
# fn main() {}
```

**Listening to broadcasts:** with [Anvil](#broadcasting) installed, a `#[spark(stream)]` component reacts to
broadcast events without any JavaScript: a method of its `#[actions]` block marked
`#[on("anvil:<channel>", "<event>")]` runs when that event is broadcast on that channel, by any process of the app.

```rust
use smeltery::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(name = "order-status", stream)]
pub struct OrderStatus {
    pub order_id: i64,
    pub status: String,
}

/// The event's data.
#[derive(Deserialize)]
pub struct Shipped {
    pub tracking: String,
}

#[actions]
impl OrderStatus {
    #[on("anvil:private-orders.{order_id}", "App\\Events\\OrderShipped")]
    pub async fn shipped(&mut self, ctx: &mut SparkCtx, event: Shipped) -> Result<()> {
        self.status = format!("Shipped: {}", event.tracking);
        ctx.dispatch("order-shipped", smeltery::json!({ "id": self.order_id }));
        Ok(())
    }
}
# fn main() {}
```

- The channel is the full name (`private-…`, `presence-…` or a public name); `{order_id}` is the value of that state
  field when the component renders (a string or an integer of letters, digits and `_ - = @ , ;`). The event name is
  the name the event is sent with: `App\Events\OrderShipped` for `#[derive(BroadcastEvent)] struct OrderShipped` (see
  [Events](#events)).
- Each render asks the app's channel rules (`routes/channels.rs`, the same callbacks as `POST /broadcasting/auth`,
  with the visitor's session) whether this visitor may receive each channel; the stream asks again when it opens.
  Only allowed channels reach the page. An event sent with `.except(socket)` reaches listeners too.
- The page receives the event through its Sparks stream, as a message signed for that component instance, and sends
  it back with an ordinary update request; the method then runs with the event's data deserialized into its last
  argument (400 when it does not fit), with the visitor's session and a fresh snapshot, like an action. Before
  anything of the request runs, the server checks the signature, that the message is at most 60 seconds old, that a
  listener takes that event on that channel, and asks the channel rules again for the visitor (403 otherwise). At
  the listener's turn, after the request's other updates and calls (the page sends pending `wire:model` values with
  it), the channel is resolved from the state as it is then: when the state names another channel, nothing runs.
  Each message runs once, whatever snapshot of the component it comes with: the app's cache remembers it for 60
  seconds (with a shared cache store, across the app's processes), and a cache failure refuses it (503). A message
  that did not run because an earlier call of the same request failed validation can be sent again.
- A listener is not an action: the page cannot call it by name. It may take `&mut SparkCtx` and one more argument
  (the data), and takes no `#[guard]`. A method may carry several `#[on]`; a component at most 16.
- The browser also gets the window event `anvil:<event>` with the data (`detail`), once per listening component,
  for Alpine code that only shows something. It comes from the page itself and any script on the page can fire one:
  act on events in the listener, on the server.
- Delivery is at most once: an event broadcast while the page's stream is reconnecting does not reach the listener
  (the page refreshes its components after a reconnect). Event data over 64 KiB is not sent to listeners.
- Without Anvil installed, on a component without `stream`, with a `{field}` the struct does not have, or with
  `CACHE_STORE=null` (listeners need a cache store that keeps values), the app does not start.

**Alpine.js:** Alpine attributes (`x-data`, `x-show`, `@click`, …) work in Mold views as they are; where an event
name is also a Mold directive, write the long form (`x-on:error`). A server value inside Alpine code goes through
the `json` filter, never between quotes: `x-data="{ name: {{ name | json }} }"`, `@click="pick({{ id | json }})"`
(see [Mold templates](#mold-templates)); inside a Spark, `$spark` reads the state without any echo. With Alpine.js 3.13 or newer loaded after
`@sparksScripts` (both scripts `defer`), Alpine code inside a Spark reaches that Spark as `$spark`:

```text
{{-- resources/views/sparks/search.mold.html --}}
<div x-data="{ open: $spark.$entangle('open'), q: $spark.$entangle('query').live }">
  <p>Found <span x-text="$spark.total"></span></p>
  <button @click="$spark.more(10)">More</button>
  <button @click="open = !open">Filters</button>
  <div x-show="open">…</div>
  <input x-model="q">
</div>
```

| In Alpine code | Does |
|---|---|
| `$spark.total`, `$spark.$get('total')` | reads a field; Alpine updates when a response changes it or a `wire:model` input of that field changes |
| `$spark.step = 2` | sets a field, sent with the next request (like `wire:model`) |
| `$spark.$set('step', 2)` | sets a field and sends it now (`$set('step', 2, false)` waits for the next request) |
| `$spark.more(10)`, `$spark.$call('more', 10)` | calls an action with those parameters |
| `$spark.$refresh()`, `$spark.$commit()` | re-renders; sends the recorded field updates |
| `$spark.$entangle('open')` | in `x-data`: an Alpine property bound both ways to the field; Alpine's changes go with the next request |
| `$spark.$entangle('open').live`, `$spark.$entangle('open', true)` | the same, each change sent at once |
| `$spark.$id`, `$spark.$name`, `$spark.$el` | the instance id, the component name, the Spark's root element |

`$spark` goes through the same requests as `wire:*`, so it reaches only what the server allows: a field without
`#[spark(model)]` or a name that is not an action is refused (403) with the whole request, and a value set from
Alpine shows until the next response replaces it. `$spark.<name>` reads the field of that name when the state has
one; any other name calls the action of that name, except the `$` helpers, the built-in object names (`toString`,
`constructor`, …), `then`, `toJSON` and names starting with `__` (call those with `$call`). Read fields in bindings
(`x-text`, `:class`, …) and call actions from events (`@click`, …): a binding that calls an action
(`$spark.total()`) sends it each time the binding re-runs, and an action called while the morph renders the bindings
is not sent (the console shows a warning). With an Alpine older than 3.13 there is no `$spark`, and the console says
so. When a Spark re-renders with Alpine 3.13 or newer on the page, the morph keeps Alpine's state: `x-data` elements
keep their data, what Alpine renders (`x-text`, `x-show`, `:class`, …) is rendered from that data into the new HTML
before the diff (Alpine's `cloneNode`, as in Alpine's morph plugin), `x-model` inputs keep their values, and the
rows of `x-for` and the element of `x-if` stay. New elements from the server are initialised by Alpine. Elements
are matched by `wire:key`, `id`, or tag and position: give Alpine components in lists a `wire:key`. `sparks.js`
does not depend on Alpine: without Alpine on the page there is no `$spark`, and everything else is the same.

**Security:** the state travels in the page signed with a key derived from `APP_KEY` (HMAC-SHA256), so it cannot be
changed, but it can be read: secrets never go into component fields. The snapshot is bound to the session it was
rendered for (a hash of the session's CSRF secret) and to the signed-in user, and is accepted for the session
lifetime after it was issued (`s.snapshot_ttl(duration)`); every response issues a fresh one. A snapshot copied to
another browser, kept past a logout or a sign-in, or older than that answers 419 and the page reloads. Within its
own session an older snapshot of the page can be sent again, so an action checks permission itself, with
`ctx.auth()` and the database, for every record it touches: a field set in `mount` is not proof that the user may
act on it. Updates are `POST /_sparks/update` and uploads `POST /_sparks/upload`, web routes with the session and
the CSRF check. A changed state, a CSRF mismatch or an `APP_KEY` that changed answers 419 too. Updating a field
without `#[spark(model)]`, calling a method that is not an action, or an unknown component is refused (403 / 404)
and logged, and nothing runs. A request carries at most one component (`s.max_components(n)`) with at most 50 action calls
(`s.max_calls(n)`); a larger request answers 413 before anything runs. The limits are set on the registry in
`app/sparks/mod.rs` (`s.max_calls(20);` next to `s.add::<…>()`).

**Testing:** `smeltery::sparks::testing::TestSpark` drives a component on a page from `TestApp`:

```rust,no_run
# use smeltery::json;
# use smeltery::sparks::testing::TestSpark;
# let app = smeltery::testing::TestApp::new(|b| b);
let page = app.get("/counter");
let mut counter = TestSpark::from_html(&page.text(), "counter").unwrap();
counter.set("step", 2).call("increment", json!([])).send(&app);
assert!(counter.html().contains("Count: 7"));
assert_eq!(counter.data()["count"], 7);
```

`smeltery::sparks::testing::BroadcastSpy` records what a `Broadcast` pushes: `BroadcastSpy::of(app.app())`, then
`spy.pushes()` returns the messages since the last call (`target`, `kind`, `event`, `payload`;
`is_refresh_of("counter")`). It listens like an open page, so it counts in what `refresh()` returns.

## Alloy: React and Vue

Alloy (`smeltery::alloy`) is the server side of the [Inertia](https://inertiajs.com) protocol, version 3. A
controller returns a page, a component name with its props, instead of a Mold view, and Inertia's client packages
(`@inertiajs/react`, `@inertiajs/vue3`) render it in the browser. The first visit gets HTML from a Mold root template
with the page inside; every later visit (a link, a form, a reload) gets the page as JSON, and the client swaps the
component without reloading the browser page. `smeltery new --frontend react` and `--frontend vue` create apps set up
this way (see [The React and Vue starter kits](#the-react-and-vue-starter-kits)). Node.js builds the JavaScript only:
the server runs the Rust binary and serves the build from `public/build/`.

### Setup

```rust
use smeltery::alloy::{Alloy, AlloyExt as _, Props, SharedCtx};

/// The HTML of a first visit: resources/views/app.mold.html.
#[derive(smeltery::Mold, Default)]
#[mold("app")]
pub struct Root {}

/// Props every page gets.
async fn shared(ctx: SharedCtx) -> smeltery::Result<Props> {
    Ok(Props::new().with("app", smeltery::json!({ "name": ctx.app().settings().name })))
}

pub fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.alloy(
        Alloy::new()
            .root::<Root>()
            .entries(["resources/js/app.tsx"]) // what a bare @vite loads
            .share(shared),
    )
}
```

The root template loads the assets with `@vite` and places the page with `@alloy`:

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>My App</title>
    @vite
    @alloyHead
  </head>
  <body>
    @alloy
  </body>
</html>
```

`@alloy` renders `<script data-page="app" type="application/json">…</script><div id="app"></div>` (the page's JSON
escaped, so no prop can close the script element); `@alloy("root")` uses another id, which the client's
`createInertiaApp({ id })` must name too. `@alloyHead` renders an empty string. `@vite` renders the configured
entries and `@vite("resources/js/admin.ts", …)` the named ones (see [Vite](#vite)). In an app without `.alloy(…)`,
`@alloy` and `@vite` are template errors.

`Alloy` also has `.version(f)` (the asset version, below), `.all_errors()` (every message per field),
`.encrypt_history()` (see [Redirects and history](#redirects-and-history)), `.build_dir("build")` (the folder under
`public/` and the URL prefix of the build: letters, digits and `_ . / @ -`, no `..`; anything else stops the app at
boot) and `.hot_file(path)` (the dev-server marker, `storage/framework/vite.hot`). `.alloy(…)` adds a middleware to
every web route (inside the session and CSRF stack), sets the `XSRF-TOKEN` cookie that Inertia's client sends back
as the `X-XSRF-TOKEN` header, and names `X-Inertia` in the `Vary` header of every web response. API routes are not
touched.

### Rendering pages

```rust
use smeltery::alloy::{self, Page};

#[derive(serde::Serialize)]
pub struct Activity {
    pub accounts: u64,
}

pub async fn dashboard() -> Page {
    alloy::render("dashboard") // resources/js/pages/dashboard.tsx
        .with("greeting", "Welcome back")
        .optional("stats", || async { Ok(42) })
        .defer("activity", || async { Ok(Activity { accounts: 3 }) })
}
```

The component name is resolved by the client; in the kits it is the file under `resources/js/pages/` without its
extension. In debug builds a component without a `resources/js/pages/<name>.{tsx,jsx,vue,svelte}` file logs a warning
once.

A typed page is a struct whose fields are the props:

```rust
#[derive(serde::Serialize, smeltery::Alloy)]
#[alloy("posts/index")]
pub struct PostsIndex {
    pub titles: Vec<String>,
}

pub async fn index() -> PostsIndex {
    PostsIndex { titles: vec!["Hello".into()] }
}

// `into_page()` adds lazy props to it:
use smeltery::alloy::{Component as _, Page};

pub async fn index_with_count() -> Page {
    PostsIndex { titles: Vec::new() }
        .into_page()
        .defer("count", || async { Ok(0) })
}
```

The struct must serialize to a JSON object; `#[derive(Alloy)]` refuses enums, tuple and unit structs.

| Method | Sent |
|---|---|
| `.with(key, value)` | on full visits and on partial reloads that name it; serialized at once |
| `.with_lazy(key, f)` | like `.with`, computed only when it is sent |
| `.optional(key, f)` | only on partial reloads that name it (`router.reload({ only: ['stats'] })`) |
| `.defer(key, f)`, `.defer_in(group, key, f)` | not on the first visit; listed in the page's `deferredProps`, and the client asks for it right after (`<Deferred data="activity">`); one request per group |
| `.merge(key, value)`, `.prepend(key, value)`, `.deep_merge(key, value)`, `.match_on(key, "id")` | like `.with`, and the client merges the value into the one it has (infinite lists) |
| `.always(key, value)` | on every response, partial reloads included |

Lazy props are `FnOnce` closures (`Send + 'static`) returning a future of `smeltery::Result<T>`; they capture what
they need (a `Db` clone); the included ones run concurrently, and an error fails the request. A partial reload names
props with `X-Inertia-Partial-Data` / `-Except` (dot paths reach into values, `user.name`); a reload of another component gets
the full page. `.status(code)`, `.encrypt_history(bool)` and `.clear_history()` are page options.

**Every prop is public**: the page is readable in the HTML of a first visit and in the JSON of every visit. Never
pass models that hold secrets (password hashes, tokens), session values or the CSRF token; send a struct with the
fields the page needs. A client can ask for any prop a page declares, optional and deferred ones included, so
authorization belongs to the route's middleware and the controller.

### Shared props

`Alloy::share(f)` adds props to every page, under the page's own (a page prop with the same key wins); the page's
`sharedProps` lists their keys. `f` gets a `SharedCtx` with `app()`, `session()`, `auth()`, `headers()`, `uri()` and
`method()`. The kits share the signed-in user through an allow-list, never the model:

```rust
use smeltery::alloy::{Props, SharedCtx};
# mod app { pub mod models {
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "users")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub name: String,
#     pub email: String,
#     pub password: String,
#     pub remember_token: Option<String>,
# }
# impl ActiveModelBehavior for ActiveModel {}
# impl smeltery::auth::Authenticatable for Model {
#     fn auth_id(&self) -> i64 { self.id }
#     fn password_hash(&self) -> &str { &self.password }
#     fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
# }
# pub use Model as User;
# } }
use app::models::User;

/// The signed-in user as pages see it (`usePage().props.auth.user`).
#[derive(serde::Serialize)]
pub struct SharedUser {
    pub id: i64,
    pub name: String,
    pub email: String,
}

pub async fn shared(ctx: SharedCtx) -> smeltery::Result<Props> {
    let user = ctx.auth().user::<User>().await?.map(|u| SharedUser {
        id: u.id,
        name: u.name,
        email: u.email,
    });
    Ok(Props::new()
        .with("app", smeltery::json!({ "name": ctx.app().settings().name }))
        .with("auth", smeltery::json!({ "user": user })))
}
# fn main() {}
```

### Errors and flash

`props.errors` is on every page: the errors of the previous request's failed validation, the first message per
field (`{ "email": "The email field must be a valid email address." }`; `Alloy::all_errors()` sends arrays), and
`{}` without errors. Inertia's client posts forms as JSON (as `multipart/form-data` when a file is in them), and the
web stack answers an Inertia request whose `Valid<T>` fails, or whose handler returns
`Err(Error::validation(field, message))`, with a `303` back and the errors in the session; `useForm` shows them as
`form.errors.email`. With the `X-Inertia-Error-Bag: login` header the errors arrive as `{ "login": { … } }`. A page
prop named `errors` is an error in debug builds.

The page's `flash` (`usePage().flash` in the browser) holds every value the previous request flashed with
`session.flash(key, value)` whose key does not start with `_`, any JSON value. **Flash values are public like
props**: never flash secrets. Keys starting with `_` (the framework's errors, old input, the intended URL) never
reach a page.

CSRF needs nothing in the pages: the client reads the `XSRF-TOKEN` cookie (a masked token, new on every response, not
`HttpOnly`) and sends it as `X-XSRF-TOKEN`. When the token does not match, an Inertia request gets a `303` back with
`flash.error` = "The page expired. Please try again." instead of the 419 page, and its handler does not run.

### Redirects and history

- `Redirect::to(url)` and `Back` answer `303`. A `302` answer to an Inertia `PUT`, `PATCH` or `DELETE` becomes `303`.
  A redirect to a URL with a `#fragment` becomes `409` with `X-Inertia-Redirect`, and an empty `200` becomes a
  redirect back.
- `alloy::location(url)` leaves the app with a full page load (OAuth, a download, another site): `409` with
  `X-Inertia-Location` for Inertia visits, `303` otherwise. Pass only URLs the app chose, never user input.
- An Inertia `GET` of a Mold page (the Watchfire dashboard) gets `409` `X-Inertia-Location`: a full page load.
- `X-Inertia-Location` for the request itself and the page's `url` hold the request's path and query starting with
  exactly one `/`: leading slashes and backslashes become one `/` (`//evil.example/x` becomes `/evil.example/x`), so
  the client never resolves them to another site.
- `alloy::clear_history(&session)` sets `clearHistory` on the next page; the kits call it on logout.
- The browser keeps every page's props in its history. With `.encrypt_history()` (on the `Alloy` builder, or
  `.encrypt_history(true)` on a page) the client encrypts them with a key it drops on `clearHistory`, so the back
  button cannot show a signed-in page after logout. The client encrypts only in a secure context: over HTTPS, or on
  `127.0.0.1` / `localhost`; elsewhere it keeps the history unencrypted and logs a warning in the browser console.
  The kits with authentication turn it on.

### Vite

The kits build the JavaScript with [Vite](https://vite.dev). Their `vite.config.ts` has a small `smeltery()` plugin:
it builds into `public/build/` with `public/build/manifest.json`, and while the dev server runs (on
`127.0.0.1:5173`) it writes its URL to `storage/framework/vite.hot`.

- **Dev server**: in debug builds, while `storage/framework/vite.hot` exists, `@vite` loads the dev server's client
  and the entries from it (hot reloading). Release builds never read this file, and neither does any build under
  `APP_ENV=production`. Only an `http(s)` URL on `127.0.0.1`, `localhost` or `[::1]` is used; any other content is
  ignored, with one warning in the log.
- **Build**: with `public/build/manifest.json`, `@vite` writes the entries' CSS links, `modulepreload` links for the
  chunks they import, and their scripts, under `/build/`. Release builds read the manifest once; debug builds read it
  again when it changes.
- **Neither**: a template error in debug builds ("No Vite assets"), nothing under `APP_ENV=testing` (so `cargo test`
  needs no Node.js), and nothing in release builds, which log an error when the server starts.

`smeltery serve` logs where the assets come from when the server starts (console commands such as `migrate` do not).
The asset version is the first 32 hex characters of the SHA-256 of `public/build/manifest.json`, `""` without one or
while the dev server runs (`.version(f)` sets another). An Inertia `GET` whose `X-Inertia-Version` differs gets `409` with
`X-Inertia-Location` before its handler runs (the flash is kept for the next request), so the browser loads the new
build after a deploy.

### Testing pages

```rust
use smeltery::alloy::testing::{AlloyAssertions as _, AlloyRequests as _};
use smeltery::alloy::{self, Alloy, AlloyExt as _, Page};
use smeltery::testing::TestApp;
# #[derive(smeltery::Mold, Default)]
# #[mold("app")]
# struct Root {}

async fn dashboard() -> Page {
    alloy::render("dashboard")
        .with("greeting", "Hello")
        .defer("activity", || async { Ok(vec!["Signed in"]) })
}

let app = TestApp::new(|app| {
    app.alloy(Alloy::new().root::<Root>())
        .routes(|r| { r.get("/dashboard", dashboard); })
});
app.get_alloy("/dashboard")
    .assert_component("dashboard")
    .assert_prop("greeting", "Hello")
    .assert_missing("activity")
    .assert_deferred("default", &["activity"]);
let reload = app.reload_alloy("/dashboard", "dashboard", &["activity"]);
assert_eq!(reload.prop("activity.0"), Some(smeltery::json!("Signed in")));
```

`get_alloy(path)` visits a page as Inertia's client does (with the current asset version), `reload_alloy(path,
component, &[props])` is a partial reload, and `post_alloy(path, &json)` posts JSON as `form.post` does. On any
response, `alloy_page()` reads the page (the JSON of an Inertia visit or the `<script data-page>` of a first visit),
`prop(path)` reads a prop by dot path, and `assert_component`, `assert_prop`, `assert_missing` and `assert_deferred`
check it. `assert_page_file_exists(component)` checks that `resources/js/pages/<component>.{tsx,jsx,vue,svelte}`
exists.

### The React and Vue starter kits

`smeltery new my-app --frontend react` (or `vue`) writes the Rust side of a Mold app with Alloy instead of Mold pages
and Sparks:

- `bootstrap/app.rs` installs `.alloy(…)` with the root template `resources/views/app.mold.html`, the entry
  `resources/js/app.tsx` (Vue: `resources/js/app.ts`), `.encrypt_history()` (with authentication) and the shared props
  of `app/providers/alloy.rs` (`app`, and `auth.user` through `SharedUser`). Apps with the Watchfire building block
  also call `.sparks(|_| {})`, for the Watchfire dashboard's live panels.
- `resources/js/`: the entry, `pages/` (React: `welcome.tsx`, `dashboard.tsx`, `auth/login.tsx`, …; Vue:
  `Welcome.vue`, `Dashboard.vue`, `auth/Login.vue`, …), `layouts/` (the app layout with
  the navigation and the flash messages, the auth layout), `components/` and `types/global.d.ts` (the shared props'
  and the flash's TypeScript types). The forms use `useForm` and show `form.errors`; the welcome page loads an
  optional prop with `router.reload({ only: ['forge'] })`, and the dashboard shows a deferred prop with
  `<Deferred>`.
- `package.json` (exact versions: Vite 8.3.2, `@inertiajs/react` or `@inertiajs/vue3` 3.8.0, React 19.3.0 or Vue
  3.5.43, TypeScript 6.0.3) with the scripts `dev`, `build` and `types` (`tsc --noEmit`, Vue: `vue-tsc --noEmit`),
  `tsconfig.json` and `vite.config.ts`.
- `tests/http.rs` tests every page with the helpers above.

The generators write pages in these apps (the kit is `frontend` in the app's `Cargo.toml`, under
`[package.metadata.smeltery]`):

| Command | Writes |
|---|---|
| `smeltery make:model Post title:string body:text? published:bool --all` (or `-r`) | `app/controllers/posts.rs` returning the pages, `resources/js/pages/posts/index.tsx`, `create.tsx`, `show.tsx`, `edit.tsx` (Vue: `posts/Index.vue`, `Create.vue`, `Show.vue`, `Edit.vue`) and `resources/js/types/post.ts`, besides the model, migration, factory, seeder and routes |
| `smeltery make:controller Reports [--resource]` | a controller returning `reports/index` (Vue: `reports/Index`) and its page, or the resource pages of an existing model |
| `smeltery make:page About` | `app/controllers/about.rs`, `resources/js/pages/about.tsx` (Vue: `About.vue`) and the route `GET /about` named `about` |

The forms use `useForm`, send number fields as numbers, post `multipart/form-data` when the model has a `file` field,
and show the messages of a failed validation under each field. `make:spark` refuses in these apps; `make:page` refuses
in Mold apps. The apps' `CLAUDE.md`, `AGENTS.md` and Bellows guides describe Alloy pages, and the Bellows skill
`alloy-page` replaces `spark`.

Inertia's `prefetch` links work with Alloy: a prefetch is an ordinary `GET` with `Purpose: prefetch`. The client
keeps a prefetched page for 30 seconds by default, so after a form changes a record, a page prefetched before can
show the old data until the cache expires. A prefetch request takes the
session's flash like any request: when it runs between the request that flashes and the page that shows the flash,
the flash goes into the prefetched response. The kits' pages do not prefetch.

## Broadcasting

Anvil (`smeltery::anvil`) is a WebSocket server inside the app that speaks the Pusher Channels protocol 7, on the
app's own port. Handlers, jobs and Watchfire agents send events to channels; clients subscribe to channels and
receive them. `pusher-js`, `laravel-echo` and the Pusher client libraries for other platforms connect to it with a
custom host.

### Setup

`bootstrap/app.rs` installs it with the app's channels:

```rust
use smeltery::anvil::{ChannelCtx, Channels};
use smeltery::prelude::*;

/// `routes/channels.rs`: the public channels and who may join the private ones.
pub fn channels(c: &mut Channels) {
    c.public("news");
    c.public("scores.{game}");
    c.private("orders.{order}", |ctx: ChannelCtx| async move {
        let order: i64 = ctx.param("order")?;
        let Some(user_id) = ctx.user_id() else { return Ok(false) };
        // Look the order up and allow its owner only.
        let rows = ctx
            .db()?
            .query_with("SELECT user_id FROM orders WHERE id = ?", [order.into()])
            .await?;
        let owner: Option<i64> = rows.first().and_then(|row| row.try_get("", "user_id").ok());
        Ok(owner == Some(user_id))
    });
}

pub fn build(app: AppBuilder) -> AppBuilder {
    app.anvil(channels)
}
# fn main() { let _ = build; }
```

`.anvil(channels)` adds:

- `GET /app/<ANVIL_APP_KEY>`: the socket endpoint, outside the web stack (no session, no cookies read). The app
  logs the path at start (`anvil: socket endpoint`); `Anvil::app_key()` returns the key.
- `POST /broadcasting/auth`: a web route (session and CSRF, `throttle:600,1`) that signs private subscriptions.
- `POST /api/broadcasting/auth`: the same for clients with a bearer token (an API route: no session, no cookie,
  no CSRF check; `throttle:600,1`); see [Mobile apps and other clients](#mobile-apps-and-other-clients). It answers
  404 when the app has no stateless guard.
- the `Anvil` service: a handler takes `anvil: Anvil`; jobs and agents use `Anvil::of(&app)` or
  `ctx.service::<Anvil>()`.

An invalid or repeated channel pattern, an invalid `ANVIL_*` value, or `ANVIL_MAX_CONNECTIONS` not below
`SERVER_MAX_CONNECTIONS` stops the app at boot.

### Channels

- **Public channels** are declared with `c.public(pattern)`. Anyone with the app key can subscribe to them, so their
  events are public. A public name that matches no declared pattern is refused (`pusher:subscription_error` with
  status 403).
- **Private channels** are `private-<name>`, declared with `c.private(pattern, callback)` (the pattern without the
  prefix: `orders.{order}` serves `private-orders.7`). Before it subscribes, the client posts `socket_id` and
  `channel_name` (form fields or JSON) to `/broadcasting/auth` with the session cookie and the CSRF token
  (`X-CSRF-TOKEN`, `X-XSRF-TOKEN` or `_token`). The callback decides; `Ok(true)` answers `200` with
  `{"auth": "…"}`, which the client sends with its subscription.
- Patterns are dot-separated; `{name}` matches one segment of letters, digits and `_ - = @ , ;`. A channel name is 1
  to 164 characters of letters, digits and `_ - = @ , . ;`.
- Guests are refused before the callback runs; `.guests()` on a private registration lets them through
  (`ctx.user_id()` is `None`).
- No matching pattern, a guest, `Ok(false)`, and an error from the callback with a 4xx status (a parameter that does
  not parse, a record not found) all answer the same `403 {"error":"Forbidden"}`, so the endpoint never tells which
  channels exist. An error with a 5xx status is logged and answered with that status. Every answer carries
  `Cache-Control: no-store`.
- Channel names starting with `private-encrypted-` are refused.

`ChannelCtx` gives the callback:

| Method | Returns |
|---|---|
| `ctx.param::<T>("order")` | the pattern parameter, parsed (a value that does not parse is a 403) |
| `ctx.user_id()` | the signed-in user's id, `None` for a guest |
| `ctx.user::<User>().await?` | the signed-in user (loaded once) |
| `ctx.db()?` | the app's database |
| `ctx.app()`, `ctx.channel()`, `ctx.socket_id()` | the app, the full channel name, the socket that subscribes |

The signature in `auth` is HMAC-SHA256 under `ANVIL_APP_SECRET` (by default a secret derived from `APP_KEY`), over
the socket id, the channel and who was authorized (the user id, a hash naming the session or the token's id, the
time it was made and an expiry five minutes ahead). It works only for that socket and that channel, and only within those five minutes; the server checks it in
constant time.

### Presence channels

A presence channel (`presence-<name>`) is a private channel whose members see each other: who is in a chat room, who
is looking at a document. `c.presence(pattern, callback)` declares it; the callback returns the `Member` the user
joins as, or `None` to refuse:

```rust
use smeltery::anvil::{ChannelCtx, Channels, Member};
use smeltery::json;

pub fn channels(c: &mut Channels) {
    c.presence("rooms.{room}", |ctx: ChannelCtx| async move {
        let room: i64 = ctx.param("room")?;
        let Some(user_id) = ctx.user_id() else { return Ok(None) };
        // … check that the user may enter room `room` …
        let _ = room;
        Ok(Some(Member::new(user_id).info(json!({ "name": format!("User {user_id}") }))))
    })
    .whispers();
}
# fn main() { let _ = channels; }
```

- **A member** is a user id (a number becomes its decimal string; 1 to 128 bytes) and `user_info`, which every member
  of the channel receives: give display fields only (a name, an avatar URL), never an email address or anything
  private.
- **Authorizing** works as for private channels (the same two endpoints, the same 403 rules, `.guests()`); the answer
  is `200 {"auth": "…", "channel_data": "{\"user_id\":\"7\",\"user_info\":{…}}"}` and the client sends both with
  its subscription. The signature covers `channel_data`, so a client cannot change who it appears as. A callback
  that returns a member whose `channel_data` is larger than `ANVIL_MAX_MEMBER_BYTES` (1,024), or whose user id is
  empty or longer than 128 bytes, gets a 500 (logged) and no signature.
- **Subscribing** answers `pusher_internal:subscription_succeeded` with the members:
  `{"presence": {"ids": ["7", "9"], "hash": {"7": {…}, "9": {…}}, "count": 2}}`. One user with several sockets is one
  member. When a user's first socket joins, the other members get `pusher_internal:member_added` with
  `{"user_id", "user_info"}`; when its last socket leaves (it unsubscribes, its connection closes or drops, it is
  closed by a revocation, its process shuts down), they get `pusher_internal:member_removed` with `{"user_id"}`.
- **Limits:** a channel holds at most `ANVIL_MAX_PRESENCE_MEMBERS` (100) distinct users; a new user beyond that gets
  `pusher:subscription_error` with status 403 ("presence channel full"), while sockets of users already in it still
  join. A socket is in at most `ANVIL_MAX_PRESENCE_CHANNELS` (10) presence channels and may join 5 at once, then one a
  second (each join and leave writes to the member store and reaches every member; subscribing again to a channel
  it is in reads the member list and counts as a join); beyond either it gets status 429.
- **Across processes**, the members live where the app's [PubSub driver](#pubsub-messages-between-processes) says:
  in the process's memory with `local`; with `database`, in the tables `presence_sockets` and `presence_users` (a
  migration calls `smeltery::anvil::presence_migrations::up` / `down`); with `redis`, in Redis (feature `redis`).
  Every serving process marks its members alive every 30 seconds and removes the members of a process unseen for
  90 seconds (a crashed process), announcing `member_removed`; at shutdown a process removes its own. The same
  heartbeat checks its members against its sockets: a join or leave the database cut off halfway is finished, and
  members other processes removed while this one could not reach the database come back (with `member_added`).
  Apps with many presence members or many client events use `PUBSUB_DRIVER=redis`: with `database`, every join,
  leave and member event is a few rows. With
  `database`, the member limit is checked before the join, so joins of new users in several processes at the same
  moment can pass it together. A socket that joins while another process announces a member can receive that
  `member_added` after its member list, for a member already in the list.
- `anvil.members("presence-rooms.1").await?` returns the members (every process's) to server code.
- An event goes to a presence channel's members with `Channel::presence("rooms.1")` or
  `#[broadcast(presence = "rooms.{room}")]`.

### Client events

A private or presence registration with `.whispers()` lets its subscribers send client events to each other
(typing indicators, cursors): `{"event": "client-typing", "channel": "private-chat.1", "data": …}` from a socket
subscribed to that channel. `laravel-echo` sends them with `.whisper("typing", data)` and receives them with
`.listenForWhisper("typing", …)`.

- The other subscribers of the channel receive it, in every process, with `data` as it was sent; on a presence
  channel it also carries the sender's `"user_id"`. The sender never receives its own.
- A client event never reaches the app's code: it goes from socket to socket.
- The name starts with `client-` and is at most 200 bytes; the frame is at most `ANVIL_MAX_MESSAGE_SIZE`.
- A socket may send 10 client events a second, a channel carries at most 100 a second in each process, one client
  address (an IPv6 client by its /64) sends at most `ANVIL_CLIENT_EVENTS_PER_CLIENT` (50) a second over all its
  sockets, and a process accepts at most `ANVIL_CLIENT_EVENTS_PER_SECOND` (500) a second in all; more are dropped
  with `pusher:error` 4301. Each accepted client event also reaches the app's other processes (a row in
  `pubsub_messages` with the `database` driver). Keep channels with client events small: each event reaches every
  other subscriber.
- On a public channel, a channel the socket is not subscribed to, or a pattern without `.whispers()`, a client event
  is answered with `pusher:error` 4009; the socket stays open.

### Events

An event is a struct that serializes to its data:

```rust
use serde::Serialize;
use smeltery::anvil::{Anvil, BroadcastEvent, SocketId};
use smeltery::prelude::*;

#[derive(Serialize, BroadcastEvent)]
#[broadcast(private = "orders.{order_id}")]
pub struct OrderShipped {
    pub order_id: i64,
    pub tracking: String,
    #[serde(skip)]
    pub internal_note: String,
}

async fn ship(anvil: Anvil, socket: Option<SocketId>) -> Result<&'static str> {
    // … mark order 7 shipped …
    anvil
        .send(&OrderShipped { order_id: 7, tracking: "1Z999".into(), internal_note: String::new() })
        .except(socket)
        .await?;
    Ok("shipped")
}
# fn main() { let _ = ship; }
```

- `#[broadcast(public = "…")]`, `#[broadcast(private = "…")]` and `#[broadcast(presence = "…")]` name the channels
  (several are allowed); `{field}`
  is the field's `Display` value, and a field the struct does not have is a compile error.
- The event name is `App\Events\<TypeName>` by default: `laravel-echo`'s `.listen("OrderShipped")` listens for that
  name. `#[broadcast(as = "order.shipped")]` sets another; `laravel-echo` listens for it with a leading dot
  (`.listen(".order.shipped")`).
- Names starting with `pusher:` or `pusher_internal:` are refused (the protocol reserves them). A `{field}` value
  goes into the channel name as it is, so a value with a `.` adds a segment: use fields such as ids. A channel named
  twice in one event is sent once.
- The data is the struct's JSON (`serde` attributes apply; `#[serde(skip)]` keeps a field out). Data on a public
  channel is public.
- Without a derive: `impl BroadcastEvent for X { fn channels(&self) -> Vec<Channel> { … } }` (the name defaults as
  above), or an event without a type:
  `anvil.to(Channel::public("news")).event("posted").with(&json!({ "id": 3 })).await?`.
- `send(...).await` returns `Delivered { local }`, the sockets of this process the event was queued for. It fails
  when the data is larger than `ANVIL_MAX_EVENT_SIZE`, a channel name is invalid, the name is empty or longer than
  200 bytes, or the app's PubSub driver fails (the sockets of this process got the event then; the other processes
  did not). A failed check sends nothing.
- **Other processes:** the event also goes out on the app's [PubSub](#pubsub-messages-between-processes) (topic
  `anvil`); every serving process hands it to its own sockets. A job in a `work` process reaches sockets held by
  `serve --no-agents` when both use a shared `PUBSUB_DRIVER`.
- Delivery is at most once, in order per sending process. A client that reconnects fetches the current state again.
- An event sent inside a database transaction is delivered at once, before the commit; send after it.

**Leaving the sender out.** A client sends its socket id with its own requests in the `X-Socket-ID` header.
`SocketId` reads it (`Option<SocketId>` is `None` without it; only `<digits>.<digits>` is accepted), and
`.except(socket)` leaves that socket out of the event. A client can name another socket's id, which keeps one event
from that socket and reveals nothing else.

### Clients

A client connects to `ws://<host>/app/<ANVIL_APP_KEY>?protocol=7` (`wss://` behind HTTPS) and receives
`pusher:connection_established` with its `socket_id` and `activity_timeout` (`ANVIL_ACTIVITY_TIMEOUT`). In
`pusher-js` and `laravel-echo` options: the key, the host as `wsHost`, the port as `wsPort` / `wssPort`, `forceTLS`
for `https`, `enabledTransports: ["ws", "wss"]`, and the auth endpoint `/broadcasting/auth` with the CSRF token in
its headers. Events arrive as `{"event": name, "channel": channel, "data": "<the event's JSON as a string>"}`.

A browser's socket is accepted when its `Origin` is `APP_URL`'s origin (its scheme, host and port; a path in
`APP_URL` does not count) or listed in `ANVIL_ALLOWED_ORIGINS`
(scheme, host and port compared exactly; non-web schemes such as `capacitor://localhost` can be listed) or in
`CORS_ALLOWED_ORIGINS`. `null` is accepted only when one of the two lists names it. Other origins get
`pusher:error` 4009 and a close with 4009. A socket without an `Origin` (a native app, a
server) is accepted: a socket carries no identity, so it gets public channels and the private channels it holds a
signature for. Under `APP_ENV=local` with a loopback `APP_URL`, the loopback names of `APP_URL`'s origin
(`localhost`, `127.0.0.1`, `[::1]`, same port) count as that origin.

`pusher:signin` (user authentication) is answered with `pusher:error` 4009; the socket stays open.

### Mobile apps and other clients

A native app, a script or another backend connects to the same socket endpoint (it sends no `Origin`) and
authorizes private channels at `POST /api/broadcasting/auth` with `Authorization: Bearer <token>`, form fields or
JSON `socket_id` and `channel_name`, as the Pusher client libraries send them. The token is checked by the app's
stateless guards (a bearer-token guard such as Hallmark's `hallmark`, see [API tokens](#api-tokens)), in registration
order, through `auth::authenticate(&app, &mut parts, GuardSet::Stateless)`; the session and its cookie are never
read there.

- No valid token: `401 {"error":"Unauthenticated."}` with `WWW-Authenticate: Bearer`.
- A token without the ability `broadcasting` (or `*`): the endpoint's uniform `403 {"error":"Forbidden"}`.
- Otherwise the channel rules are the cookie endpoint's: the pattern's callback decides, `ctx.user_id()` is the
  token's user, `ctx.user::<User>()` loads it and `ctx.principal()` gives the principal (guard, credential,
  abilities). The answer is `200 {"auth": "…"}`.
- The signature names the token by its key (`<guard>:token:<id>`, such as `hallmark:token:12`), and expires after
  five minutes or when the token does, whichever comes first.

### Flutter

Flutter and Dart apps connect with the [`dart_pusher_channels`](https://pub.dev/packages/dart_pusher_channels)
package, a Pusher protocol 7 client in pure Dart that takes a custom host, and sign in with Hallmark tokens over
`http`. [`examples/flutter-client`](examples/flutter-client) is a complete app.

```sh
flutter pub add dart_pusher_channels http
```

**Signing in.** `POST /api/tokens` (see [API tokens](#api-tokens)) answers `201` with the token; keep it in the
platform's secure storage (the `flutter_secure_storage` package, for example) rather than in plain preferences.

```dart
import 'dart:convert';

import 'package:dart_pusher_channels/dart_pusher_channels.dart';
import 'package:http/http.dart' as http;

final api = Uri.parse('https://app.example.com');

Future<String> signIn(String email, String password) async {
  final res = await http.post(
    api.resolve('/api/tokens'),
    headers: {'Content-Type': 'application/json', 'Accept': 'application/json'},
    body: jsonEncode({'email': email, 'password': password, 'device_name': 'Pixel 9'}),
  );
  if (res.statusCode != 201) throw Exception('sign-in failed: ${res.statusCode}');
  return jsonDecode(res.body)['token'] as String;
}
```

**Connecting.** The key is the app's `ANVIL_APP_KEY` (set it in `.env`: the derived key changes with `APP_KEY`). In
production the socket is `wss` on the HTTPS host and port; in development it is `ws` on `SERVER_PORT`, and the
Android emulator reaches the development machine's `127.0.0.1` at `10.0.2.2` (`ws://10.0.2.2:8000`; Android refuses
plain `http` / `ws` unless the app's manifest allows cleartext traffic, which the example does in debug builds only).

```dart
final client = PusherChannelsClient.websocket(
  options: PusherChannelsOptions.fromHost(
    scheme: 'wss',
    host: 'app.example.com',
    port: 443,
    key: 'your-anvil-app-key', // ANVIL_APP_KEY
  ),
  connectionErrorHandler: (exception, trace, refresh) => refresh(),
);
```

**Channels.** Private and presence channels authorize at `/api/broadcasting/auth` with the bearer token; the
package's delegate posts `socket_id` and `channel_name` as form fields:

```dart
final authUrl = api.resolve('/api/broadcasting/auth');
final headers = {'Authorization': 'Bearer $token', 'Accept': 'application/json'};

final news = client.publicChannel('news');
final orders = client.privateChannel(
  'private-orders.7',
  authorizationDelegate: EndpointAuthorizableChannelTokenAuthorizationDelegate.forPrivateChannel(
    authorizationEndpoint: authUrl,
    headers: headers,
    onAuthFailed: onAuthFailed,
  ),
);
final room = client.presenceChannel(
  'presence-rooms.1',
  authorizationDelegate: EndpointAuthorizableChannelTokenAuthorizationDelegate.forPresenceChannel(
    authorizationEndpoint: authUrl,
    headers: headers,
    onAuthFailed: onAuthFailed,
  ),
);

// The package reconnects after a close but does not subscribe again by itself: subscribe on every connection.
final channels = <Channel>[news, orders, room];
client.onConnectionEstablished.listen((_) {
  for (final channel in channels) {
    channel.subscribeIfNotUnsubscribed();
  }
});
await client.connect();
```

**Events** arrive with their name and the event's JSON:

```dart
orders.bind(r'App\Events\OrderShipped').listen((event) {
  final data = event.tryGetDataAsMap(); // {"order_id": 7, "tracking": "1Z999"}
});
```

**Presence members.** `room.state?.members` holds the members' ids, but the package drops the `user_info` of the
list that comes with the subscription; read it from the `pusher:subscription_succeeded` event's data, and follow
`member_added` / `member_removed`:

```dart
final names = <String, String>{};
room.whenSubscriptionSucceeded().listen((event) {
  final hash = event.tryGetDataAsMap()?['presence']?['hash'] as Map? ?? {};
  names
    ..clear()
    ..addAll({for (final e in hash.entries) '${e.key}': '${e.value['name']}'});
});
room.whenMemberAdded().listen((event) {
  final data = event.tryGetDataAsMap()!;
  names['${data['user_id']}'] = '${data['user_info']['name']}';
});
room.whenMemberRemoved().listen((event) => names.remove('${event.tryGetDataAsMap()!['user_id']}'));
```

**Whispers** (client events, on a registration with `.whispers()`): the other subscribers receive them with the
sender's `user_id` on a presence channel; the sender does not.

```dart
room.trigger(eventName: 'client-typing', data: {'typing': true});
room.bind('client-typing').listen((event) => print('${event.userId} is typing'));
```

**Revocation and signing out.** When the token is signed out or revoked elsewhere, the server closes the socket with
4200; the package reconnects, authorizing the channels again answers `401`, and `onAuthFailed` gets the exception:
the app returns to its sign-in. Signing out on the device deletes the token:

```dart
void onAuthFailed(dynamic exception, StackTrace trace) {
  if (exception is EndpointAuthorizableChannelTokenAuthorizationException &&
      exception.response.statusCode == 401) {
    // The token has ended: back to the sign-in.
  }
}

Future<void> signOut() async {
  await http.delete(api.resolve('/api/tokens/current'), headers: headers); // 204
  await client.disconnect();
  client.dispose();
}
```

### Revocation

A private subscription ends when the credential that authorized it ends. Every serving process listens to the app's
auth events (PubSub topic `auth`, see [PubSub](#pubsub-messages-between-processes)): signing out ends the
subscriptions authorized by that session's cookie endpoint requests; a password change or `logout_other_devices`
ends every credential of the user except the one named; a token its guard revokes ends the subscriptions it
authorized. The sockets holding such a subscription are closed with 4200 (the client reconnects at once and
authorizes its channels again, which now fails for the ended credential), and a signature made before the event
can no longer subscribe. That check also refuses a signature dated up to 30 seconds after the event, so a process
whose clock runs a little ahead cannot slip one through; the processes' clocks must agree (NTP). Each process
remembers the last 10,000 events for 15 minutes for that check, and refuses what it cannot check: a signature made
before the process started, before an event it forgot early (more than 10,000 events within 15 minutes), or before
the moment it started listening to them or fell behind them (its PubSub subscription lagged). When it falls
behind, it also closes every socket holding a subscription such a signature authorized with 4200, each at a random
moment within 30 seconds from the next second on, and at most once a minute (later lags join the next round), so
after a lag such a socket can keep its subscription for up to about 90 seconds (60 s for the per-minute limit plus
the 30 s spread). The clients authorize again and get new signatures.

Events are delivered at most once: a socket a lost event did not close lives at most `ANVIL_MAX_CONNECTION_AGE`
(24 hours by default), and a session that only expires (idle or absolute lifetime) sends no event, so its
subscriptions also end with the socket's age. Several processes need a shared `PUBSUB_DRIVER` for an event to reach
them all.

### Settings and limits

| Key | Default | Meaning |
|---|---|---|
| `ANVIL_APP_ID` | `smeltery` | the app id |
| `ANVIL_APP_KEY` | derived from `APP_KEY` (20 characters) | the public key in the socket path; letters, digits, `-`, `_`, at most 64. The derived key changes when `APP_KEY` changes: set it in `.env` when clients are built with it (a front end bundle, a mobile app) |
| `ANVIL_APP_SECRET` | derived from `APP_KEY` | the secret signatures are made with; set it only when another deployment must make them; at least 32 bytes outside `APP_ENV=local` / `testing`, else the app stops at boot |
| `ANVIL_ALLOWED_ORIGINS` | empty | origins allowed besides `APP_URL`'s, comma-separated; `*` allows any (logged as a warning outside `APP_ENV=local`) |
| `ANVIL_MAX_CONNECTIONS` | `2048` | sockets in this process (below `SERVER_MAX_CONNECTIONS`); past it a socket is closed with 4100 |
| `ANVIL_MAX_CONNECTIONS_PER_IP` | `100` | sockets of one client address (an IPv6 client by its /64, the address from `TRUSTED_PROXIES` resolution); past it 4100; below `SERVER_MAX_CONNECTIONS_PER_IP` when both are on, else the app stops at boot; `0` turns it off and leaves only the server's per-address cap |
| `ANVIL_HANDSHAKES_PER_MINUTE` | `60` | socket handshakes of one client address a minute; past it `429`. Once 100,000 addresses are counted in a minute, further ones are counted with their network (an IPv4 /16, an IPv6 /48) at 16 times the budget, and once 10,000 networks are counted, further clients are limited only by the socket caps |
| `ANVIL_MAX_SUBSCRIPTIONS` | `100` | channels per socket; past it the subscription gets status 429 |
| `ANVIL_MAX_MESSAGE_SIZE` | `10000` | bytes of one message from a client (256 to 1048576); a larger one closes the socket with 1009 |
| `ANVIL_MAX_EVENT_SIZE` | `32768` | bytes of one event's data (at most `49152`) |
| `ANVIL_MAX_PRESENCE_MEMBERS` | `100` | distinct users in one presence channel (1 to 10000); the member list holds at most this many |
| `ANVIL_MAX_MEMBER_BYTES` | `1024` | bytes of one member's `channel_data` (64 to 8192) |
| `ANVIL_MAX_PRESENCE_CHANNELS` | `10` | presence channels one socket is in (1 to 100) |
| `ANVIL_CLIENT_EVENTS_PER_SECOND` | `500` | client events this process accepts a second from all its sockets (1 to 100000); more are dropped with 4301 |
| `ANVIL_CLIENT_EVENTS_PER_CLIENT` | `50` | client events one client address (an IPv6 client by its /64) sends a second over all its sockets (1 to 100000); more are dropped with 4301 |
| `ANVIL_ACTIVITY_TIMEOUT` | `30` | seconds (1 to 3600); clients ping after this much silence |
| `ANVIL_PING_INTERVAL` | `60` | seconds of silence (1 to 3600) before the server sends `pusher:ping` |
| `ANVIL_PONG_TIMEOUT` | `30` | seconds (1 to 300) to answer it; then the socket is closed with 4201 |
| `ANVIL_MAX_CONNECTION_AGE` | `86400` | seconds (60 to 2592000) a socket lives; then it is closed with 4200 and the client reconnects |
| `ANVIL_IN_SERVE` | `true` | `false`: `serve` answers the socket path with 404, because an `anvil` process holds the sockets (see [A separate socket process](#a-separate-socket-process)) |
| `ANVIL_SERVER_HOST` | `127.0.0.1` | the address the `anvil` process listens on |
| `ANVIL_SERVER_PORT` | `8080` | the port the `anvil` process listens on |

A timer value outside its range in `.env` is replaced by the nearest bound, with a warning; outside its range in
`Settings` passed to `anvil_with`, it stops the app at boot. Every socket that joins a presence channel is sent the
whole member list in one frame, so `ANVIL_MAX_PRESENCE_MEMBERS` and `ANVIL_MAX_MEMBER_BYTES` together may allow a
list of at most 16 MiB (about 2 × member bytes + 600 bytes per member); a larger product stops the app at boot.

A socket may send 20 frames a second with bursts of 40; a frame over that is dropped with one `pusher:error` 4301,
and 40 more in a row close the socket with 4100. Each socket has an outbox of 256 messages; a socket that does not
read them is closed with 4100, and a write that takes longer than 10 seconds drops it. A binary frame closes it with
1003, the server's shutdown with 1001. pusher-js reconnects after codes below 4000 and from 4100 to 4299, and stops
after 4007, 4008 (protocol version unsupported or missing) and 4009.

A socket keeps its connection's place in `SERVER_MAX_CONNECTIONS` and `SERVER_MAX_CONNECTIONS_PER_IP`, so sockets
and HTTP requests share those limits, and the server's idle timeout does not close it. Sockets are closed with 1001
when the app shuts down, within `SHUTDOWN_TIMEOUT`. Frames are never logged.

### Behind a proxy

Caddy's `reverse_proxy` passes WebSocket upgrades as it is. nginx forwards `Upgrade` and `Connection` only when told
to; add this location to the server block (with the app's key):

```nginx
location = /app/ANVIL_APP_KEY_VALUE {
    proxy_pass http://127.0.0.1:8000;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Forwarded-Host $host;
    # Longer than ANVIL_PING_INTERVAL + ANVIL_PONG_TIMEOUT.
    proxy_read_timeout 120s;
    proxy_send_timeout 120s;
}
```

Behind a proxy every socket comes from the proxy's address; with the proxy in `TRUSTED_PROXIES`, the per-client
limits count the client from `X-Forwarded-For`.

### A separate socket process

`serve` holds the sockets by default. The `anvil` command runs the app binary as a process that serves only the
socket endpoint (`/app/<ANVIL_APP_KEY>`) and `/up`, on `ANVIL_SERVER_HOST:ANVIL_SERVER_PORT` (`127.0.0.1:8080`;
`--host` and `--port` override them); every other path answers 404. The web processes can then restart (a deploy)
without closing a socket.

```bash
smeltery anvil              # in development
/srv/myapp/myapp anvil      # the release binary
```

- The web processes run with `ANVIL_IN_SERVE=false`: their socket path answers 404. They keep the auth endpoints
  (`/broadcasting/auth`, `/api/broadcasting/auth`), which need the session and the guards; the `anvil` process
  checks the signatures they make with the same secret (derived from the same `APP_KEY`, or the same
  `ANVIL_APP_SECRET`) and the same `ANVIL_APP_KEY`.
- Events, revocations and presence reach the `anvil` process through the [PubSub](#pubsub-messages-between-processes).
  Under `PUBSUB_DRIVER=auto` the `anvil` process and a `serve` with `ANVIL_IN_SERVE=false` use the shared driver
  (`redis` when `CACHE_STORE=redis`, else `database`); without a usable shared driver `anvil` stops at start with an
  error. Console commands use `local` under `auto`, so a deployment whose commands send events sets
  `PUBSUB_DRIVER=database` or `redis`. The `database` driver needs the `pubsub_messages` table, presence the
  presence tables (see [Presence channels](#presence-channels)).
- No agents, jobs or schedules run in it; `serve` or `work` runs them.
- The server's limits hold on its listener as on `serve`'s (`SERVER_MAX_CONNECTIONS`,
  `SERVER_MAX_CONNECTIONS_PER_IP`, `SERVER_HEADER_TIMEOUT`, `TRUSTED_PROXIES`), with the `ANVIL_*` limits above. On
  SIGTERM it closes its sockets with 1001 and exits within `SHUTDOWN_TIMEOUT`; its presence members leave with them.
- With `ANVIL_IN_SERVE` left at `true` it logs a warning at start: `serve` processes hold sockets too.

**systemd.** A third unit next to the one of [systemd](#systemd-start-on-boot-restart-on-crash) (or next to
`myapp-web` and `myapp-work`): set `ANVIL_IN_SERVE=false` in `.env`, then

```bash
sudo cp /etc/systemd/system/myapp.service /etc/systemd/system/myapp-anvil.service
sudo nano /etc/systemd/system/myapp-anvil.service
```

and change `Description=myapp sockets (Smeltery)` and `ExecStart=/srv/myapp/myapp anvil`. Each socket holds a file
descriptor: for more than about a thousand sockets add `LimitNOFILE=65536` under `[Service]` and raise
`SERVER_MAX_CONNECTIONS` (4096) and `ANVIL_MAX_CONNECTIONS` (2048, below it) in `.env`.

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now myapp-anvil
sudo systemctl restart myapp
curl -s http://127.0.0.1:8080/up
```

**Caddy** sends the socket path to the `anvil` process and everything else to the web process:

```text
app.example.com {
    handle /app/* {
        reverse_proxy 127.0.0.1:8080
    }
    handle {
        reverse_proxy 127.0.0.1:8000
    }
}
```

**nginx:** the `location = /app/ANVIL_APP_KEY_VALUE` block above with `proxy_pass http://127.0.0.1:8080;`.

### Testing broadcasts

```rust
use serde_json::json;
use smeltery::anvil::testing::{AnvilSpy, TestSocket, auth_of, authorize};
use smeltery::anvil::{Anvil, AnvilExt as _, Channel, ChannelCtx};
use smeltery::testing::TestApp;

let app = TestApp::new(|b| {
    b.anvil(|c| {
        c.public("news");
        // In this test, user n owns order n.
        c.private("orders.{order}", |ctx: ChannelCtx| async move {
            Ok(ctx.user_id() == Some(ctx.param::<i64>("order")?))
        });
    })
});
let spy = AnvilSpy::of(app.app());
let mut socket = TestSocket::connect(app.app());

// A private subscription with the auth endpoint's answer.
app.acting_as(7);
let auth = auth_of(&authorize(&app, socket.socket_id(), "private-orders.7", None)).unwrap();
assert_eq!(
    socket.subscribe("private-orders.7", Some(&auth))["event"],
    "pusher_internal:subscription_succeeded"
);

let anvil = Anvil::of(app.app()).unwrap();
app.block_on(async {
    anvil.to(Channel::private("orders.7")).event("shipped").with(&json!({ "id": 7 })).await
})?;
assert!(spy.sent_on("private-orders.7", "shipped"));
assert_eq!(socket.events()[0]["data"], r#"{"id":7}"#);
# Ok::<(), smeltery::Error>(())
```

`AnvilSpy` records every send, the last 10,000 (`sent()`, `sent_on(channel, name)`, `nothing_sent()`); `TestSocket` connects to the
hub without a network (`subscribe`, `subscribe_presence`, `whisper`, `unsubscribe`, `send`, `events`, `closed`;
dropping it leaves its presence channels); `authorize` posts to the auth endpoint as the `TestApp`'s browser (with
`TestApp::with_csrf`, pass the CSRF token), and `presence_of` reads a presence answer's `auth` and `channel_data`.
`TestSocket` keeps presence members in the process's memory (the `TestApp`'s `local` PubSub driver).

## Watchfire: agents, jobs and the scheduler

Watchfire (`smeltery::watchfire`) runs the app's background work on one runtime: **agents** (long-lived, supervised
tasks such as pollers, scrapers and bots), **jobs** (short tasks dispatched from the app, on a queue) and **scheduled
tasks** (started by the clock). `app/agents/mod.rs` registers everything; `bootstrap/app.rs` wires it with
`.agents(app::agents::register)` (the trait is `smeltery::watchfire::AgentsExt`). `serve` runs the agents next to
the HTTP server and `work` runs them alone; both stop them within `SHUTDOWN_TIMEOUT`.

```rust,no_run
use smeltery::watchfire::prelude::*;

pub fn register(w: &mut Watchfire) {
    // Quick forms: closures, supervised like any agent.
    w.every(30.secs(), "heartbeat", |ctx| async move {
        ctx.log().info("tick");
        Ok(())
    });
    w.run("scraper", |ctx| async move {
        while ctx.sleep(5.secs()).await {
            let page = ctx.http().get("https://example.com/").await?.text().await?;
            ctx.counter("bytes").add(page.len() as i64);
        }
        Ok(())
    })
    .restart(Restart::OnFailure)
    .backoff(1.secs()..=30.secs())
    .group("scrapers");
    w.on_event("post.created", "notify", |ctx, event| async move {
        ctx.log().info(format!("new post: {}", event.payload));
        Ok(())
    });
    w.group("scrapers").limit(2);
    w.rate_limit("example.com", 2.per_second());
    w.schedule()
        .call("cleanup", |_ctx| async move { Ok(()) })
        .every(5.mins())
        .overlap(Overlap::Skip);
    w.schedule().agent("scraper").cron("0 3 * * *");
}
# fn main() {}
```

A full agent is a struct (`smeltery make:agent PricePoller`); its fields survive restarts:

```rust,no_run
use smeltery::watchfire::prelude::*;

/// Polls prices.
#[derive(Default)]
pub struct PricePoller {
    last: Option<String>,
}

impl Agent for PricePoller {
    fn name(&self) -> String {
        "price_poller".into()
    }

    fn config(&self) -> AgentConfig {
        AgentConfig::default()
            .restart(Restart::OnFailure)
            .backoff(1.secs()..=60.secs())
            .heartbeat_timeout(2.mins())
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        let mut ticker = ctx.interval(30.secs());
        while ticker.tick().await {
            // `tick` heartbeats, and returns false once the agent is asked to stop
            self.last = Some("42".into());
        }
        Ok(())
    }
}
# fn main() {}
```

Register it with `w.agent(price_poller::PricePoller::default())`, or a pool of them with
`w.pool(4, "fetcher", Fetcher::new)` (named `fetcher#0` … `fetcher#3`).

**Supervision.** States: `starting` (waiting for a group or global slot), `running`, `paused`, `stopping`,
`stopped`, `backing_off`, `completed`, `failed`, and `standby` (another process runs the agent; see "Several
processes"). The restart policy (`Restart::Never`, `OnFailure` (default),
`Always`) decides what happens when a run ends on its own; restarts wait an exponential backoff with full jitter
(a random delay up to `initial × 2ⁿ`, capped; a run of 60 s or more resets it). `.max_restarts(n, per)` marks the
agent `failed` after `n` restarts within `per`; `.heartbeat_timeout(d)` cancels a run that has not heartbeated
(`ctx.heartbeat()` or a ticker tick) for `d` (outcome `stalled`, restarted like a failure); `.shutdown_timeout(d)` (default 10 s) is how long a
cancelled run may take before it is dropped. A panic is caught and counts as a failure. Every run ends with exactly
one outcome: `completed`, `failed`, `panicked`, `stopped`, `killed` (did not stop in time), `stalled` (heartbeat
timeout) or `interrupted` (the process ended during the run; found on the next launch). Alerts (failed agents,
stalls) are logged at error level with the target `smeltery_watchfire::alert`.

**Control from code.** `Agents` (a service: `app.service::<Agents>()`, or a handler argument `agents: Agents`) has
`list`, `status`, `start`, `stop`, `pause` (stop and stay stopped until `resume`; kept in memory only, so a restart of
the process starts the agent again as `autostart` says), `resume`, `restart`, `add`,
`remove`, `logs` (the last 200 lines of `ctx.log()`), `runs`, `emit` and `subscribe` (status changes).

**What a run gets (`AgentCtx`).** `name()`, `run_id()`, cancellation (`cancelled()`, `is_cancelled()`, `token()`),
`heartbeat()`, `sleep(d)` and `interval(d)` (both return `false` once cancelled), `http()`, `rate_limited(host)`,
`app()`, `db()`, `service::<T>()`, `checkpoint(&state)` / `checkpoint_get::<T>()` (JSON that survives restarts),
`emit(event, payload)` (to `on_event` agents), `counter(name).inc()` / `.add(n)` (saved in the run record),
`log().info(..)` (a `tracing` event with `agent` and `run` fields, also kept for `Agents::logs`),
`spawn_child(name, agent)` (a supervised child named `<agent>.<name>`, stopped when the run ends) and
`dispatch(job)`.

**HTTP.** `ctx.http()` sends requests with a timeout (`WATCHFIRE_HTTP_TIMEOUT`, 30 s; 10 s to connect), retries
connect errors, timeouts, `429` and `5xx` up to 3 times with jittered backoff (honouring `Retry-After` up to 60 s;
`5xx` and timeouts are retried for `GET`, `HEAD`, `PUT`, `DELETE` and `OPTIONS` only), waits for the host's token
bucket from `w.rate_limit(host, rate)`, and sends `User-Agent: <APP_NAME> (smeltery-watchfire)`. A response body
larger than `WATCHFIRE_HTTP_MAX_BODY` (10 MiB) fails with `HttpError::TooLarge`; the body is read in chunks and the
read stops at the limit. The client follows redirects itself (`301`, `302`, `303`, `307`, `308`, at most 5 hops), so
each hop waits for the token bucket of its own host: only to `http` and `https` URLs, never from `https` to `http`,
and on a hop to another origin only `User-Agent`, `Accept` and `Accept-Language` go along (no `Authorization`, cookies
or key headers) and no request body (`HttpError::Redirect` otherwise). `.redirects(Redirects::SameOrigin)` stays on
one origin, `.redirects(Redirects::None)` returns the `3xx` answer. The default follows a redirect to any `http` or
`https` host, addresses on this machine and the private network included (without the headers and body above); an
agent that fetches URLs its users or other sites supply sets `.redirects(Redirects::SameOrigin)` on those requests
(or `HttpOptions::redirects` for its client), so a redirect cannot send it to an internal address. Errors and logs
name a URL as
`scheme://host:port/path`, without its query string or user info. Methods: `get(url)`,
`post_json(url, &body)`, `request(method, url)` with `.header`, `.bearer`, `.body`, `.json`, `.timeout`,
`.max_body(bytes)`, `.redirects(policy)`, `.send()`; responses have `status()`, `text()`, `json::<T>()`, `bytes()`,
`error_for_status()`.

**Jobs.** A job is a serializable struct (`smeltery make:job SendWelcome`), registered with `w.job::<SendWelcome>()`:

```rust,no_run
use serde::{Deserialize, Serialize};
use smeltery::watchfire::prelude::*;

#[derive(Serialize, Deserialize)]
pub struct SendWelcome {
    pub user_id: i64,
}

impl Job for SendWelcome {
    const NAME: &'static str = "send_welcome";

    fn max_attempts(&self) -> u32 {
        3 // the default
    }

    async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
        ctx.log().info(format!("welcoming user {}", self.user_id));
        Ok(())
    }
}

async fn register_user(app: smeltery::App) -> smeltery::Result<&'static str> {
    SendWelcome { user_id: 1 }.dispatch(&app).await?;
    SendWelcome { user_id: 1 }.dispatch_later(&app, 10.mins()).await?;
    Ok("welcome")
}
# fn main() {}
```

Queue workers (`queue#0` … `queue#n`, `WATCHFIRE_WORKERS`, default 2) take jobs one at a time. The `database` driver
reserves a job with one atomic `UPDATE … WHERE reserved_at IS NULL`, so no two workers hold the same job;
reservations older than twice `WATCHFIRE_JOB_TIMEOUT` (a worker that died) are released, and the job can then run
again. A worker's outcome (done, retry, dead letter, back to the queue) is stored only while its reservation is still
the one held: a worker whose reservation was released and taken by another one changes nothing and logs a warning.
Jobs run at least once, so a job with side effects checks whether its work is already done; give every process the
same `WATCHFIRE_JOB_TIMEOUT`. A failed attempt (an error,
a panic, or running longer than `WATCHFIRE_JOB_TIMEOUT`) is retried after `Job::backoff(attempt)` (5 s, doubling, at
most 10 minutes); after `max_attempts` the job moves to the dead letters, as does a job with an unknown name or a
payload that does not decode (the stored error names the problem and its line and column, never a value of the
payload). A payload larger than `WATCHFIRE_MAX_PAYLOAD` (1 MiB) is refused by `dispatch`; a stored one that large
goes to the dead letters without being read or run. Moving a job to the dead letters and `retry` moving it back are one database
transaction each, so the job is in exactly one of the two tables. A job reserved for an attempt beyond `max_attempts`
(its last attempt ended without its outcome being stored) goes to the dead letters without running again. A job still
running at shutdown goes back to the queue. Each job is recorded as a run of its worker, with the job name. Error
texts stored with runs, agents and dead letters keep their first 8 KiB (marked as truncated). The `jobs` and
`dead_letters` tables hold each job's payload as plain JSON, and the API's `GET /jobs` shows the dead letters'
payloads; dead letters stay until they are retried or deleted (the `memory` driver keeps the latest 1000).

With `QUEUE_DRIVER=redis` (feature `redis` of `smeltery`) the jobs and dead letters live on the Redis server of
`REDIS_URL` (`rediss://` uses rustls), under keys that start with `QUEUE_PREFIX` (default
`<app name in snake case>_queue_`) and `{default}:`: a hash per job and per dead letter, and sorted sets of the
waiting, reserved and dead ones. `{default}` is a hash tag, so all of the queue's keys share one hash slot. The driver
connects to one Redis server (a standalone server, or the primary of a replicated one), not to Redis Cluster. Every
queue operation is one Lua script (sent by its SHA-1 with `EVALSHA`), which Redis runs atomically: each job is reserved
by one worker across all processes on that server and prefix, and a job is waiting, reserved or a dead letter, never
two of them. Order, attempts, backoff, stale reservations, dead letters and the dashboard's retry and delete work as
with the `database` driver. The driver connects on first use and reconnects by itself; every call has the
`WATCHFIRE_STORE_TIMEOUT` budget. A `redis://` URL with a password to another machine logs a warning at start:
the password and the jobs travel unencrypted, so use `rediss://` unless the network is private. `CACHE_PREFIX` is
compared with the start of the queue's keys,
`<QUEUE_PREFIX>{default}:`: neither may start with the other, and `QUEUE_PREFIX` has no `{` or `}`; otherwise the app
refuses to boot, because `cache:clear` on Redis removes every key that starts with `CACHE_PREFIX`.

The queue's keys have no expiry. With the `maxmemory-policy` `noeviction` (or a `volatile-*` policy, which only evicts
keys with an expiry) Redis keeps every queued job; when its memory is full it refuses new jobs, and `dispatch` returns
that error. Redis's persistence (RDB or AOF) decides which jobs survive a restart of Redis: writes it had not saved yet
are lost, so a job can be lost or run again. New job and dead-letter ids never reuse the id of a job or dead letter
that still exists, also when the id counter itself was among the lost writes.

**The scheduler.** `w.schedule()` takes a target, `.job(J)` (dispatches it), `.call(name, closure)` or
`.agent(name)` (starts it when it is stopped, completed, failed or backing off), and a timing: `.every(d)`, `.every_minute()`,
`.hourly()`, `.daily()`, `.daily_at("03:00")`, `.weekly()` or `.cron("m h dom mon dow")` (five fields with `*`, lists,
ranges and steps, in UTC). `.overlap(Overlap::Skip)` (default) skips a call that is still running,
`Overlap::Queue` runs it once the previous one finishes, `Overlap::Allow` runs both. The clock is an agent named
`scheduler`. `schedule:run` runs the tasks due this minute once, for system cron. `.every(d)` runs first `d` after
launch; with a shared lock store (see "Several processes") it runs on multiples of `d` since the Unix epoch instead,
so that every process agrees on the ticks.

**Persistence.** With a database, Watchfire keeps the agent registry, run history, checkpoints and (with the
`database` queue driver) the job queue and the dead letters in `watchfire_agents`, `watchfire_runs`, `watchfire_checkpoints`, `watchfire_jobs` and
`watchfire_dead_letters`. A migration creates them by delegating to `smeltery::watchfire::migrations::up(schema)` /
`down(schema)`; it also creates `watchfire_commands` (commands for agents in other processes) and gives each run the
name of the process that ran it. An app whose Watchfire migration ran before those existed adds a second migration
that calls `smeltery::watchfire::migrations::up_multi_process(schema)` / `down_multi_process(schema)` (it does nothing
when they exist). Without a database (or before the migration ran) the history and checkpoints live in memory, and
the queue too (`QUEUE_DRIVER=memory`). Every store call has a timeout (`WATCHFIRE_STORE_TIMEOUT`, 5 s); a store failure
is logged and never stops an agent.

| Variable | Default | Meaning |
|---|---|---|
| `WATCHFIRE_MAX_CONCURRENT` | `0` | agents running at once (0: no limit; the queue workers and the scheduler do not count) |
| `WATCHFIRE_WORKERS` | `2` | queue workers |
| `WATCHFIRE_JOB_TIMEOUT` | `60` | seconds a job may run |
| `WATCHFIRE_HTTP_TIMEOUT` | `30` | seconds per `ctx.http()` request |
| `WATCHFIRE_HTTP_MAX_BODY` | `10485760` | the largest response body `ctx.http()` reads, in bytes |
| `WATCHFIRE_STORE_TIMEOUT` | `5` | seconds per store or queue call |
| `WATCHFIRE_MAX_PAYLOAD` | `1048576` | the largest job payload in bytes: `dispatch` refuses a larger one, and a larger stored one is dead-lettered without running |
| `QUEUE_DRIVER` | `database` with a database, else `memory` | where queued jobs wait: `database`, `redis` (feature `redis`; the server of `REDIS_URL`) or `memory` |
| `QUEUE_PREFIX` | `<app name in snake case>_queue_` | the start of the `redis` queue driver's keys |
| `WATCHFIRE_DASHBOARD` | `local` | who may use the dashboard: `local` (signed-in users the dashboard gate admits; under `APP_ENV=local` also requests from this machine without signing in), `auth` (signed-in users the gate admits, also under `APP_ENV=local`), `off` (no dashboard) |
| `WATCHFIRE_API_ADDR` | empty | the address `work` serves the API on (e.g. `127.0.0.1:8001`); the `agents:*` commands call it (an address on this machine, or an `https://` URL), else `SERVER_HOST:SERVER_PORT` |
| `WATCHFIRE_IN_SERVE` | `true` | whether `serve` runs the agents, queue workers and scheduler (`false`: only `work` runs them) |
| `WATCHFIRE_LOCK_STORE` | `CACHE_STORE` | the cache store whose locks coordinate several processes; `off` for none |
| `WATCHFIRE_LEASE_TTL` | `30` | seconds a lease lasts without renewal (at least 3); at least 12/7 of the longest singleton agent's shutdown timeout, and 18 with `Overlap::Skip` / `Queue` scheduled calls (see "Several processes") |
| `WATCHFIRE_ALERT_WEBHOOK` | empty | a URL every alert is POSTed to as JSON |
| `WATCHFIRE_ALERT_MAIL` | empty | comma-separated addresses every alert is mailed to (needs `.mail()`) |

**Several processes.** `serve` runs the web server and the background work together. `serve --no-agents` (or
`WATCHFIRE_IN_SERVE=false`) serves the web only, so a deployment can run web processes next to one or more `work`
processes. When the processes share a cache store whose locks span processes (`database`, `redis`, `memcached`,
and `file` for the processes of one machine; `WATCHFIRE_LOCK_STORE`, by default `CACHE_STORE`), they coordinate
through its atomic locks:

- **Agents run in one process at a time.** Each agent holds a lease (a lock named `watchfire:agent:<name>`). The
  other processes show it as `standby` and try to take the lease every `WATCHFIRE_LEASE_TTL / 3`. A process that
  stops releases its leases, so another one takes over within that interval; a process that dies leaves them to
  expire, so another one takes over between `WATCHFIRE_LEASE_TTL` and 4/3 of it after its last renewal. The holder
  renews a lease every third of its cut-off, `TTL - shutdown timeout - TTL / 6` after the last renewal it sent. At the
  cut-off, whatever store call is still in flight (a renewal, or a status write of the running agent), or as soon as
  a renewal finds the lease taken, it stops the agent (within the agent's shutdown timeout) and goes to `standby`; a
  run whose lease went while its start was being recorded does not start. So it has stopped by `TTL - TTL / 6` after
  the store's last renewal: processes whose clocks differ by less than `WATCHFIRE_LEASE_TTL / 6` (5 s by default)
  never run an agent twice at once. A singleton whose shutdown timeout leaves a cut-off under a quarter of the TTL is
  refused at launch with the TTL it needs (at least 12/7 of the shutdown timeout: the default 10 s timeout needs at
  least 18); so is a coordinated scheduled call with `Overlap::Skip` / `Queue` (stopped within 10 s) under 18, by
  `serve`, `work` and `schedule:run` alike. The
  holder keeps the agent in every state: paused, stopped or completed there means the same everywhere while it runs;
  the next holder starts it as `autostart` says. `remove` frees the lease, so another process takes the agent over:
  remove it in every process, or from the registration. Pool members are agents of their own (`fetcher#0` may run in
  one process and `fetcher#1` in another). `.per_process()` (on the builder or `AgentConfig`) runs an agent in every
  process instead.
- **Each scheduled tick runs once.** A job or call that is due claims its tick in the store (`Cache::add` of
  `watchfire:schedule:<name>:<tick>`); the first process runs it and the others skip it. A call with `Overlap::Skip`
  or `Overlap::Queue` also holds a lease while it runs, so it never runs in two processes at once: a run in another
  process makes it skip (`Queue` waits only for a run in its own process), and when its lease is lost the call is
  cancelled and dropped after 10 s. `schedule:run` claims the minute's tick and takes the same leases, so system cron
  on several servers, or next to `serve`, runs a task once; it skips `every(d)` tasks whose `d` is not a whole number
  of minutes (only `serve` / `work` run those then). `.agent(name)` targets start the agent in the process that holds
  it. `.per_process()` on a scheduled task runs it in every process.
- **Queue workers run in every process.** The `database` queue reserves each job for one worker (above).
- **Live pages update from every process.** `serve --no-agents` and `work` use the shared
  [PubSub](#pubsub-messages-between-processes) driver under `PUBSUB_DRIVER=auto`, so an agent's
  `broadcast.to("counter").refresh()` in `work` reaches the pages held by the web process.
- **Run history keeps processes apart.** Each run records its process (`RunRecord::process`: host, process id and a
  random part). Every process holds a lease on its own name; runs left `running` by a process whose lease is gone are
  marked `interrupted` (checked at launch and every `WATCHFIRE_LEASE_TTL`). A process that loses its own lease (its
  store calls fail) takes it again as soon as the store answers and puts its running runs that were marked
  `interrupted` back to `running`; a run that ended meanwhile keeps its outcome. A final run record that could not be
  written (the database did not answer) is written again at the next check.
  Processes that share the database without coordinating (lock store `off`, or the local fallback below) hold no
  such lease, so a coordinating process marks their running runs `interrupted`.

At launch Watchfire checks the store by taking and freeing a lock. When it does not answer (or the `cache_locks` table
is missing), a process under `APP_ENV=local` or `testing` runs everything itself (with a warning), and any other
coordinates anyway: its agents wait in `standby` and its ticks are skipped until the store answers. A store named in
`WATCHFIRE_LOCK_STORE` that cannot be opened is an error; one that does not answer the probe is an error under
`APP_ENV=local` / `testing` and coordinated through (as above) elsewhere. The `memory`, `array` and `null` stores do not span
processes: each process then runs every agent and every tick itself.

**One dashboard for every process.** With the Watchfire tables in the database, a process that does not run
Watchfire (`serve --no-agents`) shows the registered agents from `watchfire_agents` and `watchfire_runs`, with the
process that holds each one (or "no process holds it", with its last recorded state); a process where an agent is in
`standby` shows the holder's row for it. A command for an agent that runs elsewhere goes into `watchfire_commands`.
Every coordinating process looks there once a second for the agents it holds, carries the command out (each agent's
commands in order, those of different agents side by side) and records the outcome (or the refusal, with its
status); the dashboard (or API) waits up to 5 s for it and shows it. When the
holder took the command but has not finished within 5 s (a slow stop) it says so; when no process took it, the
command is carried out when a process holding the agent takes it, or lapses after 60 s. A command taken but never
finished is closed after 11 minutes as "outcome never recorded"; a process that shuts down while carrying a command out
closes it at once (503, outcome unknown). Command times come from the database's clock. At
most 20 commands wait per agent.
Log lines stay in the memory of the process that runs an agent; from another process, `GET /agents/{name}/logs` (and
`agents:logs`) asks that process for them through `watchfire_commands` (the newest lines that fit in 60 KB; 504 when it
does not answer within 5 s). The answer's row is deleted once read, or a minute after it was written when nobody read it. The `/events` stream stays with the process that runs an agent.

**JSON API.** Under `/_watchfire/api` (no sessions, no CSRF check):

| Request | Answer |
|---|---|
| `GET /agents` | every agent's status |
| `GET /agents/{name}` | `{"status": …, "runs": […]}` (the latest 20 runs) |
| `POST /agents/{name}/{start\|stop\|pause\|resume\|restart}` | the new status |
| `GET /agents/{name}/logs` | the last log lines (at most 200), also from the process that runs the agent |
| `GET /runs?agent=&limit=` | the latest runs (default 50, at most 500) |
| `GET /jobs` | `{"driver", "pending", "reserved", "dead", "dead_letters": […]}` (the latest 50) |
| `POST /jobs/dead/{id}/retry` | queues a dead letter again: `{"job_id": …}` |
| `DELETE /jobs/dead/{id}` | deletes a dead letter |
| `GET /schedule` | the scheduled tasks with their next run |
| `GET /events` | Server-Sent Events: `snapshot` (every agent) first, then `status` per change and `event` per `emit`; the stream ends when the app shuts down |

Errors are `{"error": "…"}` with 401 (no or a wrong token), 404 (unknown agent, action or dead letter), 409 (the
agent is in the wrong state, e.g. stopping a stopped agent; or the process holding it refused the command), 502 (the
log lines the holding process sent could not be read), 503 (Watchfire is not running in this process and no other process
can be reached, it is shutting down, or the commands table failed) or 500. A command for an agent in another process
answers with the holder's own status for a refusal (404, 409, 503), 202 `{"message"}` when the holder took it but
has not finished within 5 s, 504 when no process took it within 5 s, and 429 when 20 commands wait for the agent
already. Every request with `Authorization: Bearer <token>` is accepted,
where the token is the hex HMAC-SHA256 of `"watchfire-api"` keyed with `APP_KEY`
(`smeltery::watchfire::web::api_token(key)`). Without the token only local development gets in:
`WATCHFIRE_DASHBOARD=local`, `APP_ENV=local`, an `APP_URL` on this machine (`localhost`, `*.localhost` or a loopback
address), and a request from a loopback address that did not come through a proxy in `TRUSTED_PROXIES` and carries
no reverse-proxy or CDN header (`Forwarded`, `X-Forwarded-For`, `-Host`, `-Proto`, `-Port`, `-Server`, `X-Real-IP`,
`X-Original-Forwarded-For`, `X-Client-IP`, `Via`, `CF-Connecting-IP`, `True-Client-IP`, `Fastly-Client-IP`), that
names this machine as its `Host` (`localhost`, `*.localhost`, a loopback address or the `APP_URL` host, so a page on
another domain that resolves to `127.0.0.1` gets nothing) and that no other site sent (an `Origin` other than the
app's own, host and port, so another dev server on `localhost:3000` is another site; more than one `Origin`; or a
`Sec-Fetch-Site` other than `same-origin` or `none`, is refused; a link followed to the dashboard page opens it). Pool members are addressed with `#`
encoded (`fetcher%230`). `serve` answers the API on the app's own port; `work` answers it only when
`WATCHFIRE_API_ADDR` is set, with HTTP/1.1, at most 64 connections at once, 10 s for a request's headers (also the
idle limit between requests) and `X-Content-Type-Options: nosniff`; bind it to `127.0.0.1`.

**Dashboard.** `GET /_watchfire` is a page (a Mold template compiled into Watchfire, the same in debug and release
builds) with summary tiles (agents running, paused, failed or backing off, in standby; queue depth, dead letters and
the next scheduled run), every agent's state, heartbeat (`on time` or `stalled`: whether the last heartbeat arrived
within the agent's timeout), last heartbeat, restarts, runs (job runs of a queue worker included), the process that
runs it, pending backoff and next restart, last error and recent runs with their counters (run ids count per
process, so runs of an earlier process are marked as such), the queue counts and dead letters, and the schedule.
Each agent has the buttons its state allows (start; pause or resume; restart and stop): forms with `@csrf` posting
to `/_watchfire/agents/{name}/{action}`, which redirect back with a flash message. Stop and restart ask first: they
link to `/_watchfire?agent=<name>&confirm=<action>`, the page with that confirmation open (its form and a Cancel
link, which has the focus), rendered without the live panels and without the 5 s reload. The page is sent with
`Cache-Control: no-store`, `X-Frame-Options: DENY` and `Content-Security-Policy: frame-ancestors 'none'`, so no other
site can frame it. The page brings its own stylesheet, embedded in Watchfire and served at
`/_watchfire/assets/watchfire.css?v=<version>` (to anyone, as it holds no data; 404 under `off`; cached for a year,
and the version changes with the file), so it needs nothing from the app: no `app.css`, no Tailwind, no web fonts,
no JavaScript besides Sparks. It uses the forge colours of new apps, follows the system's light or dark mode, and
shows each agent as a card on narrow screens.
The dashboard is for signed-in users that the app's gate admits, in `app/agents/mod.rs`:

```rust,no_run
use smeltery::watchfire::prelude::*;

pub fn register(w: &mut Watchfire) {
    // `auth` is the visitor's sign-in (`auth.id()`, `auth.user::<User>().await?`), `app` the app.
    w.dashboard_gate(|auth, _app| async move { Ok(auth.id() == Some(1)) });
}
# fn main() {}
```

Without a gate no signed-in user gets in. Guests are sent to the `login` route (JSON clients get 401), a signed-in
user the gate refuses gets 403, and an error from the gate is a 500. With `WATCHFIRE_DASHBOARD=local` (default),
`APP_ENV=local` and a local `APP_URL`, the requests the API's local rule admits (a loopback address, no
reverse-proxy header, a `Host` on this machine and no other site's `Origin`; see the API) get in without signing in; `auth` asks for the gate under `APP_ENV=local` too; `off` hides the dashboard (404). The page, its forms and its
live panels follow the same rule. The dashboard is a web route, so it is mounted only when the app has a usable
`APP_KEY`.

When the app installs Sparks too (`.agents(…)` and `.sparks(…)`, in either order), the dashboard is live: its
panels are two Spark components registered by Watchfire, `watchfire.agents` (the agents table) and
`watchfire.queue` (queue, dead letters, schedule), both streamed (`#[spark(stream)]`). Watchfire pushes a refresh of
the agents panel after status changes, at most one every 500 ms however busy the supervisor is, and of the queue
panel every 5 seconds, only while a dashboard is open. The buttons call the panel's `act(name, action)` action
(with JavaScript, stop and restart first call `ask(name, action)` instead of following their link, which opens the
confirmation in the panel's state, so a pushed refresh keeps it open; `cancel` closes it) through the Sparks update endpoint, which applies the same rule to actions and refreshes (401 for a guest, 403 for a
signed-in user the gate refuses, 404 when `off`); its update requests show no request headers, so local development
there also needs the session of a browser the page let in from a loopback address (any dashboard request the local
rule does not admit takes that back). In a process that does not run an agent, the panels check the shared tables
every 2 seconds while a dashboard is open and refresh when they changed. The page is rendered on the
server first, so it reads without JavaScript; then the forms post as before (stop and restart lead to the
confirmation page) and a `<noscript>` meta tag reloads the page every 5 seconds. Without Sparks the page reloads itself every 5 seconds (`<meta http-equiv="refresh">`).

**Console.** `agents:list`, `agents:start|stop|pause|resume|restart <name>` and `agents:logs <name>` call the running
app's API (at `WATCHFIRE_API_ADDR`, else `SERVER_HOST:SERVER_PORT`) with the token, with a 10 s timeout, following no
redirect; when nothing answers they say so and how to start the app. They refuse to send the token over plain
`http://` to an address that is not this machine (set a loopback `WATCHFIRE_API_ADDR` or an `https://` URL). They
print control characters from the app (terminal escape sequences in log lines) as `�`. `agents:token` prints the
token. The token is derived from `APP_KEY`: a new `APP_KEY` is the way to revoke it (it also signs every user out
and invalidates signed Spark state).

**Alerts.** An agent becoming `failed` (its run failed under `Restart::Never`, or it reached its restart limit), a
stalled run, and a job moved to the dead letters each raise an alert: an error-level log line (target
`smeltery_watchfire::alert`), every `w.on_alert(|alert| async move { … })` hook (one at a time, at most 10 s each,
in a task of their own, so supervision never waits), and a POST of the alert as JSON to `WATCHFIRE_ALERT_WEBHOOK`
(10 s timeout, retried up to 3 times; a failed delivery is logged with the webhook's host only), and a mail to the addresses in `WATCHFIRE_ALERT_MAIL` (subject
`[APP_NAME] Watchfire alert: <kind> <agent>`) when the app has `.mail()`. An `Alert` has `kind` (`failed`, `stalled`, `dead_letter`), `app`, `agent`,
`job`, `message` and `at_ms`.

**LLM helpers** (the `llm` feature: `smeltery = { …, features = ["llm"] }`). `smeltery::watchfire::llm` has the
`Provider` trait (`complete(Request) -> Response` with the system prompt, messages, tool definitions, the answer's
content blocks, stop reason and input / output token usage), `FakeProvider` (scripted replies and tool calls, every
request recorded), `Anthropic` (the Messages API through `ctx.http()`, the key from `ANTHROPIC_API_KEY`, the model and
`max_tokens` set in `AnthropicConfig`; its calls follow no redirect, so the key and the prompt go only to the base URL,
and its `Debug` output hides the key) and `Agent`, a tool-calling loop:

```rust,no_run
# #[cfg(feature = "llm")]
# async fn demo(ctx: smeltery::watchfire::AgentCtx) -> Result<(), smeltery::watchfire::AgentError> {
use smeltery::watchfire::llm::{Agent, Anthropic, AnthropicConfig, Budget, Price};

#[derive(serde::Deserialize)]
struct Lookup { symbol: String }

let claude = Anthropic::from_env(ctx.http().clone(), AnthropicConfig::new("claude-opus-5-5", 16_000))?;
let outcome = Agent::new(claude)
    .system("You answer questions about stock prices.")
    .tool("price", "The latest price of a stock symbol",
        serde_json::json!({"type": "object", "properties": {"symbol": {"type": "string"}}, "required": ["symbol"]}),
        |input: Lookup| async move { Ok(format!("{}: 42.00", input.symbol)) })
    .max_turns(8)
    .budget(Budget::new().max_tokens(200_000).max_cost(1.0).price("claude-opus-5-5", Price::per_mtok(4.0, 20.0)))
    .cancel_on(ctx.token().clone())
    .run("What does ACME trade at?")
    .await?;
ctx.log().info(outcome.text);
# Ok(())
# }
# fn main() {}
```

Tool calls of one answer run in order and their results go back in one message; a tool error, an input that does
not match the tool's type, or an unknown tool goes back to the model as an error result. The loop stops with
`MaxTurns` after `max_turns` model calls (default 10) and with `BudgetExceeded` once the tokens or the cost (from the
prices you give per model) pass the budget. Rate limits (429), overload (529), 5xx answers and network errors are
retried (default 3 times, backoff 1 s..=30 s with jitter, honouring `retry-after`); other errors are not.

**Testing.** `TestApp` leaves Watchfire off (dispatched jobs wait in the queue); `TestApp::new(build).with_agents()`
starts it. `smeltery::watchfire::testing::Harness` runs agents under the real supervisor on paused Tokio time with a
fake HTTP transport (`http::FakeTransport`: canned answers by method and URL, every request recorded):

```rust
use smeltery::watchfire::prelude::*;
use smeltery::watchfire::testing::Harness;

# #[tokio::main(flavor = "current_thread", start_paused = true)]
# async fn main() {
// In a #[tokio::test(start_paused = true)] function:
let mut h = Harness::new(agent_fn("flaky", |ctx: AgentCtx| async move {
    ctx.sleep(1.secs()).await;
    Err(AgentError::msg("boom"))
}))
.config(AgentConfig::default().backoff(1.secs()..=8.secs()));
h.start().await.unwrap();
h.advance(1.secs()).await;
assert_eq!(h.state(), AgentState::BackingOff);
assert!(h.next_backoff().unwrap() <= 1.secs());
h.advance(10.secs()).await;
assert!(h.restarts() >= 1);
assert_eq!(h.runs().await[0].outcome, RunOutcome::Failed);
h.shutdown().await;
# }
```

`Harness` also has `transitions()` (every state in order), `runs()`, `logs()`, `stop()`, `agents()` and
`Harness::from_watchfire(w)` for a whole registration; `JobHarness::new().await.run(&job)` runs a job's `handle`
(`JobHarness::for_app(app).await` runs it inside a given app, e.g. a `TestApp`'s with its database and services)
directly.

## Mail

Mail (`smeltery::mail`) is wired with `.mail()` in `bootstrap/app.rs` (the trait is `smeltery::mail::MailExt`, in
the prelude). A mail class is a Mold template struct that also implements `Mailable`: the template is the HTML body,
the struct's fields are its variables, and `envelope()` says who gets it.
`smeltery make:mail Welcome` creates `app/mail/welcome.rs` and `resources/views/mail/welcome.mold.html` (an HTML mail
with inline styles) and adds the module to `app/mail/mod.rs`; new apps install mail and have the `MAIL_*` keys in
`.env`.

```rust
use smeltery::prelude::*;
# #[derive(serde::Deserialize)]
# pub struct RegisterForm { name: String, email: String }

#[derive(Mold)]
#[mold("mail/welcome")]                 // resources/views/mail/welcome.mold.html
pub struct Welcome {
    pub name: String,
    pub email: String,
}

impl Mailable for Welcome {
    fn envelope(&self) -> Envelope {
        Envelope::new()
            .to(self.email.as_str())        // also "Ada <ada@example.com>" or ("ada@example.com", "Ada")
            .subject(format!("Welcome, {}!", self.name))
    }
}

async fn register(mailer: Mailer, Form(form): Form<RegisterForm>) -> Result<Redirect> {
    mailer.send(Welcome { name: form.name, email: form.email }).await?;
    Ok(Redirect::to("/"))
}
# fn main() {}
```

`Envelope` has `to`, `cc`, `bcc`, `reply_to`, `from` (instead of `MAIL_FROM_ADDRESS` / `MAIL_FROM_NAME`) and
`subject`. Besides `envelope()`, a `Mailable` may implement `text()` (the plain-text part, e.g. from a second
template rendered with `smeltery::mail::render`; by default it is made from the HTML: tags dropped, links as
`text (url)`) and `attachments()` (`Attachment::from_bytes(name, bytes)`, `Attachment::from_path(path)`). Mail
templates render like views (from the files with hot reload in debug builds, compiled in release builds);
`route("name")` gives absolute URLs (`APP_URL` + path) in mail.

`Mailer` is a handler argument, or `Mailer::of(&app)` elsewhere: `send(mail)` renders on a blocking thread and
hands the mail to the transport, `render(mail)` returns the rendered `Email`, and `send_email(email)` sends an
`Email` built by hand (`Email::new(envelope).html(…).text(…).attach(…)`). With Watchfire, `mailer.queue(mail)` /
`queue_later(mail, delay)` (the trait `smeltery::watchfire::mail::QueueMail`, in the prelude) render the mail now
and send it from a queue worker, retried like any job (the job `smeltery-send-mail` is registered when the app has
both `.mail()` and `.agents(…)`).

| Variable | Default | Meaning |
|---|---|---|
| `MAIL_MAILER` | `log` | `smtp`, `log` (the mail is written to the log at `info`, target `smeltery::mail`: sender, recipients, subject and text part; outside local development (`APP_ENV` `local` or `testing` with an `APP_URL` on this machine) only sender, recipients and subject, with a note that the body was withheld because it can carry secrets such as reset links, and a warning at boot) or `fake`; always `fake` under `APP_ENV=testing` |
| `MAIL_HOST` | `127.0.0.1` | the SMTP server |
| `MAIL_PORT` | `465` for `tls`, `587` for `starttls`, `25` for `none` | its port |
| `MAIL_USERNAME`, `MAIL_PASSWORD` | empty | the SMTP login (none when the username is empty); the password is never logged |
| `MAIL_ENCRYPTION` | `starttls` | `tls` (TLS from the start), `starttls` (an upgrade the server must offer), `none` (plain SMTP; with a `MAIL_USERNAME` only for a `MAIL_HOST` on this machine, since the password would cross the network in clear: anywhere else the mailer refuses to start) |
| `MAIL_TLS_CA` | empty | a PEM file (relative to the app root) of extra CA certificates to trust, for an SMTP server with a private CA |
| `MAIL_TIMEOUT` | `10` | seconds per SMTP step; a whole send gives up after four times that |
| `MAIL_FROM_ADDRESS`, `MAIL_FROM_NAME` | `hello@example.com`, `APP_NAME` | the default sender |

SMTP runs on lettre's Tokio transport with rustls (ring) and the platform's certificate verifier; `MAIL_TLS_CA`
adds certificates to the platform's trusted roots. A server certificate that is not trusted fails the send before
any login. A failed send is an error for the caller and a `warn` log line.

**Testing:** under `APP_ENV=testing` mail goes to a fake mailbox: `Mailer::of(app.app())?.mailbox()` (or
`Mailer::fake(app.app())` on an app without `.mail()`) returns it, with `sent()`, `emails()`,
`sent_of::<Welcome>()`, `assert_sent::<Welcome>(|mail, email| mail.name == "Ada")`, `assert_sent_count::<T>(n)`,
`assert_sent_to(address)` and `assert_nothing_sent()`.

## Bellows: AI-agent support

Bellows makes an app ready for coding agents. Each part is chosen with `smeltery new … --bellows` (or the question)
and can be added to an existing app with `smeltery bellows:install`:

| Part | Files | What it gives an agent |
|---|---|---|
| MCP server | `.mcp.json` | the app's own [MCP](https://modelcontextprotocol.io) server, `smeltery bellows:mcp` |
| Skills | `.bellows/skills/*.md` | step-by-step guides: `crud-resource` (web apps), `spark` (Mold apps), `alloy-page` (React and Vue apps), `auth-route` (web apps with authentication), `migration`, `mail`, `agent` (apps with agents) |
| Guidelines | `.bellows/guidelines.md` | the app's conventions |

Every new app also has `CLAUDE.md` and `AGENTS.md` (the same text): the layout, the conventions, the commands and
generators, and which Bellows parts are installed. In React and Vue apps they and the skills describe the kit
(Alloy pages, `useForm`, partial reloads, deferred props); `bellows:install` reads the kit from the app's `Cargo.toml`.

`bellows:mcp` is an app command: `bootstrap/app.rs` calls `.bellows()`, and `smeltery bellows:mcp` runs the app
binary with it. It speaks JSON-RPC 2.0 over stdin / stdout, one message per line (MCP's stdio transport), answers
`initialize` (protocol versions `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`), `server/discover`
(`2026-07-28`), `ping`, `tools/list` and `tools/call`, and writes nothing else to stdout (logs go to stderr).
Requests run one at a time, in order; while one runs, `ping` is answered and `notifications/cancelled` stops it (a
running `cargo test` or generator is killed). A line longer than 1 MiB gets a parse error and is skipped. Tool results
hold the app's data, log lines, test and compiler output and documentation files; the server's instructions tell the
agent to treat them as data, not as instructions.

| Tool | Answers |
|---|---|
| `route_list` | every route: methods, path, name, middleware |
| `models` | the models in `app/models/` with their table's columns, and tables without a model |
| `db_schema` | tables with columns (type, nullable, default, primary key) and indexes; `table` for one table (SQLite, PostgreSQL, MySQL) |
| `config_keys` | the names of the keys in `.env`, `.env.example` and the framework's settings; values are never returned |
| `last_errors` | the latest `ERROR` lines of the app's log file, `LOG_FILE` and its `.1` backup, else `storage/logs/*.log` (`limit`, default 20; `warnings: true` adds `WARN` lines); at most 20 files, lines cut at 2,000 characters, 12,000 characters in all |
| `docs_search` | the sections of this guide (embedded), the app's `CLAUDE.md` and `.bellows/` files that match the words |
| `run_generator` | one `smeltery make:*` command (only the generators; `SMELTERY_BIN` picks the binary), stopped after 60 s; the arguments are names, fields (`title:string`, `body:text?`) and the generators' options, nothing else |
| `run_tests` | `cargo test [filter]`, stopped after `BELLOWS_TEST_TIMEOUT` seconds (600); the result lines and failures. The filter is a test path (letters, digits, `_`, `:`, at most 200 characters). The tool compiles and runs the app's code, its build scripts and its tests |
| `agents_list`, `agent_control` | the running app's Watchfire agents, and start / stop / pause / resume / restart, through its API with the token derived from `APP_KEY` (like `agents:*`) |

`smeltery bellows:install [--mcp] [--skills] [--guidelines] [--all]` writes the missing files and keeps existing
ones; when `.mcp.json` exists with other servers, it adds the `smeltery` entry to its `mcpServers` object.

## Errors

Return `smeltery::Error` from a handler: `Error::not_found()`, `forbidden()`, `unauthorized()`, `bad_request(msg)`,
`http(status, msg)`, `internal(msg)`, `validation(field, msg)` (see [Validation](#validation)), or any error
through `?` (`std::io::Error`, `serde_json::Error`, `Error::other(e)`). The client gets an HTML error page, or `{"error": "…"}` when it sent `Accept: application/json`.
Internal errors are logged; their details appear in the page only with `APP_DEBUG=true`. A panicking handler answers
500.

## The server

`smeltery serve` (or the app binary with `serve`) binds `SERVER_HOST:SERVER_PORT`. Every response carries an
`x-request-id`. Requests are traced with `tracing`. On Ctrl-C or SIGTERM the server stops accepting connections and
drains in-flight requests for up to `SHUTDOWN_TIMEOUT` seconds. Watchfire's agents stop within the same budget, and
so do password reset mails still being sent.

**Connections:** a client has `SERVER_HEADER_TIMEOUT` seconds (30) to send a request's headers, and a connection
with no request running for that long is closed: before its first request, between two HTTP/1 requests, and on an
HTTP/2 connection (cleartext HTTP/2 with prior knowledge is served too), which gets a `GOAWAY`. A request counts as
running until its response body is sent, so a long download or an event stream keeps its connection. At most
`SERVER_MAX_CONNECTIONS` (4096) connections are open at once; further clients wait to be accepted (each connection
is a file descriptor, so the process's open-file limit stays above it). One client address holds at most
`SERVER_MAX_CONNECTIONS_PER_IP` (128) of them (an IPv6 client by its /64); a further connection from it is closed at
once. The same address runs at most `SERVER_MAX_CONNECTIONS_PER_IP` requests at once across its connections (HTTP/2
carries many requests on one connection); a further request gets 429 with `Retry-After: 1`. One HTTP/2 connection
runs at most `SERVER_MAX_STREAMS` (32) requests at once; further ones wait for a free stream. Peers listed in
`TRUSTED_PROXIES` are not counted per address, since a proxy carries many clients: list the
proxy there when the app runs behind one. `REQUEST_TIMEOUT` counts from when the server
takes the request, reading a form body for its `_method` field included, so a body that trickles in gets a 408 like
a slow handler. `serve` and `work` refuse to start under `APP_ENV=testing`.

**Upgraded connections (WebSockets):** HTTP/2 WebSockets (extended `CONNECT`) are not offered, so clients and proxies
upgrade over HTTP/1.1. A route that upgrades a connection itself checks the handshake's `Origin`: browsers open
WebSockets to any site and send the cookies their `SameSite` rules allow (a page on a sibling subdomain of the same
site included), and a WebSocket handshake is a `GET`, which the CSRF check does not cover. Compare it with the
origin of `APP_URL` (scheme, host and port) and answer 403 to any other origin before the upgrade.
[Anvil](#broadcasting) does this with `ANVIL_ALLOWED_ORIGINS`.

**Request log:** at `LOG_LEVEL=debug` every request gets a span with its method, URI, HTTP version and client IP.
The URI never shows secrets: a path segment of a route parameter whose name contains `token`, `secret`, `signature`,
`password`, `hash` or `key` (such as `/reset-password/{token}`), every query value and every query item without a
value appear as `[redacted]`, so `/reset-password/abc?email=a@b.test` is logged as
`/reset-password/[redacted]?email=[redacted]`. Secret segments are found in mangled links too (repeated or trailing
slashes, other letter case, percent-encoded characters).

**Compression:** responses are compressed with brotli or gzip when the client sends `Accept-Encoding`, except
server-sent event streams (`text/event-stream`, such as the Sparks and Watchfire streams), images other than SVG,
formats that are compressed already (`font/woff`, `font/woff2`, zip, gzip, 7z, rar and zstd archives, PDF, `video/*`,
`audio/*`), responses under 32 bytes, range responses and responses that already have a `Content-Encoding`. A
compressed response's `ETag` is weak (`W/"…"`), so it is never mistaken for the uncompressed bytes; `If-None-Match`
still answers 304. The CSRF token in pages is masked per response (see [CSRF](#csrf)), so compressed HTTPS pages do
not leak it through their size (BREACH).

**Security headers:** every response (pages, API routes, files from `public/`, error pages, `/up`) carries
`X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`, `X-Frame-Options: SAMEORIGIN`
and `Content-Security-Policy: frame-ancestors 'self'`, so other sites cannot show the app's pages in a frame.
`FRAME_OPTIONS=DENY` sends `DENY` and `frame-ancestors 'none'` (no frames at all), `FRAME_OPTIONS=off` neither
header; `SECURITY_HEADERS=false` sends none of the four. A header the response already has, set by its handler or a
middleware, is never replaced: the Watchfire dashboard keeps its `DENY`. A response with its own `X-Frame-Options`
gets no `Content-Security-Policy` from the framework, and a response's own `Content-Security-Policy` is kept as it is,
so a page that a partner site may frame sends `Content-Security-Policy: frame-ancestors https://partner.example`
(browsers follow `frame-ancestors` over `X-Frame-Options`).

`HSTS_MAX_AGE` (seconds, `0` by default) adds `Strict-Transport-Security: max-age=…` to every response when `APP_URL`
starts with `https://`, whatever `SECURITY_HEADERS` says; with an `http://` `APP_URL` it is not sent and the app logs
a warning at boot. A browser that has seen the header refuses plain HTTP for the domain until `max-age` runs out, so
it is set once HTTPS works for the domain, and lowering it later only reaches browsers that visit again.

**CORS (hybrid and cross-origin clients):** a page or app on another origin (a Capacitor app is
`capacitor://localhost`, a Tauri app `tauri://localhost` or `http://tauri.localhost`, an Electron app the scheme it
registers, a front end on another host) may call the app's API when its origin is listed in `CORS_ALLOWED_ORIGINS`:

```text
CORS_ALLOWED_ORIGINS=capacitor://localhost,tauri://localhost,http://tauri.localhost
CORS_PATHS=/api/
```

- Origins are compared exactly (scheme, host and port). `*`, wildcards and entries with a path stop the app at boot.
- A preflight (`OPTIONS` with `Access-Control-Request-Method`) from a listed origin to a path under `CORS_PATHS` is
  answered `204` before routing, with `Access-Control-Allow-Origin` (that origin), the requested method,
  `Access-Control-Allow-Headers: Accept, Authorization, Content-Type, X-Requested-With, X-Socket-ID, X-CSRF-TOKEN` and
  `Access-Control-Max-Age: 600`. Other requests from it get `Access-Control-Allow-Origin`, the framework's own
  answers included (the `408` of `REQUEST_TIMEOUT`, the `500` of a panic, error pages).
- Every answer under `CORS_PATHS` carries `Vary: Origin`, whatever the origin, so a shared cache keeps the answers
  for different origins apart.
- Credentials are never allowed (no `Access-Control-Allow-Credentials`): a page on another origin cannot read an
  answer to a request made with cookies, and the browser refuses to send a preflighted one, so these clients call
  the API with a bearer token (see [API tokens](#api-tokens)).
- Requests from origins that are not listed, and paths outside `CORS_PATHS`, get no `Access-Control-Allow-Origin`;
  the browser blocks their answers.
- `CORS_PATHS` entries are prefixes compared as text: end each with `/` (`/api` would also cover `/apiary`; a prefix
  without the `/` is warned about at boot).
- A front end on another origin cannot use the session through CORS (no credentials): serve it from the app's own
  origin (or behind a proxy that puts both under one origin), or have it call the API with bearer tokens.
- `null` (sent by sandboxed frames, pages opened from files, and some app shells) is allowed only when it is listed
  literally; then every page that sends `null` is allowed, from any site.
- The listed origins may also open [Anvil](#broadcasting) sockets.

**Behind a reverse proxy:** the client of a request is `smeltery::http::ClientInfo`, a handler argument:

```rust,no_run
use smeltery::http::ClientInfo;

async fn whoami(client: ClientInfo) -> String {
    let ip = client.ip().map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    format!("{ip} over {} to {}", client.scheme(), client.host().unwrap_or("?"))
}
# fn main() {}
```

By default it is the TCP peer, and `X-Forwarded-For`, `X-Forwarded-Proto` and `X-Forwarded-Host` are ignored. List
the proxy in `TRUSTED_PROXIES` (`TRUSTED_PROXIES=127.0.0.1` for nginx or Caddy on the same host, CIDR ranges such as
`10.0.0.0/8` for a load balancer network). For a request whose TCP peer is listed:

- `ip()` is the rightmost `X-Forwarded-For` address that is not itself a listed proxy (the addresses left of it were
  written by the client and are never used). When every address in the header is a listed proxy, `ip()` is the
  rightmost one (the address the nearest proxy saw). When that walk meets a hop that is not an IP address, `ip()` is
  `None` (`auth.ip()` is `unknown`), never the proxy's own address; without the header it is the proxy's address;
- `scheme()` / `is_secure()` follow the first `X-Forwarded-Proto` value (`http` or `https`);
- `host()` is the first `X-Forwarded-Host` value, else the `Host` header.

`host()` and `scheme()` are only as trustworthy as the proxy's configuration: the proxy must set (overwrite)
`X-Forwarded-Host` and `X-Forwarded-Proto` itself, otherwise the client's own values pass through. Caddy sets both;
for nginx:

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
proxy_set_header X-Forwarded-Proto $scheme;
proxy_set_header X-Forwarded-Host $host;
```

Smeltery builds its own absolute URLs (password reset links, mail) from `APP_URL`, never from `host()`. `Back` (the
previous page) accepts a `Referer` on the client's host or on `APP_URL`'s host.

`TRUSTED_PROXIES=*` trusts whatever peer connects, but not the addresses inside `X-Forwarded-For`: the client is the
address the proxy appended. Use `*` only when nothing but the proxy can reach the app (it listens on `127.0.0.1`, a
private network or a firewalled port); a client that reaches the app directly can then send any forwarded header. An
entry that is not an IP address, a CIDR range or `*`, and a range of every address (`0.0.0.0/0`, `::/0`, which would
let any client name its own address), stop the app at boot with the entry named. The login throttle,
`auth.ip()` and the request log use `ClientInfo`'s IP. The `Secure` flag of cookies follows `APP_URL` only.

## The `smeltery` command

| Command | Does |
|---|---|
| `smeltery new <name>` | create an app (questions or flags, see above) |
| `smeltery serve` | run the app (`work` instead of `serve` in a headless app, which has no `routes/`), restart it when Rust files change, run Tailwind in watch mode when it is installed (`TAILWIND_BIN`, the binary of `smeltery tailwind:install`, or `tailwindcss` on `PATH`); in a React or Vue app (one with `vite.config.ts`) run the Vite dev server instead (`node node_modules/vite/bin/vite.js`, `SMELTERY_NODE` overrides `node`; without `node_modules/` a warning) and delete `storage/framework/vite.hot` when it stops |
| `smeltery work` | run the app's Watchfire agents, queue workers and scheduler without the HTTP server, until Ctrl-C / SIGTERM (then they stop within `SHUTDOWN_TIMEOUT`) |
| `smeltery agents:list` | print the running app's agents (through its Watchfire API, see below) |
| `smeltery agents:start` / `stop` / `pause` / `resume` / `restart <name>` | control an agent of the running app through its API |
| `smeltery agents:logs <name>` | print an agent's last log lines from the running app |
| `smeltery agents:token` | print the app's Watchfire API token |
| `smeltery agents:runs <name>` | print an agent's latest runs from the database: run id, start, duration, outcome, job, error, counters (`--limit N`) |
| `smeltery schedule:list` | print the scheduled tasks: name, kind, expression, next run (UTC) |
| `smeltery schedule:run` | run the scheduled tasks due this minute once and exit |
| `smeltery route:list` | print every route: method, path, name, middleware |
| `smeltery migrate` | run the pending migrations (`--force` in production) |
| `smeltery migrate:rollback` | revert the last batch of migrations; `--step N` reverts the last N |
| `smeltery migrate:fresh` | drop every table and run every migration; `--seed` then seeds |
| `smeltery migrate:status` | list the migrations with `Ran` / `Pending` and their batch |
| `smeltery db:seed` | run the seeders; `--class Name` runs one (`--force` in production) |
| `smeltery cache:clear [store]` | remove every entry of the default cache store, or of the named one (`cache:clear redis`), except Watchfire's leases and schedule claims (`--all`: those too) |
| `smeltery key:generate` | write a new `APP_KEY` into `.env` (`--show` prints one instead and reads no `.env`); with `APP_ENV=production` (or no `APP_ENV`) an existing key is kept unless `--force` is given, since a new key signs every user out |
| `smeltery storage:link` | link `public/storage` to `storage/app/public`; when that link is already there it says so and exits with 0, and anything else at `public/storage` is an error |
| `smeltery test` | run the app's tests |
| `smeltery build` | build `public/assets/css/app.css` from `resources/css/app.css` with `tailwindcss --minify` when Tailwind is installed (found as for `serve`; otherwise it prints a warning and builds on, and a failing Tailwind fails the build), then the release binary; in a React or Vue app instead of Tailwind: `npm ci` (or `npm install` without a `package-lock.json`) when `node_modules/` is missing, then `npm run build` into `public/build/` (`SMELTERY_NPM` overrides `npm`; without npm, or when a step fails, the build fails before cargo runs) |
| `smeltery tailwind:install` | download the pinned Tailwind CSS standalone binary into the per-user folder (see [Pages and CSS](#pages-and-css)); a binary already there with the right checksum is kept |
| `smeltery make:model <Name> [field:type ...]` | create a model in `app/models/`; fields are `name:type` with the types `string`, `text`, `integer`, `bigint`, `bool`, `float`, `date`, `datetime`, `json`, `uuid`, `<name>_id:foreign`, `file` (an upload: the forms post `multipart/form-data` with a file input, the controller stores the file in `storage/app/public/<table>/` and the column holds its path under `storage/app/public`; its form rule `mimes` accepts `jpg`, `jpeg`, `png`, `gif` and `webp` when a part of the field's name says image (`image`, `img`, `photo`, `picture`, `pic`, `avatar`, `logo`, `thumbnail`, `thumb`, `icon`, `cover`, `banner`, `poster`: `image`, `cover_image`), else those and `pdf`, `txt`, `csv`, `docx`, `xlsx`; add types to the rule in the controller as needed, never `html`, `svg`, `xml` or `js`, which a browser runs as a page of the app), and a trailing `?` for nullable (`body:text?`). Flags: `-m` migration, `-c` controller, `-r` resource controller with views (in a React or Vue app: pages, see [The React and Vue starter kits](#the-react-and-vue-starter-kits)) and routes, `-f` factory, `-s` seeder, `--all` (`-mrfs`), `--searchable` (in an app with the `search` building block: `impl Searchable`, the search index migration, the registration in `app/providers/search.rs`, and with `-r` a list page that searches, see [Create an app](#create-an-app)) |
| `smeltery make:controller <Name>` | create a controller with an index view and a `GET` route; `--resource [--model Post]` creates the seven resource actions (validated with `Valid<…Form>`, flash messages), their views (forms with `@csrf`, `@error` and `old()`) and the `r.resource(...)` route entries: in an app with authentication `index` and `show` are public and `create`, `store`, `edit`, `update` and `destroy` need a signed-in user (`.middleware("auth")`); without authentication all seven are public, as the comment above them, the controller's module doc and the generator's note say; in a React or Vue app the controller returns Alloy pages and the generator writes them under `resources/js/pages/`. In an app without `routes/web.rs` (headless) it refuses and writes nothing, and `make:model -c`/`-r` makes the other parts and skips the controller |
| `smeltery make:migration <name>` | create a migration: `create_posts_table` creates a table (`down` drops it), `add_slug_to_posts_table` adds a nullable `slug` column (`down` drops it with `ALTER TABLE … DROP COLUMN`), any other name gives empty `up` / `down` |
| `smeltery make:seeder <Name>` | create a seeder in `database/seeders/` and register it |
| `smeltery make:factory <Name> [--model Post]` | create a factory in `database/factories/` filling every field of the model with fake values |
| `smeltery make:command <Name>` | create a console command in `app/commands/` and register it (`SendReport` runs as `smeltery send-report`) |
| `smeltery make:mail <Name>` | create a mail class in `app/mail/` and its HTML template `resources/views/mail/<name>.mold.html`, and print how to send it |
| `smeltery make:middleware <Name>` | create a middleware function in `app/middleware/` and print how to register it |
| `smeltery bellows:install [--mcp] [--skills] [--guidelines] [--all]` | add Bellows files to the app (`.mcp.json`, `.bellows/skills/`, `.bellows/guidelines.md`); existing files are kept |
| `smeltery pubsub:install` | add the migration of the `pubsub_messages` table (the `database` driver of [PubSub](#pubsub-messages-between-processes)) to `database/migrations/` and register it; refuses when the app has it already |
| `smeltery prospect:install` | add [Search](#search) to a web app: `app/providers/search.rs` (where `make:model --searchable` registers models) and its `mod` line; it prints the `bootstrap/app.rs` lines to add (`use smeltery::prospect::ProspectExt as _;` and `.prospect(app::providers::search::register)`) and `PROSPECT_DRIVER=database`; it refuses when `bootstrap/app.rs` already calls `.prospect(` and in a headless app |
| `smeltery hallmark:install` | add [API tokens](#api-tokens) to an app with authentication: the `personal_access_tokens` migration, `app/controllers/api/{tokens,user}.rs`, the three token routes in `routes/api.rs`, `tests/api_tokens.rs` and, with Watchfire, the daily `hallmark-prune` schedule, as `smeltery new` writes them with the `hallmark` block; it prints the two `bootstrap/app.rs` lines to add (`use smeltery::hallmark::{Hallmark, HallmarkExt as _};` and `.hallmark(Hallmark::new())`) and the optional `.env` lines; it refuses in an app without authentication and when the migration or a file exists |
| `smeltery bellows:mcp` | run the app's MCP server on stdin / stdout (for coding agents; see Bellows) |
| `smeltery make:spark <Name>` | create a Spark in `app/sparks/` with its view `resources/views/sparks/<name>.mold.html` and register it in `app/sparks/mod.rs`; refuses in React and Vue apps. `--listen <channel> --event <Event>` (an app with Anvil) makes it a listener: a `#[spark(stream)]` component whose method `#[on("anvil:<channel>", "App\\Events\\<Event>")]` counts the events (`--event order.shipped` takes a name as sent), a field per `{field}` of the channel (`i64` for `id` / `*_id`, else `String`) set from the page's props in `mount`, and an event struct to fill; it refuses without `.anvil(` in `bootstrap/app.rs` and for a channel name Anvil does not accept |
| `smeltery make:page <Name>` | in a React or Vue app: create a page (`resources/js/pages/about.tsx`, Vue: `About.vue`), its controller `app/controllers/about.rs` and the route `GET /about`; refuses in Mold and headless apps |
| `smeltery make:agent <Name>` | create a Watchfire agent in `app/agents/` (restart on failure with backoff, heartbeat timeout, a 30-second tick loop) and register it with `w.agent(...)`; in an app without Watchfire (no `app/agents/mod.rs`) it refuses and writes nothing |
| `smeltery make:job <Name>` | create a queued job in `app/jobs/` and register it with `w.job::<...>()` in `app/agents/mod.rs`; in an app without Watchfire it refuses and writes nothing |

The `make:*` commands create new files only: they refuse when a target file exists, and they add `pub mod` and
registration lines above the `// smeltery:…` marker comments, laid out as `cargo fmt` lays them out. When a marker is
missing they print the line to add by hand. The Rust files they create go through `rustfmt --edition 2024` (`RUSTFMT`
picks another binary; the app's `rustfmt.toml` applies); without rustfmt the files stay as written and a note says
to run `cargo fmt`.

`smeltery new` on a terminal shows a SMELTERY banner in block letters, one section per question, a
`✓` line per created folder, a spinner while `migrate` and `db:seed` build and run the app, and a summary box with
the choices, the one-line command and the next steps. Errors and warnings carry coloured `ERROR` / `WARN` labels.
Output is plain text, without colours, banner or spinner, when stdout is not a terminal, when `NO_COLOR` is set, or
with `--no-color` (accepted by every command).

Commands that need the app's code (`route:list`, `migrate…`, `db:seed`, `work`, `agents:…`, `schedule:…`, the
app's own commands, `serve` of the app binary itself) run inside the app: the `smeltery` command forwards them to `cargo run -- <command>`, and the release
binary accepts the same commands. The release binary also has `key:generate` (with `--show` and `--force`) and
`storage:link`, the same code as the `smeltery` commands, so a server needs no `smeltery` CLI: `./my-app key:generate`,
`./my-app storage:link`, `./my-app migrate --force`.

## Testing

```rust
use smeltery::testing::TestApp;

async fn home() -> &'static str { "Hello" }

// In tests/http.rs, inside a #[test] function:
let app = TestApp::new(|app| app.routes(|r| { r.get("/", home); }));
let res = app.get("/");
assert_eq!(res.status(), 200);
assert_eq!(res.text(), "Hello");
```

`TestApp::new(build)` builds the whole app (all middleware, error pages and static files) with `APP_ENV=testing`;
`get`, `get_json`, `post_form`, `post_json`, `post_multipart` and `request` send requests without a server
(`post_multipart(url, &[("title", "Sea")], &[TestFile::new("image", "sea.png", bytes)])`; `TestFile::with_mime`
sets the content type, and `testing::multipart_body` builds such a body for `request`). A generated app's
`tests/http.rs` uses it with the app's own `build`. Like a browser, a `TestApp` keeps the cookies responses set and
sends them with later requests, so sessions and flash messages work across requests; `cookie(name)`,
`set_cookie(name, value)` and `clear_cookies()` reach the jar. `app.acting_as(user_id)` signs a user in for the
following requests, and `.with_csrf()` turns the CSRF check on. `app.with_header(name, value)` sends a header with
every following request (a header given to `request` itself wins), `app.without_header(name)` stops it, and
`app.with_bearer(token)` sends `Authorization: Bearer <token>`.

Each `TestApp` has its own database: `TEST_DATABASE_URL` when it is set, otherwise (with the `sqlite` feature) a
fresh in-memory SQLite database, otherwise `DATABASE_URL`. Before `TestApp::new` returns it drops every table and runs
every registered migration, so each test starts with an empty, migrated database. `app.db()` is the handle for
creating data, and `app.block_on(future)` runs async calls:

```rust,no_run
# use smeltery::db::factory::{Factory, Fake};
# use smeltery::db::prelude::*;
# use smeltery::testing::TestApp;
# mod my_app { pub fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder { app } }
# mod app { pub mod models { pub mod post {
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "posts")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub title: String,
#     pub body: String,
# }
# impl ActiveModelBehavior for ActiveModel {}
# } } }
# struct PostFactory;
# impl Factory for PostFactory {
#     type Entity = app::models::post::Entity;
#     fn definition(&self, fake: &mut Fake) -> app::models::post::ActiveModel {
#         app::models::post::ActiveModel { title: Set(fake.sentence(4)), ..Default::default() }
#     }
# }
# fn main() {
let app = TestApp::new(my_app::build);
let post = app.block_on(PostFactory.create(&app.db())).unwrap();
assert_eq!(app.get(&format!("/posts/{}", post.id)).status(), 200);
# }
```

## Deployment

A Smeltery app runs on a server as one program: the release binary, started by systemd, behind Caddy or nginx for
HTTPS. This section walks through it on Ubuntu 24.04; the commands use the app name `myapp`, the folder
`/srv/myapp` and the domain `app.example.com`.

### What goes on the server

| Item | What it is |
|---|---|
| `myapp` | the release binary (`target/release/myapp`) |
| `.env` | the production settings (or the same keys in the process environment, which win over `.env`) |
| `public/` | the static files, including the built `public/assets/css/app.css` (in a React or Vue app: `public/build/`, the output of `npm run build`) |
| `storage/` | uploads, logs, file sessions and the file cache; writable, kept across deploys; its folders are created when first needed |
| `database/` | the SQLite file and its `-wal` / `-shm` files (SQLite apps only); writable, kept across deploys |

Everything else is compiled into the binary: the routes and controllers, the migrations and seeders, the Mold views
and mail templates (release builds render compiled templates), the Sparks components and `sparks.js`, and the
Watchfire dashboard. Running it needs no `resources/`, no Rust sources and no Cargo. The binary also runs the
setup commands, so the server needs no `smeltery` CLI: `./myapp key:generate`, `./myapp storage:link`,
`./myapp migrate --force` (see [The `smeltery` command](#the-smeltery-command)).

The app root is the working directory (or `SMELTERY_ROOT`): `.env`, `public/`, `storage/`, `LOG_FILE` and a relative
SQLite path are read from there.

### Build

```text
smeltery build
```

It builds `public/assets/css/app.css` with Tailwind (`--minify`) when Tailwind is installed (see
[Pages and CSS](#pages-and-css); without it the committed `app.css` is kept) and then runs `cargo build --release`.
`app.css` is a normal file of the app, so a plain `cargo build --release` with the committed `app.css` gives the same
result. In a React or Vue app it runs `npm run build` instead (installing the packages first when `node_modules/` is
missing), which writes the JavaScript, the CSS and `manifest.json` into `public/build/`; this needs Node.js on the
build machine, not on the server.

The binary runs on the OS it was built for: build a Linux server's binary on Linux. Two ways:

- **On the server.** Install Rust (`rustup`, Rust 1.94 or newer) and a C compiler, then build in a checkout of the
  app:

  ```bash
  sudo apt install build-essential git
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  cargo build --release
  ```

- **On another Linux machine** (a CI runner, or WSL on Windows), with the same tools, then copy the binary. It links
  the system C library (glibc) dynamically, so build on the server's Ubuntu release or an older one.

The C compiler builds `ring` (TLS) and the bundled SQLite. OpenSSL is not used: TLS is rustls, so neither the build
nor the server needs OpenSSL. For outgoing TLS (SMTP, `ctx.http()`, Redis over `rediss://`) the server needs the OS
certificate bundle (`ca-certificates`, installed on Ubuntu by default).

### React and Vue apps

- `smeltery build` runs `npm run build`, which writes `public/build/` (the hashed JavaScript and CSS and
  `manifest.json`); deploy it with `public/`. The server needs no Node.js, no `node_modules/` and no
  `resources/js/`. A release binary started without `public/build/manifest.json` logs an error when it starts
  serving, and its pages load without their scripts.
- After a deploy with new assets, open pages notice the new asset version on their next visit and load the new
  build (a `409` with `X-Inertia-Location`).
- Serve the app over HTTPS: the browser encrypts the history of pages (`encrypt_history`, which the kits with
  authentication turn on) only in a secure context, and the `XSRF-TOKEN` cookie gets `Secure` when `APP_URL`
  starts with `https://`.
- Every web response sets the `XSRF-TOKEN` cookie, so every visitor gets a session, anonymous visitors and bots
  included; with `SESSION_DRIVER=database` that is one row in `sessions` per visitor without a cookie (expired rows
  are removed as with any session).

### Server layout

A system user without a login shell runs the app. It reads the app's files and writes only `storage/` and
`database/`:

```bash
sudo useradd --system --home-dir /srv/myapp --shell /usr/sbin/nologin myapp
sudo mkdir -p /srv/myapp/storage /srv/myapp/database
# copy the binary and public/ (see "Updating" for rsync / scp), then:
sudo chown -R root:root /srv/myapp
sudo chown -R myapp:myapp /srv/myapp/storage /srv/myapp/database
sudo chmod 750 /srv/myapp/storage /srv/myapp/database
sudo chmod 755 /srv/myapp/myapp
sudo chmod -R u=rwX,go=rX /srv/myapp/public
sudo install -o root -g myapp -m 640 /dev/null /srv/myapp/.env
```

| Path | Owner | Mode | Why |
|---|---|---|---|
| `/srv/myapp/myapp` | `root` | `755` | the app cannot replace its own binary |
| `/srv/myapp/.env` | `root:myapp` | `640` | holds `APP_KEY` and passwords; only the app's group reads it |
| `/srv/myapp/public/` | `root` | folders `755`, files `644` | served read-only |
| `/srv/myapp/storage/` | `myapp` | `750` | uploads, logs, sessions, cache: no other local user may read them |
| `/srv/myapp/database/` | `myapp` | `750` | SQLite creates the file and its `-wal` / `-shm` files here (`0600`); they hold password hashes and sessions |

On Unix Smeltery creates its own files private: the SQLite database `0600`, the log `0640`, cache entries and
session files `0600`, and the log and cache folders it creates `0750` and `0700`. The systemd unit below also sets
`UMask=0027` for everything else the process writes. A `database.sqlite` from an older setup keeps its mode:
`sudo chmod 600 /srv/myapp/database/database.sqlite*`.

### Production `.env`

Edit it with `sudo nano /srv/myapp/.env`:

```ini
APP_NAME="My App"
APP_ENV=production
APP_DEBUG=false
APP_URL=https://app.example.com
APP_KEY=

SERVER_HOST=127.0.0.1
SERVER_PORT=8000
TRUSTED_PROXIES=127.0.0.1
SHUTDOWN_TIMEOUT=30

LOG_LEVEL=info
LOG_FILE=storage/logs/smeltery.log

DATABASE_URL=sqlite://database/database.sqlite?mode=rwc

SESSION_DRIVER=database
SESSION_LIFETIME=120
CACHE_STORE=database
STATIC_CACHE_CONTROL=no-cache

MAIL_MAILER=smtp
MAIL_HOST=smtp.example.com
MAIL_PORT=587
MAIL_ENCRYPTION=starttls
MAIL_USERNAME=postmaster@example.com
MAIL_PASSWORD="the SMTP password"
MAIL_FROM_ADDRESS=hello@example.com
MAIL_FROM_NAME="My App"

WATCHFIRE_DASHBOARD=auth
```

Then the key: print a new one and paste it after `APP_KEY=`:

```bash
cd /srv/myapp && ./myapp key:generate --show
```

`--show` only prints a fresh key: it reads no `.env` and writes nothing, so any user can run it. (`sudo ./myapp
key:generate` without `--show` writes the key into `.env` itself and keeps the file's owner, group and mode; with
`APP_ENV=production` (or without `APP_ENV`) it does so only while `APP_KEY` is empty, unless `--force` is given.)

| Key | Production value | Why |
|---|---|---|
| `APP_ENV` | `production` | `migrate`, `migrate:rollback`, `migrate:fresh` and `db:seed` refuse to run without `--force`; `key:generate` keeps an existing key; the seeder of new apps creates no demo user; the `log` mailer withholds mail bodies; reset links never reach the log; the Watchfire dashboard is never open without signing in |
| `APP_DEBUG` | `false` | error pages show no internal details (new apps' `.env` has `true`) |
| `APP_URL` | `https://app.example.com` | `https` turns on the session cookie's `Secure` flag; password-reset links and mail use it |
| `APP_KEY` | from `key:generate --show` | encrypts session cookies and signs Spark state and uploads; `serve` refuses to start without one (at least 32 bytes) |
| `SERVER_HOST` / `SERVER_PORT` | `127.0.0.1` / `8000` | only the proxy on the same machine reaches the app |
| `TRUSTED_PROXIES` | `127.0.0.1` | the client IP (login throttle, `auth.ip()`, the request log) is read from the proxy's `X-Forwarded-For`; see [The server](#the-server) |
| `SHUTDOWN_TIMEOUT` | `30` | seconds a stop waits for running requests and agents; the systemd unit's `TimeoutStopSec` is larger |
| `LOG_LEVEL` | `info` | new apps' `.env` has `debug`, which logs every request |
| `LOG_FILE` | `storage/logs/smeltery.log`, or empty | stderr goes to the journal anyway; the file keeps one `.1` backup at `LOG_MAX_BYTES` (10 MiB), so it needs no logrotate. Empty logs to the journal only |
| `DATABASE_URL` | see below | |
| `SESSION_DRIVER` | `database` (or `file`) | a logout deletes the session on the server, so a copied session cookie stops working; `database` uses the `sessions` table of new apps. `cookie` stores nothing on the server: a copy of the cookie stays valid until the session expires (see [Authentication](#authentication)) |
| `CACHE_STORE` | `database` (or `redis`) | the store must span processes for Watchfire's coordination ("Several processes" in [Watchfire](#watchfire-agents-jobs-and-the-scheduler)); `memory` does not |
| `PUBSUB_DRIVER` | `auto`, or `database` / `redis` with several `serve` processes | `serve --no-agents` and `work` share Sparks pushes through the database (with the `pubsub_messages` table: in new apps with Watchfire, else from `smeltery pubsub:install`) or Redis; several full `serve` processes need it set explicitly (see [PubSub](#pubsub-messages-between-processes)) |
| `STATIC_CACHE_CONTROL` | `no-cache` | browsers revalidate `public/` files with a cheap 304, so a deploy's new `app.css` is seen at once; `public, max-age=31536000, immutable` for versioned file names |
| `HSTS_MAX_AGE` | `31536000` once HTTPS works for the domain (not set at first) | browsers then use HTTPS only for the domain for a year; sent only with an `https://` `APP_URL` |
| `MAIL_*` | `MAIL_MAILER=smtp` and the server's settings | with `log`, mail is not sent (see [Mail](#mail)) |
| `WATCHFIRE_DASHBOARD` | `auth` or `off` | `auth`: signed-in users the dashboard gate admits (below); `off`: no dashboard (404) |

`DATABASE_URL` per backend (the backend's feature must be compiled in, see [Database and models](#database-and-models)):

```ini
DATABASE_URL=sqlite://database/database.sqlite?mode=rwc
DATABASE_URL=postgres://myapp:secret@127.0.0.1:5432/myapp
DATABASE_URL=mysql://myapp:secret@127.0.0.1:3306/myapp
```

The SQLite path is relative to the app root; its directory must exist (the file is created). SQLite runs in WAL mode,
so `serve`, `work` and `migrate` can use the file at the same time, on a local disk.

Further keys for processes and Watchfire: `QUEUE_DRIVER` (`database`, or `redis` with `REDIS_URL` and the `redis`
feature), `WATCHFIRE_IN_SERVE` (`false`: `serve` never runs the agents),
`WATCHFIRE_API_ADDR` (the API address of `work`, e.g. `127.0.0.1:8001`), `WATCHFIRE_LOCK_STORE` (default
`CACHE_STORE`) and `WATCHFIRE_LEASE_TTL` (30 s); see [Watchfire](#watchfire-agents-jobs-and-the-scheduler). Every key
of the framework is in [Configuration](#configuration).

### First deploy

With the binary, `public/`, `.env` and the folders in place:

```bash
cd /srv/myapp
sudo -u myapp ./myapp migrate --force
sudo -u myapp install -d -m 750 storage/app storage/app/public
[ -L public/storage ] || sudo ln -s ../storage/app/public public/storage
```

- `migrate --force` runs the migrations compiled into the binary (as `myapp`, so the SQLite files belong to it).
- The link makes public uploads (`storage/app/public`) available at `/storage/…`. It is made with `ln` as root,
  because `public/` belongs to root; the folder is made as `myapp`, the owner of `storage/`. Root never needs to run
  the app binary here. (`./myapp storage:link` makes both in one step where one user owns `public/` and
  `storage/`. Run as root it only adds the link to a `public/` that no other user controls and leaves `storage/`
  alone; it never opens the log file.) Deploys keep the link (the deploy script below excludes it), and the `[ -L … ]`
  test makes running these lines again change nothing.
- `sudo -u myapp ./myapp db:seed --force` runs the seeders. The `DatabaseSeeder` of new apps creates the demo user
  only under `APP_ENV` `local` or `testing`, so in production it adds nothing; seeders of your own run as written.

### systemd: start on boot, restart on crash

`/etc/systemd/system/myapp.service`:

```ini
[Unit]
Description=myapp (Smeltery)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=myapp
Group=myapp
WorkingDirectory=/srv/myapp
ExecStart=/srv/myapp/myapp serve
Restart=on-failure
RestartSec=2
KillSignal=SIGTERM
TimeoutStopSec=45
UMask=0027
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/srv/myapp/storage /srv/myapp/database
PrivateTmp=true
PrivateDevices=true
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
ProtectClock=true
ProtectHostname=true
RestrictSUIDSGID=true
RestrictNamespaces=true
RestrictRealtime=true
LockPersonality=true
MemoryDenyWriteExecute=true
CapabilityBoundingSet=
AmbientCapabilities=
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now myapp
systemctl status myapp
journalctl -u myapp -f
curl -s http://127.0.0.1:8000/up
```

- `enable` starts the app at every boot; `--now` also starts it now.
- `WorkingDirectory` is the app root: `.env`, `public/`, `storage/` and the SQLite file are found from there.
- On SIGTERM (`systemctl stop` / `restart`) the app stops accepting connections, finishes running requests and stops
  its agents within `SHUTDOWN_TIMEOUT`, and exits with 0. `TimeoutStopSec` (45) gives it that time before systemd
  kills it.
- `Restart=on-failure` restarts the app after a crash and after an exit with an error (code 1: the port is taken,
  `APP_KEY` is missing, the database cannot be reached, a boot hook failed), but not after a clean stop. A handler
  that panics answers 500 and the app keeps running.
- With a `CACHE_STORE` that spans processes (`database`, `redis`, `memcached`, `file`), after a crash the restarted
  process waits for the dead one's agent leases to expire (`WATCHFIRE_LEASE_TTL`, 30 s)
  before it runs those agents again; after a clean stop or `restart` the leases are released and the agents start at
  once.
- `Type=simple`: the app does not use `sd_notify` or socket activation.
- The `ProtectSystem` / `ReadWritePaths` lines make the whole file system read-only for the app except `storage/`
  and `database/`. With PostgreSQL or MySQL on the same machine, add `After=postgresql.service` (or
  `mysql.service`) and drop `/srv/myapp/database`.
- `UMask=0027`: files the process creates are never readable by other users. The lines after `PrivateTmp` take
  away what a web app does not use: `/home`, devices, kernel settings and modules, other address families than
  Unix sockets, IPv4 and IPv6, every capability, system calls outside `@system-service`, and memory that is both
  writable and executable. `systemd-analyze security myapp` rates the unit. A Smeltery app with SQLite runs under
  all of them (serving pages, migrations, Watchfire agents, a clean stop on SIGTERM).
- `/up` answers `200 OK` without a session or the database (see [Routes and controllers](#routes-and-controllers)).

**One process or two.** `serve` runs the web server and the Watchfire agents, queue workers and scheduler together:
one unit is a complete deployment. To run the background work in its own process (for example to restart the web server
without stopping long-running agents), run `serve --no-agents` and `work` as two units that replace `myapp`
(`myapp-web` listens on the same port, so stop and disable `myapp` first):

```bash
sudo systemctl disable --now myapp
sudo cp /etc/systemd/system/myapp.service /etc/systemd/system/myapp-web.service
sudo cp /etc/systemd/system/myapp.service /etc/systemd/system/myapp-work.service
```

Then change two lines in each copy:

| File | `Description=` | `ExecStart=` |
|---|---|---|
| `myapp-web.service` | `myapp web (Smeltery)` | `/srv/myapp/myapp serve --no-agents` |
| `myapp-work.service` | `myapp Watchfire (Smeltery)` | `/srv/myapp/myapp work` |

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now myapp-web myapp-work
```

Back to one process: `sudo systemctl disable --now myapp-web myapp-work`, then `sudo systemctl enable --now myapp`.

Anvil's sockets can have a process of their own as well: see
[A separate socket process](#a-separate-socket-process).

With more than one `serve` process (`serve` next to `work`, or several behind a load balancer), set `PUBSUB_DRIVER`
to `database` or `redis`: under the default `auto`, a `serve` that runs its background work keeps its messages,
including the auth events that end sessions and tokens elsewhere, inside its own process (see
[PubSub](#pubsub-messages-between-processes)).

The processes coordinate through the cache store's locks when `CACHE_STORE` (or `WATCHFIRE_LOCK_STORE`) spans
processes: `database`, `redis`, `memcached`, or `file` on one machine. Then each agent runs in one process at a time
(the others hold it in `standby` and take it over when its process stops or dies), each scheduled tick runs once, and
the `database` queue gives each job to one worker. The web process's dashboard shows and controls the agents of the
`work` process. With `memory`, `array` or `null` every process runs every agent and every tick itself. Details:
"Several processes" in [Watchfire](#watchfire-agents-jobs-and-the-scheduler). `agents:logs <name>` reads an agent's
recent lines from the process that runs it; `journalctl -u myapp-work` has the full log. `work` exits with 1 when the app
registers no background work. A headless app (no web routes) runs only `work`; set `WATCHFIRE_API_ADDR=127.0.0.1:8001`
for its API and the `agents:*` commands.

### HTTPS: Caddy or nginx

The app listens on `127.0.0.1:8000`; a reverse proxy on the same machine terminates HTTPS. The app compresses its
responses itself and never buffers its two event streams (`/_sparks/stream` for Sparks push and the live dashboard,
`/_watchfire/api/events`); both send a keep-alive every 15 seconds. The app also sends its own security headers
(`X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`, `Content-Security-Policy: frame-ancestors`, and
`Strict-Transport-Security` with `HSTS_MAX_AGE`; see [The server](#the-server)), so the proxy needs no header
settings of its own; neither configuration below adds any, and an nginx `add_header` for one of them would send it a
second time.

**Caddy** gets and renews the certificate itself, sets `X-Forwarded-For`, `-Proto` and `-Host`, has no body-size
limit by default and flushes event streams at once. `/etc/caddy/Caddyfile`:

```text
app.example.com {
    reverse_proxy 127.0.0.1:8000
}
```

```bash
sudo apt install caddy
sudo nano /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

A write timeout in Caddy's global options (`servers { timeouts { write 60s } }`) ends each event stream at that
limit; the browser's `EventSource` then reconnects. Leave `write` unset for the server that carries the app to keep
streams open. When a client sends no `Accept-Encoding`, Caddy asks the app for gzip and decompresses the answer
itself, so that client gets a weak `ETag` and no `Content-Length`; this is harmless, and
`transport http { compression off }` inside `reverse_proxy` turns it off:

```text
app.example.com {
    reverse_proxy 127.0.0.1:8000 {
        transport http {
            compression off
        }
    }
}
```

**nginx** with a Let's Encrypt certificate from certbot. `/etc/nginx/sites-available/myapp`:

```nginx
server {
    listen 80;
    listen [::]:80;
    server_name app.example.com;

    # At least UPLOAD_MAX_BYTES (10 MiB by default); nginx's own default is 1 MiB.
    client_max_body_size 10m;

    location / {
        proxy_pass http://127.0.0.1:8000;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $host;
    }

    # Server-sent events: passed on unbuffered.
    location ~ ^/(_sparks/stream|_watchfire/api/events) {
        proxy_pass http://127.0.0.1:8000;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $host;
        proxy_buffering off;
        proxy_cache off;
    }
}
```

```bash
sudo apt install nginx certbot python3-certbot-nginx
sudo ln -s /etc/nginx/sites-available/myapp /etc/nginx/sites-enabled/myapp
sudo nginx -t && sudo systemctl reload nginx
sudo certbot --nginx -d app.example.com
```

`certbot --nginx` adds the `listen 443 ssl` lines and the redirect from HTTP to the server block, and renews the
certificate with a systemd timer. The app's `REQUEST_TIMEOUT` (30 s) is below nginx's default 60 s
`proxy_read_timeout`. `proxy_read_timeout` and `send_timeout` (both 60 s by default) limit the time between two
reads or writes, not a whole response, so the 15-second keep-alives keep event streams open; a value under 15 s in
the event-stream location would cut them.

**Plesk.** The app runs as above (binary, `.env` and folders in a directory of their own, the systemd unit, created
as root over SSH); Plesk's nginx is the proxy. For the domain, turn on SSL/TLS with Let's Encrypt, open **Apache &
nginx Settings**, turn off **Proxy mode** and put this into **Additional nginx directives** (Plesk's own
configuration already has a `location /`, so these blocks use regular expressions, which nginx prefers over prefix
locations, in the order written):

```nginx
client_max_body_size 10m;

location ~ ^/(_sparks/stream|_watchfire/api/events) {
    proxy_pass http://127.0.0.1:8000;
    proxy_http_version 1.1;
    proxy_set_header Connection "";
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Forwarded-Host $host;
    proxy_buffering off;
}

location ~ ^/ {
    proxy_pass http://127.0.0.1:8000;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Forwarded-Host $host;
}
```

A Smeltery app is a long-running process with its own port. Hosting that offers only PHP or CGI, with no SSH access
and no way to keep a process running, cannot run it.

### The Watchfire dashboard in production

Outside `APP_ENV=local`, `/_watchfire` is open only to signed-in users that the app's dashboard gate admits; without
a gate nobody gets in. A gate in `app/agents/mod.rs` that admits the users listed in an `ADMIN_EMAILS` key of
`.env` (a key of the app, read with `env`):

```rust,no_run
# mod app { pub mod models { pub mod user {
# use smeltery::db::prelude::*;
# #[sea_orm::model]
# #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
# #[sea_orm(table_name = "users")]
# pub struct Model {
#     #[sea_orm(primary_key)]
#     pub id: i64,
#     pub email: String,
#     pub password: String,
#     pub remember_token: Option<String>,
# }
# impl ActiveModelBehavior for ActiveModel {}
# impl smeltery::auth::Authenticatable for Model {
#     fn auth_id(&self) -> i64 { self.id }
#     fn password_hash(&self) -> &str { &self.password }
#     fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
# }
# } } }
use crate::app::models::user::Model as User;
use smeltery::config::env;
use smeltery::watchfire::prelude::*;

pub fn register(w: &mut Watchfire) {
    w.dashboard_gate(|auth, _app| async move {
        let admins: String = env("ADMIN_EMAILS", "");
        let Some(user) = auth.user::<User>().await? else {
            return Ok(false);
        };
        Ok(admins.split(',').any(|email| email.trim() == user.email))
    });
}
# fn main() {}
```

With `ADMIN_EMAILS=ops@example.com` in `.env`, that user signs in at `/login` and opens `/_watchfire`; other users
get 403, guests are sent to `/login`. `WATCHFIRE_DASHBOARD=off` removes the dashboard (404).

The JSON API under `/_watchfire/api` needs `Authorization: Bearer <token>` outside local development. On the server,
`cd /srv/myapp && sudo -u myapp ./myapp agents:list` (and `agents:start|stop|pause|resume|restart|logs`) call it
with the token, which they derive from `APP_KEY`. For other clients, `agents:token` prints the token (the hex
HMAC-SHA256 of `watchfire-api` keyed with `APP_KEY`, `smeltery::watchfire::web::api_token`) to standard output:

```bash
cd /srv/myapp && sudo -u myapp ./myapp agents:token
```

The token changes only with `APP_KEY`; a new `APP_KEY` revokes it (and signs every user out).

### Updating

1. Build the new binary (and `app.css`).
2. Copy `public/` and the binary next to the running one; `mv` it into place (a running binary cannot be
   overwritten in place, but it can be replaced by a rename).
3. `sudo -u myapp ./myapp migrate --force` with the NEW binary: the migrations are compiled into it.
4. `sudo systemctl restart myapp`: the old process finishes its running requests and stops its agents (within
   `SHUTDOWN_TIMEOUT`), then the new one starts. Between the two the proxy answers 502 for a moment. Jobs that were
   running go back to the queue.

With two processes: migrate, then `sudo systemctl restart myapp-work myapp-web`. Agents stop in the old `work`
process and start in the new one as soon as it is up.

A deploy script, run from the app's folder on a Linux build machine (`deploy` is an account on the server with sudo):

```bash
#!/usr/bin/env bash
set -euo pipefail
APP=myapp
SERVER=deploy@app.example.com
DIR=/srv/myapp

smeltery build
# A new private folder (mode 700) of the deploy account on the server: no other user can change the upload.
STAGE=$(ssh "$SERVER" mktemp -d)
rsync -a --delete --exclude /storage public/ "$SERVER:$STAGE/public/"
scp "target/release/$APP" "$SERVER:$STAGE/$APP.new"
ssh "$SERVER" "set -e
  sudo rsync -a --delete --chown=root:root --chmod=D755,F644 --exclude /storage $STAGE/public/ $DIR/public/
  sudo install -o root -g root -m 755 $STAGE/$APP.new $DIR/$APP.new
  rm -rf $STAGE
  [ -f $DIR/$APP ] && sudo cp -p $DIR/$APP $DIR/$APP.previous
  sudo mv $DIR/$APP.new $DIR/$APP
  cd $DIR && sudo -u $APP ./$APP migrate --force
  sudo systemctl restart $APP
  sleep 2 && curl -fsS http://127.0.0.1:8000/up"
```

`--chown` and `--chmod` give `public/` the owner and modes of the table under "Server layout", whoever uploaded the
files. `--exclude /storage` keeps the `public/storage` link. The previous binary stays as `myapp.previous`. With two
processes, the restart line is `sudo systemctl restart myapp-work myapp-web`.

**Rollback.** When the new release added migrations, undo them with the NEW binary first (an older binary does not
know them and refuses), then put the old binary back:

```bash
cd /srv/myapp
sudo -u myapp ./myapp migrate:rollback --force
sudo mv myapp.previous myapp
sudo systemctl restart myapp
# with two processes instead: sudo systemctl restart myapp-work myapp-web
```

`migrate:rollback` reverts the last batch, the migrations of the last `migrate` run, by running each migration's
`down`; `--step N` reverts the last N migrations instead. A rollback undoes exactly what those `down` functions do,
so check them before relying on it (`make:migration` writes a `down` that drops what `create_*` and `add_*`
migrations add; other names get an empty `down`): a `down` that does nothing leaves the change in the database, and
running that migration again can then fail (for example on a column that already exists).

### Operations

- **Logs:** `journalctl -u myapp` (`-f` follows, `--since "1 hour ago"`); with `LOG_FILE`, also
  `storage/logs/smeltery.log` and its `.1` backup. Requests are logged at `debug`; secrets in URLs appear as
  `[redacted]`.
- **Health checks:** point an uptime monitor or load balancer at `https://app.example.com/up` (`200`, body `OK`).
- **Backups:** `.env` (without its `APP_KEY` every session, signed Spark state and the API token change), the
  database and `storage/app/`. Backups hold password hashes and sessions, so they go into a folder only root can
  read:

  ```bash
  sudo install -d -m 700 /var/backups/myapp
  # SQLite (apt install sqlite3): a consistent copy while the app runs
  sudo sqlite3 /srv/myapp/database/database.sqlite ".backup '/var/backups/myapp/db.sqlite'"
  # PostgreSQL
  sudo -u postgres pg_dump -Fc myapp | sudo tee /var/backups/myapp/myapp.dump > /dev/null
  # MySQL (root signs in through the socket on Ubuntu)
  sudo mysqldump --single-transaction myapp | sudo tee /var/backups/myapp/myapp.sql > /dev/null
  ```

  Copying the SQLite file alone misses what is still in `database.sqlite-wal`; `.backup` includes it. With
  `QUEUE_DRIVER=redis` the queued jobs and dead letters are on the Redis server, kept by its own persistence (RDB or
  AOF).
- **Rotating `APP_KEY`** (a new key from `key:generate --show` as under "Production `.env`", pasted into `.env`,
  then a restart) signs every user out (session and remember-me
  cookies), invalidates the Spark state in open pages and pending signed uploads, and changes the Watchfire API
  token. Database rows stay as they are.
- **Clearing the cache:** `sudo -u myapp ./myapp cache:clear` keeps Watchfire's leases and schedule claims.
  `cache:clear --all` removes them too: the processes take their agents again within `WATCHFIRE_LEASE_TTL`, and
  until then a singleton agent can run in two processes and a tick of the current minute can run again.
- **Sharing a cache store:** Watchfire's leases and schedule claims, the cache entries and locks of the app all live
  under `CACHE_PREFIX` (default `<app name>_cache_`). Two apps, or the staging and production copy of one app, on one
  Redis database, memcached server or database need different `CACHE_PREFIX` values (or separate Redis databases or
  tables); with the same prefix they take each other's leases and claims and read each other's entries. Memcached
  cannot flush by prefix, so a shared memcached is emptied for every app by `cache:clear --all`.
- **Sharing a Redis queue:** with `QUEUE_DRIVER=redis`, two apps (or the staging and production copy of one app) on
  one Redis database need different `QUEUE_PREFIX` values (default `<app name>_queue_`) or separate Redis databases;
  with the same prefix their workers take each other's jobs.
- **HTTP caches:** web pages are per visitor (their session, the CSRF token in their forms): a CDN or a caching
  proxy in front of the app caches only `public/` files (`STATIC_CACHE_CONTROL`), never pages.

### Checklist

- [ ] `smeltery build` on Linux; binary, `public/` (with `app.css`), `storage/`, `database/` on the server
- [ ] a `myapp` system user; only `storage/` and `database/` writable (`750`); `.env` `640` `root:myapp`;
  `UMask=0027` in the unit
- [ ] `.env`: `APP_ENV=production`, `APP_DEBUG=false`, `APP_URL=https://…`, a new `APP_KEY`, `SERVER_HOST=127.0.0.1`,
  `TRUSTED_PROXIES=127.0.0.1`, `LOG_LEVEL=info`, `SESSION_DRIVER=database`, `MAIL_MAILER=smtp` with the `MAIL_*`
  keys, `DATABASE_URL`
- [ ] `WATCHFIRE_DASHBOARD=auth` with a `dashboard_gate`, or `off`
- [ ] `migrate --force`, the `public/storage` link
- [ ] the systemd unit enabled (`enable --now`), `curl http://127.0.0.1:8000/up` answers `OK`
- [ ] Caddy or nginx with HTTPS; `client_max_body_size` at least `UPLOAD_MAX_BYTES`; the event streams unbuffered
- [ ] two processes only with a shared `CACHE_STORE` (`database`, `redis`, `memcached`); the `pubsub_messages` table
  (new apps with Watchfire have it, others get it from `smeltery pubsub:install`) unless `CACHE_STORE=redis`
- [ ] with `QUEUE_DRIVER=redis`: `maxmemory-policy noeviction` (or `volatile-*`) on the Redis server, persistence
  (RDB or AOF) that fits how many jobs may be lost, and a `QUEUE_PREFIX` per app that does not overlap `CACHE_PREFIX`
- [ ] an uptime monitor on `/up`; backups of `.env`, the database and `storage/app/`

## Demo apps

Three apps in [`examples/`](examples/), each listing the exact commands in its README (the two Smeltery apps are
built with the `smeltery` generators):

- [`examples/web-demo`](examples/web-demo): a Posts CRUD with validation and image uploads, authentication, a live
  counter fed by a scheduled job over server-sent events, and a rate-limited, checkpointed scraper agent with the
  live Watchfire dashboard.
- [`examples/headless-demo`](examples/headless-demo): agents only, run with `smeltery work`: a poller and a
  deliberately flaky worker whose restarts and backoff show in `smeltery agents:runs`.
- [`examples/flutter-client`](examples/flutter-client): a Flutter app on a Smeltery app's API tokens and broadcasting:
  sign-in, a public, a private and a presence channel, whispers, and the return to the sign-in when the token ends.

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

## Contributing

Issues and pull requests are welcome at <https://github.com/smelteryworks/smeltery>. Run `cargo fmt --all`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings` and `cargo test --workspace --all-features`
before sending a change.
