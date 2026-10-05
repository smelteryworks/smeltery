# smeltery-cli

The `smeltery` command of the [Smeltery](https://github.com/smelteryworks/smeltery) framework. Install it with the
framework crate:

```sh
cargo install smeltery
smeltery new my-app
```

## Commands

| Command | What it does |
|---|---|
| `smeltery new <name>` | Creates an app. Asks for the kind (web or headless), database, starter kit, Tailwind, Alpine.js (Mold), the building blocks to smelt into a web app, Bellows files, npm install (React / Vue), migrations, seeders and git, or takes them as flags: `--kind web\|headless`, `--db sqlite\|postgres\|mysql`, `--frontend mold\|react\|vue`, `--tailwind/--no-tailwind`, `--alpine/--no-alpine` (default no), `--smelt watchfire,temper,hallmark,anvil,prospect\|none` (default `watchfire,temper`; `watchfire` adds Watchfire agents, jobs, the scheduler and the `/_watchfire` dashboard, `temper` the authentication pages (Temper), `hallmark` API tokens (it adds `temper`, and says so), `anvil` WebSockets and broadcasting, `prospect` full-text search for models (`app/providers/search.rs`, `PROSPECT_DRIVER=database`); a hint names the blocks a chosen one works well with; headless apps always have Watchfire and refuse `--smelt` with any block), `--bellows none\|mcp,skills,guidelines\|all`, `--npm/--no-npm` (default yes), `--migrate/--no-migrate`, `--seed/--no-seed`, `--git/--no-git`, `--path <dir>`. `--migrate` and `--seed` run `migrate` and `db:seed` in the new app; `--tailwind` (Mold) downloads Tailwind (about 110 MB) as `tailwind:install` does, and a failed download leaves the app with its prebuilt CSS. `--frontend react` / `vue` writes a React or Vue (TypeScript, Inertia, Vite) starter kit whose controllers return `smeltery::alloy` pages; there `--tailwind` adds the `tailwindcss` npm packages and `--no-tailwind` a prebuilt stylesheet, and `--npm` runs `npm install --no-audit --no-fund` when Node.js 20.19+ or 22.12+ and npm answer `--version` within 5 seconds (`SMELTERY_NODE` / `SMELTERY_NPM` override the programs); a missing Node.js or a failed install is a warning, never an error. `--alpine` (Mold frontend) writes the Alpine.js 3.17.4 embedded in the CLI to `public/assets/js/alpine.min.js` and loads it in the layout after `@sparksScripts`, without a download. With authentication the app also has email verification, scaffolded and off (`.verify_email::<app::models::User>()` is commented in `bootstrap/app.rs`). Without `temper` in `--smelt`, a web app leaves out the login, registration, password reset, email verification and dashboard pages, their routes, the `password_reset_tokens` migration and the demo user. Without a terminal, or with any of these flags, it asks nothing and uses the defaults for the rest. Prints the equivalent one-line command at the end. |
| `smeltery serve` | Builds and runs the app (`serve`; `work` in a headless app, which has no `routes/`), and rebuilds and restarts it when `.rs` files or `Cargo.toml` change. Runs Tailwind with `--watch=always` when it is installed: `TAILWIND_BIN`, then the binary of `tailwind:install`, then `tailwindcss` on `PATH`. In a React or Vue app (one with `vite.config.ts`) it runs the Vite dev server instead, as `node node_modules/vite/bin/vite.js` (stdin closed), and deletes `storage/framework/vite.hot` when it stops; without `node_modules/` it prints a warning. |
| `smeltery build` | Builds `public/assets/css/app.css` from `resources/css/app.css` with Tailwind `--minify` when Tailwind is installed (found as for `serve`); without it, prints a warning and builds on; a failing Tailwind fails the build. In a React or Vue app it runs `npm ci` (or `npm install` without a `package-lock.json`) when `node_modules/` is missing and then `npm run build` instead; without npm, or when a step fails, the build fails with npm's output. Then `cargo build --release` and prints the binary path. |
| `smeltery tailwind:install` | Downloads the pinned Tailwind CSS standalone binary (v4.3.3) for this platform over HTTPS, checks its SHA-256 and keeps it in `%LOCALAPPDATA%\smeltery\bin` (Windows), `~/Library/Application Support/smeltery/bin` (macOS) or `$XDG_DATA_HOME/smeltery/bin` / `~/.local/share/smeltery/bin`. A binary already there with the right checksum is kept. |
| `smeltery test [args]` | `cargo test` with the given arguments. |
| `smeltery make:model`, `make:controller`, `make:migration`, `make:seeder`, `make:factory`, `make:command`, `make:middleware`, `make:mail`, `make:spark`, `make:agent`, `make:job` | Generators: create new files and add registration lines above the app's `// smeltery:…` markers. They never overwrite an existing file. |
| `smeltery key:generate [--show] [--force]` | Writes a new random `APP_KEY` into the app's `.env`, or prints it with `--show`. With `APP_ENV=production` an existing key is kept and the exit code is 1 unless `--force` is given. |
| `smeltery storage:link` | Links `public/storage` to `storage/app/public`. When that link is already there it says so and exits with 0; anything else at `public/storage` is an error. |
| `smeltery make:spark Name --listen <channel> --event <Event>` | In a Mold app with Anvil: a Spark that listens to broadcasts (`#[spark(stream)]`, an `#[on("anvil:<channel>", "App\\Events\\<Event>")]` method, a field per `{field}` of the channel, an event struct to fill). Refused without `.anvil(` in `bootstrap/app.rs` and for a channel name Anvil does not accept. |
| `smeltery bellows:install [--mcp] [--skills] [--guidelines] [--all]` | Adds the Bellows files (`.mcp.json`, `.bellows/skills/`, `.bellows/guidelines.md`); existing files are kept. |
| `smeltery prospect:install` | Adds full-text search to a web app: `app/providers/search.rs` and its `mod` line; prints the `bootstrap/app.rs` lines and `PROSPECT_DRIVER=database`. Refuses when the app has it. `make:model … --searchable` (in an app with search) writes `impl Searchable`, the search index migration, the registration and, with `-r`, a list page that searches. |
| `smeltery hallmark:install` | Adds API tokens to an app with authentication, as `smeltery new --smelt hallmark` writes them: the `personal_access_tokens` migration, `app/controllers/api/{tokens,user}.rs`, the token routes in `routes/api.rs`, `tests/api_tokens.rs` and, with Watchfire, the daily prune; prints the `bootstrap/app.rs` lines to add. Refuses without authentication and when the migration or a file exists. |
| any other command | Runs inside the app: `cargo run --quiet -- <command> <args>` (for example `smeltery route:list`). |

`smeltery new` never writes into a non-empty directory.

`smeltery new` on a terminal shows a SMELTERY banner in full-block letters, one section per question, a
`✓` line per created folder, a spinner while `migrate` and `db:seed` build and run the app, and a summary box with
the choices, the one-line command and the next steps. Errors and warnings carry coloured `ERROR` / `WARN` labels.
Output is plain text, without colours, banner or spinner, when stdout is not a terminal, when `NO_COLOR` is set, or
with `--no-color` (accepted by every command).

## Library use

The crate exposes one function, `smeltery_cli::main`, which the `smeltery` binary calls.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
