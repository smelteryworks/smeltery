# My App: guide for coding agents

This is a [Smeltery](https://github.com/smelteryworks/smeltery) application: a batteries-included Rust web
framework. One crate; every folder is a Rust module.

## Layout

| Path | What lives there |
|---|---|
| `bootstrap/main.rs` | The binary. Calls `smeltery::run` with the app's `build` function. |
| `bootstrap/app.rs` | The library root. Pulls in the folders below with `#[path]` and defines `build`, which registers config, migrations, seeders, commands and routes. |
| `app/` | Application code: `commands`, `controllers`, `helpers`, `jobs`, `mail`, `middleware`, `models`, `providers`, `services`, `sparks`. |
| `config/` | Typed settings structs, read with `smeltery::config::env("KEY", default)`. |
| `database/` | `migrations`, `seeders`, `factories`, each with a `mod.rs` that registers them. The SQLite database file `database.sqlite` (not committed). |
| `routes/web.rs`, `routes/api.rs` | Route registration. API routes are registered with `api_routes`. `routes/channels.rs` declares the broadcasting channels; `app/events/` holds the events. |
| `resources/views/` | Mold templates (`*.mold.html`): pages, `layouts/`, `components/` (`<x-…>` Mold components), `sparks/` (Spark views). |
| `resources/css/app.css` | The Tailwind entry point: the theme colours and the component classes (`btn-primary`, `form-input`, `panel`, ...). Tailwind builds `public/assets/css/app.css` from it and from the classes in the views. |
| `public/` | Files served as-is. |
| `storage/` | Files the app writes: `app/public` and `app/private` (the app's own files), `framework` (Smeltery's working files: the file cache, file sessions, upload temp files), `logs`. Not committed. |
| `tests/` | Integration tests using `smeltery::testing::TestApp`. |
| `.env` | Local settings and secrets. Not committed; `.env.example` lists the keys. |

## Conventions

- One module per folder. Each folder has a `mod.rs` that lists its modules.
- Marker comments (`// smeltery:mods`, `// smeltery:models`, `// smeltery:migrations`, `// smeltery:seeders`,
  `// smeltery:commands`, `// smeltery:routes`, `// smeltery:channels`) show where generated lines go. New lines are added
  directly above the marker. Keep the markers in place.
- Prefer the generators (below) over writing these files by hand: they create the file and its registration
  lines together. They never overwrite an existing file.
- Config is typed Rust: add a field to a struct in `config/` and read it with `env("KEY", default)`. The real
  environment overrides `.env`.
- Secrets live only in `.env`, never in code or logs.

## Database

- `DATABASE_URL` in `.env` picks the database. Handlers take `db: Db`; elsewhere use `app.db()`.
- **Models** (`app/models/<name>.rs`) are SeaORM entities: `pub struct Model` plus `pub use <name>::Model as
  <Name>;` in `app/models/mod.rs`. `use smeltery::db::prelude::*;` brings in what they need. Queries:
  `Post::all(&db)`, `Post::find(&db, id)`, `Post::find_or_404(&db, id)`, `Post::count(&db)`,
  `Post::create(&db, post::ActiveModel { title: Set(…), ..Default::default() })`, `post.update(&db, |m| { … })`,
  `post.delete(&db)`, and SeaORM filters on `Post::query()`. `Found<Post>` in a handler loads the record named by
  the route's last parameter, or answers 404. `created_at` / `updated_at` are filled automatically.
- **Migrations** (`database/migrations/m<timestamp>_<name>.rs`) implement `Migration` with `up` / `down` using the
  schema builder (`schema.create("posts", |t| { t.id(); t.string("title"); t.timestamps(); })`) and are registered
  in `database/migrations/mod.rs`, oldest first. Never edit a migration that has run; add a new one.
- **Seeders** (`database/seeders/`) implement `Seeder`; `DatabaseSeeder` runs on `smeltery db:seed` and
  is where development data goes. **Factories** (`database/factories/`)
  implement `Factory` and fill a model with `Fake` values: `UserFactory.create(&db)`,
  `UserFactory.count(3).create(&db)`.
- Tests: `TestApp::new(build)` runs every migration on a fresh database (in-memory SQLite, or
  `TEST_DATABASE_URL`); `app.db()` gives the handle for test data.

## Sessions and validation

- **Authentication** is off in this app (it was created without the `temper` building block). To turn it on, copy
  from an app made with `smeltery new <name>` (authentication on):
  1. `app/providers/temper.rs` (with `pub mod temper;` in `app/providers/mod.rs`), `app/actions/` (with
     `pub mod actions;` in `app/mod.rs`), `app/controllers/dashboard.rs` and `app/controllers/settings.rs` (with
     their `pub mod` lines in `app/controllers/mod.rs`), `resources/views/auth/`, `resources/views/settings/`,
     `resources/views/dashboard.mold.html`, the routes in `routes/web.rs`, the login links of
     `resources/views/layouts/app.mold.html`, and the `password_reset_tokens` and
     `add_two_factor_columns_to_users_table` migrations.
  2. On `User`: the `email_verified_at` field (a new migration adds the column) and the four `two_factor_*` fields,
     `impl smeltery::auth::MustVerifyEmail` and `impl smeltery::temper::TwoFactorAuthenticatable`.
  3. In `bootstrap/app.rs`: `use smeltery::temper::TemperExt as _;` and `.temper(app::providers::temper::temper())`
     in `build` (it registers the `auth`, `guest`, `verified` and `password.confirm` middleware aliases).
  4. The rest from the same app: the demo user of `database/seeders/database_seeder.rs` (created under `APP_ENV`
     `local` or `testing` only), `AUTH_VERIFICATION_EXPIRE=60` and `AUTH_PASSWORD_TIMEOUT=10800` in `.env` and
     `.env.example`, and the authentication tests of `tests/http.rs`.
- **Sessions** (`session: smeltery::session::Session`): `get`, `insert`, `remove`, `flash("status", "…")` for the
  next request. `SESSION_DRIVER=database` (this app's `.env`: the `sessions` table of its migrations, so a
  session can be ended on the server), `cookie` (encrypted cookie) or `file`
  (`storage/framework/sessions/`); `SESSION_LIFETIME` in minutes. Sessions and cookies are encrypted with `APP_KEY`.
- **CSRF**: every `POST`/`PUT`/`PATCH`/`DELETE` web form needs `@csrf` inside the `<form>`; `/api` routes are exempt.
  Use `@method("PUT")` / `@method("DELETE")` for those verbs in HTML forms.
- **Validation**: a form struct derives `Deserialize` and `smeltery::Validate`, with rules per field
  (`#[validate(required, email, max = 255, unique(table = "users", column = "email"))]`); the handler takes
  `Valid(form): Valid<TheForm>`. On failure the browser is sent back with the errors and old input, shown in Mold
  with `@error("field") {{ message }} @enderror` and `{{ old("field") }}`; JSON requests get a 422 with the errors.
  Empty fields count as missing, so `Option` fields become `None`.
- Templates show flash messages with `session("status")` (the layout already does for `status` and `error`).

## Broadcasting (Anvil)

- `bootstrap/app.rs` installs Anvil with `.anvil(routes::channels::channels)`: a WebSocket endpoint on the app's own
  port (`/app/<ANVIL_APP_KEY>`, the Pusher protocol 7), `POST /api/broadcasting/auth` for clients with a bearer token (it answers 404 until the
  app has a bearer guard such as Hallmark's API tokens), and the `Anvil` service.
- `routes/channels.rs` declares the channels: `c.public("announcements")` (anyone with the app key may subscribe, so
  only public data goes there). Add channels above
  `// smeltery:channels`.
- Events are structs in `app/events/` with `#[derive(Serialize, BroadcastEvent)]` and
  `#[broadcast(public = "…")]`; `app/events/announcement_posted.rs`
  is the example. Send with `anvil.send(&event).await?` (a handler takes `anvil: smeltery::anvil::Anvil`; jobs and
  agents use `smeltery::anvil::Anvil::of(&app)`); clients receive the JSON under `App\Events\<TypeName>`. Send after a
  transaction commits.
- The key and the signing secret are derived from `APP_KEY` (the `ANVIL_*` lines in `.env` are commented). Set
  `ANVIL_APP_KEY` when clients are built with it, because a new `APP_KEY` changes the derived key.
  `PUBSUB_DRIVER` carries events between processes (`serve --no-agents` and `work`); the `pubsub_messages`
  migration is there for its `database` driver.
- Mold pages listen without JavaScript: a `#[spark(stream)]` component's method marked
  `#[on("anvil:<channel>", "App\\Events\\<Event>")]` runs when that event is broadcast (`smeltery make:spark Name
  --listen <channel> --event <Event>` writes one). Examples: `app/sparks/announcements.rs` on the home page. Other clients (mobile apps, scripts) use `pusher-js` or a Pusher client library with the key.
- Tests: `smeltery::anvil::testing` (`AnvilSpy`, `TestSocket`); see `tests/broadcasting.rs`.

## Cache

- Take `cache: smeltery::cache::Cache` as a handler argument (or `app.cache()`). Values are any `Serialize`/`Deserialize` type: `cache.get::<T>("key")`,
  `put("key", &value, Duration::from_secs(60))`, `forever`, `add` (only when absent; atomic), `forget`, `has`, `pull`,
  `increment("hits", 1)` / `decrement`, `flush()`, and `cache.remember("key", ttl, || async { Ok(value) }).await?`
  (computes on a miss).
- Locks: `let lock = cache.lock("name", Duration::from_secs(60)); if lock.get().await? { …; lock.release().await?; }`
  (`lock.block(timeout)` waits for it). On a shared store they exclude every process (`serve` and `work`).
- `CACHE_STORE` picks the default store: `database` (the `cache` / `cache_locks` tables, migration
  `create_cache_tables`), `redis` (`REDIS_URL`, feature `redis` of `smeltery`), `memcached` (`MEMCACHED_SERVERS`,
  feature `memcached`), `file` (`storage/framework/cache/`), `memory` (this process only), `array` or `null`.
  `cache.store("redis")?` uses another store. Redis is the recommended production store (shared by every process
  and machine, native atomic operations and locks); memcached is the second choice (its client is synchronous and
  runs on blocking threads). `CACHE_PREFIX` goes in front of every key.
- Tests use the `array` store (a cache per `TestApp`); `smeltery cache:clear [store]` empties a store.

## Mail

- A mail is a struct in `app/mail/<name>.rs` with `#[derive(Mold)]` and `#[mold("mail/<name>")]` (its HTML body is
  `resources/views/mail/<name>.mold.html`; the fields are its variables) implementing `Mailable`
  (`fn envelope(&self) -> Envelope { Envelope::new().to(…).subject(…) }`). `smeltery make:mail Name` creates both.
- Send with a `mailer: smeltery::mail::Mailer` handler argument (or `Mailer::of(&app)?`):
  `mailer.send(Welcome { … }).await?`.
  `bootstrap/app.rs` installs mail with `.mail()`.
- `MAIL_MAILER` picks the transport: `log` (default: the mail is written to the log), `smtp` (`MAIL_HOST`,
  `MAIL_PORT`, `MAIL_USERNAME`, `MAIL_PASSWORD`, `MAIL_ENCRYPTION=starttls|tls|none`, `MAIL_TIMEOUT`) or `fake`.
  The sender is `MAIL_FROM_ADDRESS` / `MAIL_FROM_NAME`.
- Tests run with `APP_ENV=testing`, where mail goes to a fake mailbox:
  `Mailer::of(app.app()).unwrap().mailbox().unwrap().assert_sent::<Welcome>(|mail, email| …)`.

## Console commands

`app/commands/<name>.rs` implement `smeltery::console::Command` (`name`, `about`, `async fn run`) and are registered in
`app/commands/mod.rs`. Run them with `smeltery <name>`; `smeltery help` lists every command.

## Views (Mold)

- A page is a struct with `#[derive(smeltery::Mold)]` and `#[mold("name")]`, rendering
  `resources/views/<name>.mold.html`. Its fields are the template's variables. A handler returns the struct (or
  `smeltery::Result<TheStruct>`). See `app/controllers/home.rs` and `resources/views/home.mold.html`.
- Syntax: `{{ expr }}` (escaped), `{!! expr !!}` (raw), `@if`/`@elseif`/`@else`/`@endif`, `@for(x in items)` …
  `@empty` … `@endfor`, `@extends("layouts/app")` with `@section`/`@yield`, `@include("partials/x")`, and
  components `<x-card title="…">body</x-card>` from `resources/views/components/card.mold.html` (the body is
  `{{ slot }}`).
- `{{ }}` escapes for HTML: right for text and quoted attribute values. A value inside JavaScript (`x-data`,
  `x-on:click`, `@click`, the arguments of `wire:click`) is `{{ value | json }}` without quotes around it; inside
  `<script>` it is `{!! value | json !!}`. A link from data is `{{ link | url }}`. Quote every attribute value.
- Debug builds read the templates at runtime, so edits show on the next request. Release builds compile them into
  the binary. A template variable without a matching field is a compile error naming the file and line.

## Sparks (live components)

- A Spark is a struct in `app/sparks/<name>.rs` with `#[derive(Serialize, Deserialize, Default, Spark)]` (its state)
  and a view `resources/views/sparks/<name>.mold.html`; `smeltery make:spark Name` creates both and registers the
  Spark in `app/sparks/mod.rs` (`s.add::<name::Name>();`). `bootstrap/app.rs` wires them with
  `.sparks(app::sparks::register)`, and the layout loads the runtime with `@sparksScripts` in `<head>`. See
  `app/sparks/counter.rs`, shown on the home page.
- A page shows a Spark with `@spark("counter", { start: 0 })`. Props named like a field set it; every prop is
  readable with `ctx.prop("start")` in `mount`.
- **Actions** are the `pub async fn` methods (taking `&mut self`) of the `#[actions] impl` block; nothing else can be
  called from the page. They may take `ctx: &mut SparkCtx` and then parameters (`wire:click="add(10)"`). Hooks:
  `mount` (first render) and `updated(ctx, field)` (after a `wire:model` change). `#[guard(auth)]` /
  `#[guard(guest)]` restrict an action. `SparkCtx` gives `db()`, `auth()`, `user_id()`, `session()`, `prop()`,
  `redirect(url)` (a path on this site or an `APP_URL` address), `redirect_away(url)` (another site, `http(s)://`),
  `dispatch(event, payload)`, `flash(key, value)` and `validate(self)`.
- Only `#[spark(model)]` fields accept values from the page, and they take no objects; a struct field the page edits
  is `#[spark(model(fields = "title, body"))]` (`wire:model="form.title"`). The state travels signed (with `APP_KEY`)
  in the page and bound to the visitor's session and user: readable, not changeable, so never put secrets in a
  Spark's fields. A snapshot can be sent again within its session, so every action re-checks permissions with
  `ctx.auth()` and the database. Limits are set in `app/sparks/mod.rs`: `s.max_calls(n)`, `s.upload_quota(…)`.
- In the view: `wire:click="action"`, `wire:submit="save"`, `wire:model="field"` (`.live`, `.debounce.300ms`,
  `.blur`, `.change`), `wire:keydown.enter="save"`, `wire:loading` (`.remove`, `.class="…"`, `.class.remove="…"`
  that keeps the element's space, `.attr="disabled"`), `wire:target="save"`, `wire:poll.5s="action"`,
  `wire:key="…"`, `wire:click="$refresh"`; `@error("field")` shows validation messages. A value from data in an
  action's arguments goes through `json`: `wire:click="remove({{ item.id | json }})"`.
- Tests: `smeltery::sparks::testing::TestSpark::from_html(&app.get("/").text(), "counter")`, then
  `.set("step", 5)`, `.call("increment", smeltery::json!([])).send(&app)`, and check `.data()` / `.html()`.

## AI-agent support (Bellows)

- `smeltery bellows:install --mcp` adds `.mcp.json`, which registers the app's MCP server (`smeltery bellows:mcp`:
  routes, models, schema, config key names, errors, docs, generators and tests for coding agents).
- `.bellows/guidelines.md` lists the conventions to follow.
- `.bellows/skills/` has step-by-step guides: `crud-resource.md`, `spark.md`, `migration.md`, `mail.md`, `broadcast.md`.
- `smeltery bellows:install [--mcp] [--skills] [--guidelines] [--all]` adds the parts that are missing; it never
  overwrites a file.

## Commands

| Command | What it does |
|---|---|
| `smeltery serve` | Builds and runs the app, restarting it when Rust code or `Cargo.toml` changes. Rebuilds the CSS with Tailwind in watch mode when Tailwind is installed. |
| `smeltery tailwind:install` | Downloads the pinned Tailwind CSS standalone binary into the per-user Smeltery folder, where `serve` and `build` find it. |
| `smeltery test` | Runs `cargo test`; extra arguments are passed through. |
| `smeltery build` | Builds the CSS with Tailwind (when it is installed) and the release binary. |
| `smeltery key:generate` | Writes a new `APP_KEY` into `.env` (`--show` prints it). With `APP_ENV=production` an existing key is kept unless `--force` is given. |
| `smeltery storage:link` | Links `public/storage` to `storage/app/public`. |
| `smeltery migrate` | Runs the pending migrations. `migrate:rollback [--step N]`, `migrate:fresh [--seed]` and `migrate:status` manage them. |
| `smeltery db:seed [--class Name]` | Runs the seeders. |
| `smeltery cache:clear [store]` | Removes every entry of the default cache store (or the named one). |
| `smeltery make:model Name [field:type ...]` | A model. Types: `string`, `text`, `integer`, `bigint`, `bool`, `float`, `date`, `datetime`, `json`, `uuid`, `<name>_id:foreign`, `file` (an upload: the form gets a file input and a `mimes` rule, images for a name such as `image` or `photo`, else images, PDF and office or text documents; never add `html`, `svg`, `xml` or `js`; the column holds the stored path); a trailing `?` makes a field nullable. Flags: `-m` migration, `-c` controller, `-r` resource controller with views and routes (all public: protect them before deploying), `-f` factory, `-s` seeder, `--all`. |
| `smeltery make:migration name` | A migration: `create_x_table`, `add_y_to_x_table`, or any other name. |
| `smeltery make:seeder Name` | A seeder. |
| `smeltery make:factory Name [--model Post]` | A factory for a model. |
| `smeltery make:command Name` | A console command. |
| `smeltery make:mail Name` | A mail class and its template in `resources/views/mail/`. |
| `smeltery make:middleware Name` | A middleware function; register it in `bootstrap/app.rs` with `.middleware("alias", f)`. |
| `smeltery make:controller Name [--resource [--model Post]]` | A controller with Mold views and route entries. |
| `smeltery make:spark Name` | A Spark (live component) and its view, registered in `app/sparks/mod.rs`. |
| `smeltery route:list` | Lists the app's routes. |

## Tests

Run `smeltery test` (or `cargo test`). HTTP tests build the app in memory:

```rust
let app = smeltery::testing::TestApp::new(my_app::build);
let res = app.get("/");
```
