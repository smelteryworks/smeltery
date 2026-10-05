# Web Demo: guide for coding agents

This is a [Smeltery](https://github.com/smelteryworks/smeltery) application: a batteries-included Rust web
framework. One crate; every folder is a Rust module.

## Layout

| Path | What lives there |
|---|---|
| `bootstrap/main.rs` | The binary. Calls `smeltery::run` with the app's `build` function. |
| `bootstrap/app.rs` | The library root. Pulls in the folders below with `#[path]` and defines `build`, which registers config, migrations, seeders, commands and routes. |
| `app/` | Application code: `agents`, `commands`, `controllers`, `helpers`, `jobs`, `mail`, `middleware`, `models`, `providers`, `services`, `sparks`. |
| `config/` | Typed settings structs, read with `smeltery::config::env("KEY", default)`. |
| `database/` | `migrations`, `seeders`, `factories`, each with a `mod.rs` that registers them. The SQLite database file `database.sqlite` (not committed). |
| `routes/web.rs`, `routes/api.rs` | Route registration. API routes are registered with `api_routes`. |
| `resources/views/` | Mold templates (`*.mold.html`): pages, `layouts/`, `components/` (`<x-…>` Mold components), `sparks/` (Spark views). |
| `resources/css/app.css` | The Tailwind entry point. |
| `public/` | Files served as-is. |
| `storage/` | Files the app writes: `app/public` and `app/private` (the app's own files), `framework` (Smeltery's working files: file sessions, upload temp files), `logs`. Not committed. |
| `tests/` | Integration tests using `smeltery::testing::TestApp`. |
| `.env` | Local settings and secrets. Not committed; `.env.example` lists the keys. |

## Conventions

- One module per folder. Each folder has a `mod.rs` that lists its modules.
- Marker comments (`// smeltery:mods`, `// smeltery:models`, `// smeltery:migrations`, `// smeltery:seeders`,
  `// smeltery:commands`, `// smeltery:routes`) show where generated lines go. New lines are added
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
- **Seeders** (`database/seeders/`) implement `Seeder`; `DatabaseSeeder` creates a demo user (`demo@example.com`,
  password `password`). **Factories** (`database/factories/`) implement `Factory` and fill a model with `Fake`
  values: `UserFactory.create(&db)`, `UserFactory.count(3).create(&db)`.
- Tests: `TestApp::new(build)` runs every migration on a fresh database (in-memory SQLite, or
  `TEST_DATABASE_URL`); `app.db()` gives the handle for test data.

## Authentication, sessions and validation

- **Auth** is wired in `bootstrap/app.rs` with `.auth::<app::models::User>()`; `User` implements
  `smeltery::auth::Authenticatable`. Pages: `/register`, `/login`, `POST /logout`, `/forgot-password`,
  `/reset-password/{token}` (controllers in `app/controllers/auth/`, views in `resources/views/auth/`) and
  `/dashboard`. The password reset link is mailed (see Mail).
- Protect a route with `.middleware("auth")` (guests are sent to `/login`); `.middleware("guest")` keeps logged-in
  users out of the login and register pages. In a handler, `auth: smeltery::auth::Auth` gives `auth.check()`,
  `auth.id()`, `auth.user::<User>().await?`, `auth.attempt(email, password, remember)`, `auth.login(&user, remember)`
  and `auth.logout()`. Passwords are stored with `smeltery::auth::hash_password` (argon2id), never in plain text.
- **Sessions** (`session: smeltery::session::Session`): `get`, `insert`, `remove`, `flash("status", "…")` for the
  next request. `SESSION_DRIVER=cookie` (default, encrypted cookie), `database` (the `sessions` table) or `file`
  (`storage/framework/sessions/`); `SESSION_LIFETIME` in minutes. Sessions and cookies are encrypted with `APP_KEY`.
- **CSRF**: every `POST`/`PUT`/`PATCH`/`DELETE` web form needs `@csrf` inside the `<form>`; `/api` routes are exempt.
  Use `@method("PUT")` / `@method("DELETE")` for those verbs in HTML forms.
- **Validation**: a form struct derives `Deserialize` and `smeltery::Validate`, with rules per field
  (`#[validate(required, email, max = 255, unique(table = "users", column = "email"))]`); the handler takes
  `Valid(form): Valid<TheForm>`. On failure the browser is sent back with the errors and old input, shown in Mold
  with `@error("field") {{ message }} @enderror` and `{{ old("field") }}`; JSON requests get a 422 with the errors.
  Empty fields count as missing, so `Option` fields become `None`.
- Templates show flash messages with `session("status")` (the layout already does for `status` and `error`) and
  login state with `@auth … @endauth` / `@guest … @endguest`.

## Agents, jobs and the schedule (Watchfire)

- `app/agents/mod.rs` registers everything in `register(w: &mut Watchfire)` (`use smeltery::watchfire::prelude::*;`).
  `bootstrap/app.rs` wires it with `.agents(app::agents::register)`.
- **Agents** (`app/agents/<name>.rs`, `smeltery make:agent Name`) implement `Agent`: `name`, `config`
  (`AgentConfig::default().restart(Restart::OnFailure).backoff(1.secs()..=60.secs()).heartbeat_timeout(2.mins())`)
  and `async fn run(&mut self, ctx: AgentCtx)`. Loop with `let mut ticker = ctx.interval(30.secs()); while
  ticker.tick().await { … }`: `tick()` sends the heartbeat and returns false once the agent is stopped. Register with
  `w.agent(name::Name::default());`. Closures work too: `w.every(30.secs(), "name", |ctx| async move { Ok(()) })`.
- **Jobs** (`app/jobs/<name>.rs`, `smeltery make:job Name`) are `Serialize + Deserialize` structs implementing
  `Job` (`const NAME`, `async fn handle(&self, ctx: JobCtx)`), registered with `w.job::<…>()` in
  `app/agents/mod.rs` and queued with `MyJob { … }.dispatch(&app).await?` (or `dispatch_later(&app, 10.mins())`).
  Failed jobs are retried with backoff (`max_attempts`, default 3), then moved to the dead letters.
- **Schedule**: `w.schedule().job(…).daily_at("03:00")`, `.call("name", |ctx| async move { … }).every(5.mins())`,
  `.cron("0 3 * * *")`. Times are UTC.
- In an agent, `ctx.db()`, `ctx.http()` (rate-limited client with timeouts and retries), `ctx.checkpoint(&state)` /
  `ctx.checkpoint_get()` (state that survives restarts), `ctx.log()`.
- Agent runs, checkpoints and the job queue live in the `watchfire_*` tables (migration
  `create_watchfire_tables`). `smeltery serve` runs the web server and the agents together.
- The dashboard at `/_watchfire` shows every agent (state, heartbeat, restarts, recent runs) with start / stop /
  pause / resume / restart buttons. Under `APP_ENV=local` it is open to requests from this machine; otherwise only
  to signed-in users that `w.dashboard_gate(|auth, app| async move { Ok(…) })` in `app/agents/mod.rs` admits
  (nobody without a gate). `WATCHFIRE_DASHBOARD=auth` needs the gate locally too; `off` hides it.
  `smeltery serve --no-agents` (or `WATCHFIRE_IN_SERVE=false`) serves the web only, next to a `work` process; its
  dashboard then shows and controls the agents running in `work` (through the database).
- `WATCHFIRE_ALERT_WEBHOOK` receives a JSON POST when an agent fails for good or stalls.
- LLM helpers (a provider trait, a fake provider for tests, an Anthropic client, a tool-calling agent loop with a
  token budget) come with the `llm` feature of `smeltery`.

## Mail

- A mail is a struct in `app/mail/<name>.rs` with `#[derive(Mold)]` and `#[mold("mail/<name>")]` (its HTML body is
  `resources/views/mail/<name>.mold.html`; the fields are its variables) implementing `Mailable`
  (`fn envelope(&self) -> Envelope { Envelope::new().to(…).subject(…) }`). `smeltery make:mail Name` creates both.
- Send with a `mailer: smeltery::mail::Mailer` handler argument (or `Mailer::of(&app)?`):
  `mailer.send(Welcome { … }).await?`; `mailer.queue(…).await?` sends it from the Watchfire queue.
  `bootstrap/app.rs` installs mail with `.mail()`.
- `MAIL_MAILER` picks the transport: `log` (default: the mail is written to the log), `smtp` (`MAIL_HOST`,
  `MAIL_PORT`, `MAIL_USERNAME`, `MAIL_PASSWORD`, `MAIL_ENCRYPTION=starttls|tls|none`, `MAIL_TIMEOUT`) or `fake`.
  The sender is `MAIL_FROM_ADDRESS` / `MAIL_FROM_NAME`. Password reset links are mailed as
  `smeltery::mail::ResetPassword`. `WATCHFIRE_ALERT_MAIL` receives agent alerts by mail.
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
  `.blur`, `.change`), `wire:keydown.enter="save"`, `wire:loading` (`.remove`, `.class="…"`, `.attr="disabled"`),
  `wire:target="save"`, `wire:poll.5s="action"`, `wire:key="…"`, `wire:click="$refresh"`; `@error("field")` shows
  validation messages. A value from data in an action's arguments goes through `json`:
  `wire:click="remove({{ item.id | json }})"`.
- Tests: `smeltery::sparks::testing::TestSpark::from_html(&app.get("/").text(), "counter")`, then
  `.set("step", 5)`, `.call("increment", smeltery::json!([])).send(&app)`, and check `.data()` / `.html()`.

## AI-agent support (Bellows)

- `.mcp.json` registers the app's MCP server: `smeltery bellows:mcp` (it runs inside the app, over stdin / stdout).
  Its tools: `route_list` (routes), `models` (models with their columns), `db_schema` (tables, columns, indexes),
  `config_keys` (setting names, never values), `last_errors` (ERROR lines from the log file, `LOG_FILE`),
  `docs_search` (the Smeltery guide, this file and `.bellows/`), `run_generator` (`smeltery make:*`), `run_tests`
  (`cargo test` with a time limit), `agents_list` and `agent_control` (the running app's agents).
- `.bellows/guidelines.md` lists the conventions to follow.
- `.bellows/skills/` has step-by-step guides: `crud-resource.md`, `auth-route.md`, `spark.md`, `migration.md`, `mail.md`, `agent.md`.
- `smeltery bellows:install [--mcp] [--skills] [--guidelines] [--all]` adds the parts that are missing; it never
  overwrites a file.

## Commands

| Command | What it does |
|---|---|
| `smeltery serve` | Builds and runs the app, restarting it when Rust code or `Cargo.toml` changes. |
| `smeltery test` | Runs `cargo test`; extra arguments are passed through. |
| `smeltery build` | Builds the release binary. |
| `smeltery key:generate` | Writes a new `APP_KEY` into `.env`. |
| `smeltery storage:link` | Links `public/storage` to `storage/app/public`. |
| `smeltery migrate` | Runs the pending migrations. `migrate:rollback [--step N]`, `migrate:fresh [--seed]` and `migrate:status` manage them. |
| `smeltery db:seed [--class Name]` | Runs the seeders. |
| `smeltery agents:list` | Lists the agents of the running app with their state. |
| `smeltery agents:start\|stop\|pause\|resume\|restart <name>` | Controls an agent of the running app. |
| `smeltery agents:logs <name>` | Shows an agent's recent log lines. |
| `smeltery agents:runs <name>` | Lists an agent's recent runs and their outcomes. |
| `smeltery schedule:list` | Lists the scheduled tasks and their next run. |
| `smeltery schedule:run` | Runs the scheduled tasks that are due now, once (for system cron). |
| `smeltery make:agent Name` | An agent, registered in `app/agents/mod.rs`. |
| `smeltery make:job Name` | A queued job, registered in `app/agents/mod.rs`. |
| `smeltery make:model Name [field:type ...]` | A model. Types: `string`, `text`, `integer`, `bigint`, `bool`, `float`, `date`, `datetime`, `json`, `uuid`, `<name>_id:foreign`; a trailing `?` makes a field nullable. Flags: `-m` migration, `-c` controller, `-r` resource controller with views and routes, `-f` factory, `-s` seeder, `--all`. |
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
let app = smeltery::testing::TestApp::new(web_demo::build);
let res = app.get("/");
```
