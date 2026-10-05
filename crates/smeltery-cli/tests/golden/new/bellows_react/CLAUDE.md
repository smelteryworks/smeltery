# My App: guide for coding agents

This is a [Smeltery](https://github.com/smelteryworks/smeltery) application: a batteries-included Rust web
framework. One crate; every folder is a Rust module.

## Layout

| Path | What lives there |
|---|---|
| `bootstrap/main.rs` | The binary. Calls `smeltery::run` with the app's `build` function. |
| `bootstrap/app.rs` | The library root. Pulls in the folders below with `#[path]` and defines `build`, which registers config, migrations, seeders, commands and routes. |
| `app/` | Application code: `agents`, `commands`, `controllers`, `helpers`, `jobs`, `mail`, `middleware`, `models`, `providers`, `services`. `app/providers/alloy.rs` holds the root page and the props every page shares. |
| `config/` | Typed settings structs, read with `smeltery::config::env("KEY", default)`. |
| `database/` | `migrations`, `seeders`, `factories`, each with a `mod.rs` that registers them. The SQLite database file `database.sqlite` (not committed). |
| `routes/web.rs`, `routes/api.rs` | Route registration. API routes are registered with `api_routes`. |
| `resources/js/` | The React side (TypeScript): `resources/js/app.tsx` (the Vite entry), `pages/` (one file per page: `alloy::render("welcome")` shows `pages/welcome.tsx`), `layouts/`, `components/`, `types/`. |
| `resources/views/app.mold.html` | The root template: the HTML of a first visit, with `@vite` (the scripts and styles) and `@alloy` (the page). Mail templates live in `resources/views/mail/`. |
| `resources/css/app.css` | The stylesheet the entry imports: the theme colours and the component classes (`btn-primary`, `form-input`, `panel`, ...), built by Tailwind (`@tailwindcss/vite`) from the classes in `resources/js/`. |
| `package.json`, `vite.config.ts` | The npm packages (exact versions) and the Vite build: `npm run dev`, `npm run build` (into `public/build/`), `npm run types` (the type check). |
| `public/` | Files served as-is. `public/build/` holds the built JavaScript and CSS (`npm run build`; not committed). |
| `storage/` | Files the app writes: `app/public` and `app/private` (the app's own files), `framework` (Smeltery's working files: the file cache, file sessions, upload temp files), `logs`. Not committed. |
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
  password `password`) only when `APP_ENV` is `local` or `testing`. **Factories** (`database/factories/`)
  implement `Factory` and fill a model with `Fake` values: `UserFactory.create(&db)`,
  `UserFactory.count(3).create(&db)`.
- Tests: `TestApp::new(build)` runs every migration on a fresh database (in-memory SQLite, or
  `TEST_DATABASE_URL`); `app.db()` gives the handle for test data.

## Authentication, sessions and validation

- **Temper** is the authentication: `build` in `bootstrap/app.rs` calls `.temper(app::providers::temper::temper())`,
  which also registers `User` (`smeltery::auth::Authenticatable`) as the user model. `app/providers/temper.rs` turns
  the features on (registration, password reset, e-mail verification, profile and password updates, two-factor
  authentication), names the React page of each `GET` route (`alloy::render("…")`, pages in
  `resources/js/pages/auth/`) and overrides one answer: logging out calls `alloy::clear_history(ctx.session())`.
  `app/actions/temper/` holds the forms and what they do: `CreateNewUser`, `ResetUserPassword`,
  `UpdateUserPassword`, `UpdateUserProfileInformation`, and the password rules once, in `password_rules.rs`.
  Temper's routes: `/login`, `POST /logout`, `/register`, `/forgot-password`, `/reset-password/{token}`,
  `/email/verify…`, `/user/confirm-password`, `PUT /user/profile-information`, `PUT /user/password`,
  `/two-factor-challenge` and `/user/two-factor-…` (`smeltery route:list` lists them). The app's own: `/dashboard`
  and the settings pages `/settings/profile`, `/settings/password` and `/settings/two-factor`
  (`app/controllers/settings.rs`, `resources/js/pages/settings/`). The password reset link is mailed (see Mail).
  `bootstrap/app.rs` turns on `encrypt_history()`, so after logging out the back button cannot show a signed-in page
  (the browser encrypts its history only over HTTPS or on `127.0.0.1` / `localhost`). A feature left out of
  `temper()` has no routes; `.without_route("name")` leaves one route out, so the app declares its own there.
- Protect a route with `.middleware("auth")` (guests are sent to `/login`); `.middleware("guest")` keeps logged-in
  users out of the login and register pages; `.middleware("password.confirm")` asks for the password again
  (`/user/confirm-password`, then valid for `AUTH_PASSWORD_TIMEOUT` seconds), as `/settings/profile` and
  `/settings/two-factor` do. In a
  handler, `auth: smeltery::auth::Auth` gives `auth.check()`, `auth.id()`, `auth.user::<User>().await?` and
  `auth.logout()`. Passwords are stored with `smeltery::auth::hash_password` (argon2id), never in plain text.
  `smeltery::auth::end_credentials(&app, user_id).await?` signs a user out everywhere (every open session on its
  next request, and the remember-me cookies), through the `credentials_epoch` column of `users`.
- Temper's form routes carry `throttle:6,1` (6 requests a minute per client on that route, counted in the cache; a
  form then shows "Too many attempts") and `POST /login` and `POST /two-factor-challenge` carry `throttle:30,1`, on
  top of the login budgets. Use the same alias on other routes that need a limit
  (`throttle:<max>,<minutes>,<field>` names the field of the message). The forms read `email` trimmed and in lower
  case; a new form that takes an account's address does the same with
  `#[serde(deserialize_with = "smeltery::auth::deserialize_email")]`.
- Pages see the signed-in user as `usePage().props.auth.user`: the `SharedUser` allow-list in
  `app/providers/alloy.rs` (`id`, `name`, `email`, `email_verified`, `two_factor_enabled`), never the `User` model.
- **Two-factor authentication**: a user turns it on at `/settings/two-factor` (scan the QR code, confirm with a
  first code, keep the recovery codes); then logging in asks for a code from the authenticator app at
  `/two-factor-challenge`, or a recovery code (each works once). The page fetches the QR code and the key from
  `/user/two-factor-qr-code` and `/user/two-factor-secret-key` while enrolling (never as props, so they stay out of
  the browser's history) and shows the QR code as an `<img>`; the recovery codes arrive once as the
  `recovery_codes` prop. The secret is stored encrypted with a key derived from `APP_KEY`: after a new `APP_KEY`
  users enrol again (recovery codes still work). `smeltery temper:two-factor-disable <email> --force` turns it off
  for a user who lost the device. Tests use `smeltery::temper::testing` (`confirm_password`, `fake_clock`,
  `two_factor_code`, `enable_two_factor`).
- **Email verification** is scaffolded and off: `User` implements `smeltery::auth::MustVerifyEmail` (column
  `email_verified_at`), Temper serves `/email/verify` (the notice,
  `resources/js/pages/auth/verify-email.tsx`), `/email/verify/{id}/{hash}` (the signed link from
  the mail, valid for `AUTH_VERIFICATION_EXPIRE` minutes) and `POST /email/verification-notification` (send it
  again), and `/dashboard` has the `verified` middleware. Uncomment `.verify_email::<app::models::User>()` in
  `bootstrap/app.rs` to turn it on: registration then mails the link and `verified` sends unverified users to
  `/email/verify`. Until then `verified` lets every signed-in user through.
- **Sessions** (`session: smeltery::session::Session`): `get`, `insert`, `remove`, `flash("status", "…")` for the
  next request. `SESSION_DRIVER=database` (this app's `.env`: the `sessions` table of its migrations, so a
  session can be ended on the server), `cookie` (encrypted cookie) or `file`
  (`storage/framework/sessions/`); `SESSION_LIFETIME` in minutes. Sessions and cookies are encrypted with `APP_KEY`.
- **Flash**: every value flashed with a key that does not start with `_` reaches the next page as `usePage().flash`
  (the layout shows `flash.status` and `flash.error`). It is sent to the browser: never flash secrets.
- **CSRF**: nothing to add to forms. The web stack sets an `XSRF-TOKEN` cookie and Inertia's client sends it back as
  the `X-XSRF-TOKEN` header on every request; `/api` routes are exempt. An expired token sends the user back with
  `flash.error`.
- **Validation**: a form struct derives `Deserialize` and `smeltery::Validate`, with rules per field
  (`#[validate(required, email, max = 255, unique(table = "users", column = "email"))]`); the handler takes
  `Valid(form): Valid<TheForm>`. `useForm` posts JSON (multipart/form-data when a file is in it); on failure the
  page comes back with the messages in `form.errors.field` (the `errors` prop). A handler can fail a field itself
  with `Err(smeltery::Error::validation("email", "…"))`. Empty fields count as missing, so `Option` fields become
  `None`.

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

## Cache

- Take `cache: smeltery::cache::Cache` as a handler argument (or `app.cache()`; `ctx.cache()` in agents
  and jobs). Values are any `Serialize`/`Deserialize` type: `cache.get::<T>("key")`,
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

## Pages (React through Alloy)

- A controller returns a page: `alloy::render("welcome").with("stats", &stats)` (`use smeltery::alloy::{self,
  Page};`, the handler returns `Page` or `smeltery::Result<Page>`). The component name is the file under
  `resources/js/pages/` without its extension: `welcome` is `pages/welcome.tsx` (React pages are lowercase paths:
  `auth/login`). A typed alternative:
  `#[derive(serde::Serialize, smeltery::Alloy)] #[alloy("welcome")] pub struct Welcome { … }`, returned from the handler.
- The first visit gets HTML (`resources/views/app.mold.html` with the page inside); every later visit (a `<Link>`,
  `router.visit`, a form) gets the page as JSON. Rust edits restart the app; `resources/js/` edits are hot-reloaded
  by Vite.
- Prop kinds: `.with(key, value)` (every visit), `.with_lazy(key, || async { … })` (computed only when sent),
  `.optional(key, …)` (only when a partial reload asks: `router.reload({ only: ['key'] })`), `.defer(key, …)` (loaded
  by the client right after the page; `<Deferred data="key">`), `.merge` / `.prepend` / `.deep_merge` (combined
  with the client's data), `.always(key, value)` (also on partial reloads that do not name it). See
  `app/controllers/home.rs` (optional) and `app/controllers/dashboard.rs` (deferred).
- `app/providers/alloy.rs` shares props with every page (`app`, `auth.user`); they are typed in
  `resources/js/types/global.d.ts` and read with `usePage().props`.
- **Every prop is public**: it is in the page's HTML and JSON. Never pass a model with secrets (password hashes,
  tokens) or session values; send a struct with the fields the page needs.
- `alloy::location(url)` leaves the app (a full page load, for OAuth or downloads; never with user input);
  `Redirect::to` and `Back` work as usual. A link to a Mold page (the Watchfire dashboard) becomes a full page load.
- Tests: `use smeltery::alloy::testing::{AlloyAssertions as _, AlloyRequests as _, assert_page_file_exists};` then
  `app.get_alloy("/").assert_component("welcome").assert_prop("app.name", "My App")`,
  `app.reload_alloy("/", "welcome", &["forge"])` (a partial reload), `app.post_alloy("/posts", &json)` (a `useForm`
  post) and `assert_page_file_exists("welcome")`. `APP_ENV=testing` renders pages without the Vite assets, so
  `cargo test` needs no Node.js. See `tests/http.rs`.
- Generators write React pages: `smeltery make:page Name` (a page, its controller and route),
  `smeltery make:controller Name` and `smeltery make:model Post title:string --all` (a resource controller and the
  pages `posts/index`, `create`, `show`, `edit` with `useForm`).
- Sparks are not used for the app's pages. The app installs them for the Watchfire dashboard's live
  panels (`.sparks(|_| {})` in `bootstrap/app.rs`).

## AI-agent support (Bellows)

- `.mcp.json` registers the app's MCP server: `smeltery bellows:mcp` (it runs inside the app, over stdin / stdout).
  Its tools: `route_list` (routes), `models` (models with their columns), `db_schema` (tables, columns, indexes),
  `config_keys` (setting names, never values), `last_errors` (ERROR lines from the log file, `LOG_FILE`),
  `docs_search` (the Smeltery guide, this file and `.bellows/`), `run_generator` (`smeltery make:*`), `run_tests`
  (`cargo test` with a time limit), `agents_list` and `agent_control` (the running app's agents).
- `.bellows/guidelines.md` lists the conventions to follow.
- `.bellows/skills/` has step-by-step guides: `crud-resource.md`, `auth-route.md`, `alloy-page.md`, `migration.md`, `mail.md`, `agent.md`.
- `smeltery bellows:install [--mcp] [--skills] [--guidelines] [--all]` adds the parts that are missing; it never
  overwrites a file.

## Commands

| Command | What it does |
|---|---|
| `smeltery serve` | Builds and runs the app, restarting it when Rust code or `Cargo.toml` changes. Starts the Vite dev server (`node node_modules/vite/bin/vite.js`, on `127.0.0.1:5173`) next to it; run `npm install` first. |
| `smeltery test` | Runs `cargo test`; extra arguments are passed through. |
| `smeltery build` | Runs `npm run build` (after `npm ci` / `npm install` when `node_modules/` is missing) into `public/build/`, then builds the release binary. |
| `smeltery key:generate` | Writes a new `APP_KEY` into `.env` (`--show` prints it). With `APP_ENV=production` an existing key is kept unless `--force` is given. |
| `smeltery storage:link` | Links `public/storage` to `storage/app/public`. |
| `smeltery migrate` | Runs the pending migrations. `migrate:rollback [--step N]`, `migrate:fresh [--seed]` and `migrate:status` manage them. |
| `smeltery db:seed [--class Name]` | Runs the seeders. |
| `smeltery cache:clear [store]` | Removes every entry of the default cache store (or the named one). |
| `smeltery agents:list` | Lists the agents of the running app with their state. |
| `smeltery agents:start\|stop\|pause\|resume\|restart <name>` | Controls an agent of the running app. |
| `smeltery agents:logs <name>` | Shows an agent's recent log lines. |
| `smeltery agents:runs <name>` | Lists an agent's recent runs and their outcomes. |
| `smeltery schedule:list` | Lists the scheduled tasks and their next run. |
| `smeltery schedule:run` | Runs the scheduled tasks that are due now, once (for system cron). |
| `smeltery make:agent Name` | An agent, registered in `app/agents/mod.rs`. |
| `smeltery make:job Name` | A queued job, registered in `app/agents/mod.rs`. |
| `smeltery make:model Name [field:type ...]` | A model. Types: `string`, `text`, `integer`, `bigint`, `bool`, `float`, `date`, `datetime`, `json`, `uuid`, `<name>_id:foreign`, `file` (an upload: the form gets a file input and a `mimes` rule, images for a name such as `image` or `photo`, else images, PDF and office or text documents; never add `html`, `svg`, `xml` or `js`; the column holds the stored path); a trailing `?` makes a field nullable. Flags: `-m` migration, `-c` controller, `-r` resource controller with React pages and routes (`index` and `show` public, the routes that change records behind `auth`), `-f` factory, `-s` seeder, `--all`. |
| `smeltery make:migration name` | A migration: `create_x_table`, `add_y_to_x_table`, or any other name. |
| `smeltery make:seeder Name` | A seeder. |
| `smeltery make:factory Name [--model Post]` | A factory for a model. |
| `smeltery make:command Name` | A console command. |
| `smeltery make:mail Name` | A mail class and its template in `resources/views/mail/`. |
| `smeltery make:middleware Name` | A middleware function; register it in `bootstrap/app.rs` with `.middleware("alias", f)`. |
| `smeltery make:controller Name [--resource [--model Post]]` | A controller with React pages in `resources/js/pages/` and route entries. |
| `smeltery make:page Name` | A React page, its controller and its route. |
| `smeltery route:list` | Lists the app's routes. |

## Tests

Run `smeltery test` (or `cargo test`). HTTP tests build the app in memory:

```rust
let app = smeltery::testing::TestApp::new(my_app::build);
let res = app.get("/");
```
