# Changelog

All notable changes to `smeltery-cli` are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- `smeltery new <name>`: generates an app from questions on a terminal or from flags, and prints the equivalent
  one-line command at the end. The questions come in the order kind, database, starter kit, Tailwind, Alpine.js,
  building blocks, Bellows, migrations, seeders, git (and npm for React and Vue); the summary and the one-line
  command follow it (`… --frontend mold --tailwind --no-alpine --smelt watchfire,temper --bellows none …`). The
  flags: `--kind`, `--db`, `--frontend mold|react|vue`, `--tailwind` / `--no-tailwind`, `--alpine` / `--no-alpine`,
  `--smelt`, `--bellows`, `--npm` / `--no-npm`, `--migrate` / `--no-migrate`, `--seed` / `--no-seed`,
  `--git` / `--no-git`, `--path` and `--smeltery-path` (a relative path stays relative in the app's
  `Cargo.toml`, seen from the app's directory, e.g. `../../crates/smeltery`, so an app kept inside the checkout
  builds wherever it is cloned). Every app gets its layout, `.env` with a fresh `APP_KEY`, `.env.example`,
  `CLAUDE.md` / `AGENTS.md` and the chosen Bellows files. `--migrate` / `--seed` run `migrate` and `db:seed` in the
  new app; a failure prints the command to run by hand.
- "What are you building?" has two answers: a web app (`--kind web`, the default) or a headless app
  (`--kind headless`: Watchfire agents, jobs and the scheduler, no web pages; it skips the starter kit, Tailwind,
  Alpine.js and building-block questions, and refuses `--smelt` with a building block; `--smelt none` is accepted).
- "Which building blocks should we smelt into your stack?" (web apps): a multi-select with Watchfire (agents, jobs,
  the scheduler and the `/_watchfire` dashboard) and Temper ("login, registration, password reset, email
  verification, two-factor"), both selected by default, and Hallmark (API tokens), Anvil (WebSockets and
  broadcasting) and Prospect ("full-text search for models"), off by default. The flag is
  `--smelt watchfire,temper,hallmark,anvil,prospect` (any of the blocks, comma-separated, or `none`); the summary
  has a `smelt` row and the one-line command prints `--smelt`. One table declares each block's texts, the blocks it
  requires (chosen with it, with a note on screen, also with `--smelt`: "Hallmark needs Temper: added") and the
  blocks it works well with (a hint under the checklist: Anvil hints at Temper, Hallmark and Watchfire when they are
  not chosen); the checklist, `--smelt`, the summary and the one-line command read it.
- Starter kits ("Starter kit": Mold + Sparks, React (Inertia, TypeScript), Vue (Inertia, TypeScript);
  `--frontend mold|react|vue`). React and Vue apps' controllers return `smeltery::alloy` pages; the kit has
  `package.json` with exact versions, `tsconfig.json`, `vite.config.ts` (a `smeltery()` plugin: build into
  `public/build/` with a manifest, dev server on `127.0.0.1:5173`, the `storage/framework/vite.hot` marker), the root
  template `resources/views/app.mold.html`, `app/providers/alloy.rs` (the root page, `SharedUser`, shared props), the
  welcome page with an optional-prop partial reload ("Ask the forge") that suggests `make:model Post title:string
  --all`, the dashboard with a deferred card, `useForm` authentication pages including email verification (none
  without the `temper` block), history encryption with `clear_history` on logout, and tests through
  `smeltery::alloy::testing`. Alpine.js is not offered for them. `.env` gets `VITE_APP_NAME` (every `VITE_` value is
  public), `.gitignore` `node_modules` and `public/build`, `Cargo.toml` `[package.metadata.smeltery] frontend`. With
  Tailwind the kit uses `tailwindcss` / `@tailwindcss/vite` from npm (no binary download); without it a prebuilt
  stylesheet of the kit's pages.
- "Install the npm packages now?" for React and Vue when Node.js 20.19+ or 22.12+ and npm are found
  (`--npm` / `--no-npm`, default yes) runs `npm install --no-audit --no-fund` behind the spinner; a missing Node.js
  or a failed install is a warning and `npm install` joins the next steps. The install stops after 5 minutes
  (`SMELTERY_NPM_TIMEOUT` seconds) and reports `failed (timed out)`; the app is complete. `SMELTERY_NODE` /
  `SMELTERY_NPM` override the programs.
- "Install Tailwind?" for web apps (`--tailwind` / `--no-tailwind`, default yes) downloads it (about 110 MB); a
  failed download is a warning, the summary shows `tailwind failed` and the retry command. New web apps ship a
  compiled `public/assets/css/app.css` (built with the pinned Tailwind from the view templates and the generators'
  views), so their pages are styled without Tailwind installed. `resources/css/app.css` limits Tailwind to
  `resources/views/` and `app/` (`source(none)` and `@source`), so text in the app's docs adds no CSS.
- "Alpine.js?" for the Mold kit (`--alpine` / `--no-alpine`, default no). With yes, the app gets Alpine.js 3.17.4
  (the npm package's `dist/cdn.min.js` with its MIT licence header, embedded in the CLI, no download) as
  `public/assets/js/alpine.min.js`; the layout loads it with `defer` after `@sparksScripts`, the welcome page has an
  Alpine toggle card and the counter Spark reads its count through `$spark`.
- `smeltery new` on a terminal is styled: a SMELTERY block-letter banner, a section per question, themed prompts
  (`›` cursor, `●` / `○` checkboxes), `✓` progress lines, a spinner while `migrate` and `db:seed` run, a summary
  box, and coloured `INFO` / `WARN` / `DONE` / `ERROR` labels. `--no-color` on every command gives plain output,
  also chosen by `NO_COLOR` or a stdout that is not a terminal.
- New apps (every kind) have a `users` migration (with `email_verified_at`, `password`, `remember_token` and the
  nullable `credentials_epoch` column), the `User` model (`Authenticatable`, returning `credentials_epoch` so
  `smeltery::auth::end_credentials` signs a user out everywhere; `MustVerifyEmail`), `DatabaseSeeder` (one demo user,
  `demo@example.com` / `password`, verified), `UserFactory`, `app/commands` with `register`, the `sessions` and
  `create_cache_tables` migrations, and `bootstrap/app.rs` registers migrations, seeders and commands. `.env` /
  `.env.example` set `SESSION_DRIVER` / `SESSION_LIFETIME`, `CACHE_STORE=database`, `CACHE_PREFIX=`,
  `LOG_FILE=storage/logs/smeltery.log` and the `MAIL_*` keys (`MAIL_MAILER=log`); every app installs mail
  (`.mail()`) and calls `.bellows()` (the `bellows:mcp` command). `CLAUDE.md` / `AGENTS.md` have Mail and Cache
  sections (with `cache:clear`), say `last_errors` reads `LOG_FILE` and describe the installed Bellows parts and the
  MCP tools.
- New web apps render the home page with Mold (`resources/views/layouts/app.mold.html`, `home.mold.html`, the
  `components/card.mold.html` component, a `#[derive(Mold)]` `HomePage` in `app/controllers/home.rs`), wire Sparks
  (`.sparks(app::sparks::register)`, `@sparksScripts` in the layout) and show a `Counter` Spark on the home page with
  a generated test driving it through `TestSpark`. The counter's "Saving…" label keeps its space while hidden
  (`wire:loading.class.remove="invisible"`, `role="status"`, on the button row). The welcome page is designed: a hero
  with the app name, the Smeltery version, next-step cards, docs links, a CSS-only ember visual, the counter Spark and
  a Watchfire card in apps with Watchfire, with a matching layout, auth pages, dashboard and card component, in light
  and dark; the theme colours and component classes (`btn-primary`, `form-input`, `panel`, ...) are in
  `resources/css/app.css`, and the `make:*` views use the same classes. Generated views (`smeltery new` and the
  `make:*` generators) are indented two spaces per level of nested HTML and inside Mold blocks. The generated
  `CLAUDE.md` / `AGENTS.md` describe Mold views and Sparks.
- Authentication is the Temper building block (`--smelt temper`, on by default, every starter kit): `bootstrap/app.rs`
  calls `.temper(app::providers::temper::temper())`; Temper serves the login (with "remember me"), logout,
  registration, password reset, e-mail verification, password confirmation, profile / password update and
  two-factor routes (no `app/controllers/auth/` in the app; `smeltery route:list` lists them).
  `app/providers/temper.rs` turns the features on and names the pages (Mold views in `resources/views/auth/`; React
  and Vue pages through `alloy::render`, with a logout answer that clears the browser's history state);
  `app/actions/temper/` holds the forms (`CreateNewUser`, `ResetUserPassword`, `UpdateUserPassword`,
  `UpdateUserProfileInformation`, the password rules in `password_rules.rs`). The app has `/dashboard` behind `auth`
  and `verified`, the settings pages `/settings/profile`, `/settings/password` and `/settings/two-factor`
  (`app/controllers/settings.rs`, `resources/views/settings/`; profile and two-factor behind `password.confirm`, as
  the profile form changes the address reset links go to; QR code, key, confirmation form, recovery codes shown once,
  `Cache-Control: no-store`; the React and Vue page fetches the QR code and the key from Temper's JSON routes, shows
  the QR code as an `<img>` and drops the recovery codes from its props once shown), a "Settings" link in the
  layout, a layout nav with `@auth` / `@guest` and a logout form, flash messages, the password confirmation and
  two-factor challenge pages (code or recovery code, no JavaScript); in React and Vue apps
  `SharedUser.two_factor_enabled` and the pages `auth/confirm-password`, `auth/two-factor-challenge`,
  `settings/{profile,password,two-factor}` with a settings layout (Vue: `auth/ConfirmPassword`,
  `auth/TwoFactorChallenge`, `settings/{Profile,Password,TwoFactor}`).
  The app gets the `password_reset_tokens` and `add_two_factor_columns_to_users_table` migrations, the four
  `two_factor_*` fields and `impl TwoFactorAuthenticatable` on `User`, `AUTH_PASSWORD_TIMEOUT=10800` in `.env`, and a
  signed-in visitor is sent to the page they first asked for (`auth.intended(…)`), else the dashboard. Tests cover
  registration, login, a wrong password, logout, forgot / reset password (the `ResetPassword` mail), the intended
  page (also for a verification link opened while logged out), `users_can_update_their_profile` (confirming the
  password first), `users_can_change_their_password_and_stay_signed_in`,
  `two_factor_can_be_enabled_confirmed_and_used_to_log_in`, `a_recovery_code_logs_in_once`,
  `settings_need_a_recent_password_confirmation` and `ending_a_users_credentials_signs_them_out_everywhere`.
  `CLAUDE.md` / `AGENTS.md`, `README.md`, `.bellows/guidelines.md` and the `auth-route` skill describe it;
  `bellows:install` finds it by `app/providers/temper.rs`, the resource generators by `.temper(` in
  `bootstrap/app.rs`. Without the block the app has no login, registration, password reset or dashboard pages,
  routes or controllers, no `password_reset_tokens` migration and no demo user in the seeder; its `CLAUDE.md` and
  `README.md` describe how to turn authentication on.
- Email verification in apps with Temper is scaffolded and off: `email_verified_at` and `MustVerifyEmail` on `User`,
  the notice, signed-link and "send it again" routes with the `verify-email` page, registration sending the link,
  `.middleware("verified")` on `/dashboard`, `AUTH_VERIFICATION_EXPIRE` in `.env`, a verified demo user and factory,
  and tests for both states. Uncommenting `.verify_email::<app::models::User>()` in `bootstrap/app.rs` turns it on.
- Watchfire apps (headless apps, and web apps with the `watchfire` block) register Watchfire
  (`.agents(app::agents::register)`), have the `create_watchfire_tables` and `create_pubsub_messages_table`
  migrations (the same file `smeltery pubsub:install` writes), and `WATCHFIRE_DASHBOARD`, `WATCHFIRE_API_ADDR`
  (`127.0.0.1:8001` in headless apps), `WATCHFIRE_ALERT_WEBHOOK`, `WATCHFIRE_ALERT_MAIL` and `QUEUE_DRIVER=database`
  (next to `CACHE_STORE`, with a comment naming the drivers `database`, `redis`, `memory` and a commented `REDIS_URL`
  line) in `.env` / `.env.example`. The generated `CLAUDE.md` / `AGENTS.md` and the headless README list the
  `agents:*` and `schedule:*` commands, the `/_watchfire` dashboard and its access (open under `APP_ENV=local` to
  requests from the machine itself, otherwise signed-in users admitted by `w.dashboard_gate`), `--no-agents` (the
  web process's dashboard then shows and controls the agents running in `work`), the `llm` feature, and describe
  agents, jobs and the schedule; headless apps document `smeltery work`, and their `CLAUDE.md` lists `smeltery build`
  without CSS and no `storage:link`.
- `--smelt hallmark` (API tokens; requires Temper): `.hallmark(Hallmark::new())`, the
  `create_personal_access_tokens_table` migration, `POST /api/tokens` (`throttle:10,1`, through
  `issue_for_credentials`), `DELETE /api/tokens/current` and `GET /api/user` (`auth:hallmark`) with their
  controllers in `app/controllers/api/`, commented `HALLMARK_*` lines in `.env`, `tests/api_tokens.rs` (its own
  users from `UserFactory`; issuing, a wrong password, 401s, signing out, abilities) and, with Watchfire, a daily
  `hallmark-prune` schedule. `smeltery hallmark:install` adds the same files to an existing app with
  authentication, registers them at the markers, and prints the two `bootstrap/app.rs` lines and the optional `.env`
  lines; it refuses without authentication and when the migration or a file exists.
- `--smelt anvil` (WebSockets and broadcasting): `.anvil(routes::channels::channels)`, `routes/channels.rs` (the
  public channel `announcements`; with Temper the private `users.{user}`, joined only by that user), the example
  events `app/events/announcement_posted.rs` and `app/events/user_notified.rs`, the `pubsub_messages` migration
  (also without Watchfire), `tests/broadcasting.rs` and commented `ANVIL_*` lines in `.env` (no secrets: the key and
  secret are derived from `APP_KEY`; `.env` explains the key, the path in `route:list`, that any public string works
  when fixed, that sockets use `SERVER_PORT`, and has commented `ANVIL_IN_SERVE`, `ANVIL_SERVER_HOST` and
  `ANVIL_SERVER_PORT` lines for a separate socket process). With Hallmark or Anvil, `.env` has commented
  `CORS_ALLOWED_ORIGINS` / `CORS_PATHS` lines (also in `hallmark:install`'s output). The block wires a client in every
  starter kit:
  - React and Vue: `package.json` pins `laravel-echo` 2.5.0, `@laravel/echo-react` / `@laravel/echo-vue` 2.5.0 and
    `pusher-js` 8.6.0. `resources/js/echo.ts` connects Echo to the page's host and port with the shared prop
    `app.anvil_key`, and authorizes channels at `/broadcasting/auth` with the session and the `XSRF-TOKEN` cookie as
    `X-XSRF-TOKEN`; Inertia requests carry `X-Socket-ID` (`http.onRequest`); the entry calls it through `withApp`.
    The welcome page has an announcements card. With Temper, the dashboard's "Live" card shows the user's private
    channel, a "Notify me" button (`POST /notify-me`, `throttle:10,1`, `app/controllers/notifications.rs`) and how
    many people are online with the viewer's own name (`presence-dashboard` shares member ids only; plus the
    `create_presence_tables` migration).
  - Mold: the `announcements` Spark on the home page and, with Temper, the `notifications` Spark on the dashboard
    listen to broadcasts without JavaScript (`#[on(…)]`); the latter's `ping` action sends the event, at most ten
    times a minute per user (a `RateLimiter` in the app's cache).
  - All kits: generated tests for presence, "Notify me" and the Spark. The generated `CLAUDE.md` / `AGENTS.md`,
    `README.md`, `.bellows/guidelines.md` and the skills `api-token.md` and `broadcast.md` describe Hallmark and
    Anvil; in React and Vue apps `CLAUDE.md` shows how to add the `laravel-echo` and `pusher-js` client.
- `--smelt prospect` (full-text search through Prospect): `.prospect(app::providers::search::register)`,
  `app/providers/search.rs` (models registered above `// smeltery:searchables`), `PROSPECT_DRIVER=database` in
  `.env`, a "Search" section in `CLAUDE.md` / `AGENTS.md`, a guideline and the skill `search.md`.
  `smeltery prospect:install` adds the provider to an existing web app and prints the `bootstrap/app.rs` and `.env`
  lines.
- `smeltery pubsub:install`: adds the migration of the `pubsub_messages` table (PubSub's `database` driver) and
  registers it.
- `smeltery bellows:install [--mcp] [--skills] [--guidelines] [--all]`: adds the Bellows files to an app, keeps
  existing files, and adds the `smeltery` server to an existing `.mcp.json`; it reads the starter kit from
  `Cargo.toml`. `--bellows skills` writes `.bellows/skills/` (`crud-resource`, `auth-route`, `spark`, `migration`,
  `agent`, `mail`, by app kind; React and Vue apps get `alloy-page` instead of `spark`), `--bellows guidelines` the
  expanded `.bellows/guidelines.md`. The `CLAUDE.md`, `AGENTS.md`, `.bellows/guidelines.md` and skills of React and
  Vue apps describe the kit (Alloy pages, props, `useForm`, partial reloads, deferred props, the test helpers). The
  `CLAUDE.md` / `AGENTS.md` and the `spark` skill of Mold apps document the `json` and `url` filters for values in
  JavaScript and links (`x-data`, `x-on:*`, `wire:click` arguments, `<script>`), `redirect_away`,
  `#[spark(model(fields = …))]`, re-checking permissions in Spark actions, and the Spark request limits.
- `smeltery serve`: builds and runs the app (`cargo build`, then the binary), restarts it on `.rs` / `Cargo.toml`
  changes, runs `tailwindcss --watch=always` with a null stdin when Tailwind is available (so the CSS is rebuilt
  also from an IDE task or a service), and runs `work` in an app without `routes/`. In a React / Vue app it starts
  the Vite dev server (`node node_modules/vite/bin/vite.js`) instead of the Tailwind watcher and deletes
  `storage/framework/vite.hot` when it stops. It stops on Ctrl-C, and on Unix also on SIGTERM and SIGHUP (a
  supervisor or a closing terminal); the app then gets SIGTERM and up to 10 seconds to shut down before it is
  killed, also before a restart. `smeltery serve --no-agents` runs the app's `serve --no-agents` (the web only;
  refused for a headless app).
- `smeltery build`: builds `public/assets/css/app.css` with `tailwindcss -i resources/css/app.css -o
  public/assets/css/app.css --minify` before `cargo build --release` when Tailwind is installed (found as for
  `serve`). Without it, a warning says how to install it and whether `app.css` is missing or left unchanged, and the
  build goes on; a failing Tailwind fails the build with its output. In a React / Vue app it runs `npm ci` /
  `npm install` when `node_modules/` is missing and `npm run build` before `cargo build --release`.
- `smeltery tailwind:install`: downloads the pinned Tailwind CSS standalone binary (v4.3.3) for this platform over
  HTTPS, checks its SHA-256 and keeps it in a per-user folder (`%LOCALAPPDATA%\smeltery\bin`,
  `~/Library/Application Support/smeltery/bin`, `$XDG_DATA_HOME/smeltery/bin` or `~/.local/share/smeltery/bin`); a
  verified binary already there is kept. `serve` and `build` look for Tailwind in `TAILWIND_BIN`, then there, then on
  `PATH`. Web apps' docs name `tailwind:install`.
- `smeltery test`, `smeltery key:generate [--show] [--force]` and `smeltery storage:link`; the last two run the code
  of `smeltery_core::console::setup`, shared with the app binary's own commands (`smeltery-cli` depends on
  `smeltery-core`). `key:generate` (without `--show`) and `storage:link` refuse outside a Smeltery app instead of
  creating `.env`, `public/` or `storage/` there; `.env` is written atomically (see `smeltery-core`). A second
  `storage:link` reports the existing link and exits with 0. The generated README lists the binary's setup commands
  (`key:generate`, `storage:link`, `migrate --force`). Every other command is forwarded to the app with
  `cargo run --quiet --`.
- The generators: `make:model` (fields `name:type[?]`, flags `-m -c -r -f -s --all`; without fields it writes a
  warning-free factory), `make:controller` (plain or `--resource [--model]` with Mold views and a `r.resource(...)`
  route entry), `make:migration`, `make:seeder`, `make:factory`, `make:command`, `make:middleware`,
  `make:mail Name` (a mail class in `app/mail/`, `#[derive(Mold)]` + `Mailable`, and its HTML template in
  `resources/views/mail/` with inline styles), `make:spark Name` (a Spark in `app/sparks/`, its view in
  `resources/views/sparks/`, registered at `// smeltery:sparks`; `--listen <channel> --event <Event>` makes one that
  listens to broadcasts in Mold apps with Anvil), `make:agent` (a supervised Watchfire agent with restart policy,
  backoff, heartbeat timeout and a tick loop) and `make:job` (a queued job), both registered at `// smeltery:agents`
  in `app/agents/mod.rs` (they remove its `let _ = &w;` placeholder with the first registration). They create new
  files only (`create_new`), refuse when a target exists, and insert `pub mod` / registration lines above the
  `// smeltery:…` markers (kept sorted for `pub mod` / `pub use`); a missing marker prints the line to add by hand.
  `make:agent` and `make:job` refuse in an app without Watchfire (no `app/agents/mod.rs`); `make:controller` in an app
  without `routes/web.rs` (headless) refuses and writes nothing, and `make:model -c` / `-r` there makes the model
  and its other parts and skips the controller with a note.
- `make:migration add_<column>_to_<table>_table` writes a `down` that drops the column (`ALTER TABLE … DROP
  COLUMN`). Migrations made in the same second (`make:model -m` then `make:migration`) get increasing timestamps: a
  new migration's stamp is one second after the newest one in `database/migrations/` when the clock is not past it.
- Resource controllers from `make:controller --resource` / `make:model -r` validate with `Valid<…Form>` (rules from
  the field types), flash a status message, and their forms (the delete form of the `show` view too) carry `@csrf`,
  `@error` and `old()` input, and tie each field to its error message (`aria-invalid`, `aria-describedby`); the
  counter Spark's form does the same. The update redirects through a `let url = …` line, so two-word resources
  (`InvoiceAttachment`) stay rustfmt-clean; a long `make:controller` name gets its route entry in rustfmt's layout.
  The record pages wrap a stored file's path (`break-all`) instead of overflowing on phones.
- The `file` field type for `make:model` (`image:file`, `scan:file?`): a string column holding the stored path; the
  resource forms post `multipart/form-data` with a file input, the controller stores the upload in
  `storage/app/public/<table>/`, the edit form keeps the stored file when none is chosen, and the show view links it.
- `make:model … --searchable` (apps with Prospect): writes `impl Searchable` (`string` / `text` fields searched, the
  first with `Weight::A`; `foreign` fields as filters), the `SearchIndex` in the create-table migration (or a
  separate `add_search_index_to_<table>_table` migration without `-m`) and the registration; with `-r` (and
  `make:controller --resource` for a searchable model) the list page searches: `?q=` + `PageQuery`, ranked,
  paginated, the label highlighted as text segments, `throttle:60,1` on the list route; Mold: a GET form and
  Previous / Next buttons; React / Vue: a search box debounced 300 ms (`router.get` with `preserveState`). With a
  factory, `-r` also writes `tests/<table>_search.rs` (the list without and with a search). Refused without the
  block (no `.prospect(` in `bootstrap/app.rs`) and for a model without a `string` / `text` field.
- Generators in React and Vue apps (the kit is `frontend` under `[package.metadata.smeltery]` in the app's
  `Cargo.toml`): `make:model --all` / `-r` and `make:controller [--resource]` write controllers returning Alloy pages
  and the pages under `resources/js/pages/` (React `posts/index.tsx`, `create.tsx`, `show.tsx`, `edit.tsx`; Vue
  `posts/Index.vue`, …) with `useForm`, field errors and `resources/js/types/<model>.ts`; the controller sends the
  props under the names the pages read (`postComments` / `postComment` for `PostComment`); forms with a `file` field
  post `multipart/form-data`; the controllers say in their module doc that `index`, `show` and `edit` send whole
  records to the browser. `make:page Name` writes a page, its controller and its route. `make:spark` refuses in
  React and Vue apps, `make:page` in Mold and headless apps.
- Generated code is `rustfmt`-clean: the `make:*` generators run `rustfmt --edition 2024` (or `RUSTFMT`) on the Rust
  files they create, so long names (`CustomerSupportTicket`) give `cargo fmt`-clean code (without rustfmt they print
  a note), and registration lines they add to existing files take rustfmt's layout when they pass 100 columns; a
  test runs `rustfmt --check` over a new app of each kind plus every generator.

### Security
- A `file` field of `make:model` gets a `mimes` rule in the generated forms: `jpg,jpeg,png,gif,webp` when a part of
  its name says image (`image`, `photo`, `avatar`, `cover_image`, ...), else those and `pdf,txt,csv,docx,xlsx`, so an
  uploaded `.html` or `.svg` is never served from `/storage/…` as a page of the app.
- `make:model -r` / `make:controller --resource` in an app with authentication put `create`, `store`, `edit`,
  `update` and `destroy` behind `.middleware("auth")` (`index` and `show` stay public); without authentication the
  routes stay public, and a comment above them, the controller's module doc and a printed note say so.
- In apps with Temper, `POST /register`, `/forgot-password` and `/reset-password/{token}` are throttled to 6
  requests a minute per client on each route (`throttle:6,1`), and the auth forms read the e-mail address trimmed and
  in lower case, so one address in other letters is not a second account; the generated tests check both.
- New apps' `.env` sets `SESSION_DRIVER=database` (the `sessions` migration is in every app), so sessions live on
  the server and end there.
- The `config/app.rs` of new apps reads `APP_ENV` as `production` and `APP_DEBUG` as `false` when they are not set
  (`.env` sets `local` and `true`).
- New apps' `DatabaseSeeder` creates the demo user (`demo@example.com` / `password`) only when `APP_ENV` is `local`
  or `testing`, so `db:seed --force` on a production server creates no account with a public password. The generated
  `CLAUDE.md` / `AGENTS.md` and `README.md` say so.
- `key:generate` with `APP_ENV=production` (process environment or `.env`) keeps an existing `APP_KEY` and exits
  with 1 unless `--force` is given.
- `smeltery new` writes `.env` (with `APP_KEY`) with mode `0600` on Unix.
- Generators create files with `create_new`, so a dangling symlink in the app is never written through, and add
  lines to existing files through a temporary file and a rename that keeps the file's permissions, so a crash or a
  full disk leaves the file intact. `bellows:install` writes the same way.
- `tailwind:install` downloads over HTTPS only, follows redirects only to `github.com` and
  `*.githubusercontent.com`, stops past 256 MiB, and removes part files of interrupted downloads older than an hour.
  `serve` and `build` check the per-user Tailwind binary before running it: its SHA-256 (a `.verified` stamp saves
  the hashing while its size and time stay the same) and, on Unix, that it, its folder and `smeltery/` belong to the
  user and are not writable by the group or others, and that the data folder above is not writable by others;
  `tailwind:install` tightens those permissions. `PATH` entries that are not absolute (empty, `.`) are skipped when
  looking for `tailwindcss`.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
