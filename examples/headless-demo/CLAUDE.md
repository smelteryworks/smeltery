# Headless Demo: guide for coding agents

This is a [Smeltery](https://github.com/smelteryworks/smeltery) application: a batteries-included Rust web
framework. One crate; every folder is a Rust module.

## Layout

| Path | What lives there |
|---|---|
| `bootstrap/main.rs` | The binary. Calls `smeltery::run` with the app's `build` function. |
| `bootstrap/app.rs` | The library root. Pulls in the folders below with `#[path]` and defines `build`, which registers config, migrations, seeders, commands. |
| `app/` | Application code: `agents`, `commands`, `helpers`, `jobs`, `mail`, `middleware`, `models`, `providers`, `services`. |
| `config/` | Typed settings structs, read with `smeltery::config::env("KEY", default)`. |
| `database/` | `migrations`, `seeders`, `factories`, each with a `mod.rs` that registers them. The SQLite database file `database.sqlite` (not committed). |
| `public/` | Files served as-is. |
| `storage/` | Files the app writes: `app/public` and `app/private` (the app's own files), `framework` (Smeltery's working files), `logs`. Not committed. |
| `tests/` | Integration tests using `smeltery::testing::TestApp`. |
| `.env` | Local settings and secrets. Not committed; `.env.example` lists the keys. |

## Conventions

- One module per folder. Each folder has a `mod.rs` that lists its modules.
- Marker comments (`// smeltery:mods`, `// smeltery:models`, `// smeltery:migrations`, `// smeltery:seeders`,
  `// smeltery:commands`) show where generated lines go. New lines are added
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
  `create_watchfire_tables`). `smeltery work` runs the agents, the queue workers and the
  schedule.
- `smeltery work` serves the Watchfire API on `WATCHFIRE_API_ADDR` (`127.0.0.1:8001` in `.env`); the `agents:*`
  commands reach the running worker there, authenticated with a token derived from `APP_KEY`.
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
  The sender is `MAIL_FROM_ADDRESS` / `MAIL_FROM_NAME`. `WATCHFIRE_ALERT_MAIL` receives agent alerts by mail.
- Tests run with `APP_ENV=testing`, where mail goes to a fake mailbox:
  `Mailer::of(app.app()).unwrap().mailbox().unwrap().assert_sent::<Welcome>(|mail, email| …)`.

## Console commands

`app/commands/<name>.rs` implement `smeltery::console::Command` (`name`, `about`, `async fn run`) and are registered in
`app/commands/mod.rs`. Run them with `smeltery <name>`; `smeltery help` lists every command.

## AI-agent support (Bellows)

- `smeltery bellows:install --mcp` adds `.mcp.json`, which registers the app's MCP server (`smeltery bellows:mcp`:
  routes, models, schema, config key names, errors, docs, generators and tests for coding agents).
- `smeltery bellows:install [--mcp] [--skills] [--guidelines] [--all]` adds the parts that are missing; it never
  overwrites a file.

## Commands

| Command | What it does |
|---|---|
| `smeltery serve` | Builds and runs the app (`work`), restarting it when Rust code or `Cargo.toml` changes. |
| `smeltery test` | Runs `cargo test`; extra arguments are passed through. |
| `smeltery build` | Builds the release binary. |
| `smeltery key:generate` | Writes a new `APP_KEY` into `.env`. |
| `smeltery storage:link` | Links `public/storage` to `storage/app/public`. |
| `smeltery migrate` | Runs the pending migrations. `migrate:rollback [--step N]`, `migrate:fresh [--seed]` and `migrate:status` manage them. |
| `smeltery db:seed [--class Name]` | Runs the seeders. |
| `smeltery work` | Runs the agents, the job queue workers and the schedule until Ctrl-C. |
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

## Tests

Run `smeltery test` (or `cargo test`). HTTP tests build the app in memory:

```rust
let app = smeltery::testing::TestApp::new(headless_demo::build);
let res = app.get("/");
```
