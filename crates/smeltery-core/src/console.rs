//! The console kernel: the commands an app binary understands (`serve`, `route:list`,
//! `migrate` …) and the app's own [`Command`]s.
//!
//! The `smeltery` CLI forwards app commands to the app binary, which hands its arguments to
//! [`run`].

pub mod setup;

use std::future::Future;
use std::io::Write;
use std::process::ExitCode;

use crate::app::{App, AppBuilder, BoxFuture, Built};
use crate::config::{Settings, load_env_file, root_dir};
use crate::db::migration::MigrationStatus;
use crate::error::Result;
use crate::routing::RouteInfo;

/// Load `.env`, set up logging, build the app with `build` and run the command named by
/// the process arguments (`serve` when there is none).
///
/// This is what a generated app's `bootstrap/main.rs` calls.
pub fn run(build: impl FnOnce(AppBuilder) -> AppBuilder) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `key:generate --show` only prints a new key: it reads no `.env` (which the user running it may not be allowed
    // to read on a server) and builds nothing.
    if only_shows_a_key(&args) {
        return match setup::generate_key() {
            Ok(key) => {
                println!("{key}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let root = root_dir();
    let env_loaded = load_env_file(root.join(".env"));
    let settings = Settings::from_env();
    // Kept to the end of `run`: dropping it flushes the log file after the runtime has shut down.
    let _log = crate::logging::init(&log_settings(&settings, &args));
    if let Err(e) = env_loaded {
        tracing::warn!(error = %e, "cannot read .env");
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let builder = build(AppBuilder::new(settings));
    // Not `stdout().lock()`: a command that streams its own protocol on stdout (`bellows:mcp`)
    // writes from runtime threads, which a lock held here for the whole command would block.
    let mut stdout = std::io::stdout();
    match runtime.block_on(dispatch(builder, &args, &mut stdout)) {
        Ok(code) => code,
        Err(e) => {
            let _ = stdout.flush();
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The settings logging starts with: the file-level setup commands (`key:generate`, `storage:link`), which an
/// operator often runs with `sudo`, log to stderr only. Opening `LOG_FILE` as root would create or append to a
/// file in `storage/logs`, a folder the app user owns and could have pointed anywhere with a symlink.
fn log_settings(settings: &Settings, args: &[String]) -> Settings {
    let mut settings = settings.clone();
    if args
        .first()
        .is_some_and(|c| matches!(c.as_str(), "key:generate" | "storage:link"))
    {
        settings.log_file = None;
    }
    settings
}

/// Whether `args` are `key:generate --show`, which needs neither `.env` nor the app.
fn only_shows_a_key(args: &[String]) -> bool {
    args.first().is_some_and(|c| c == "key:generate")
        && Args::new(args.iter().skip(1).cloned()).flag("show")
}

/// A command the app binary runs: `cargo run -- greet` / `smeltery greet`.
///
/// ```
/// use smeltery_core::console::{Args, Command, Commands};
/// use smeltery_core::{App, Result};
///
/// pub struct Greet;
///
/// impl Command for Greet {
///     fn name(&self) -> &'static str {
///         "greet"
///     }
///
///     fn about(&self) -> &'static str {
///         "Say hello"
///     }
///
///     async fn run(&self, _app: &App, args: Args) -> Result<()> {
///         println!("Hello, {}!", args.get(0).unwrap_or("world"));
///         Ok(())
///     }
/// }
///
/// pub fn register(c: &mut Commands) {
///     c.add(Greet);
/// }
/// ```
pub trait Command: Send + Sync + 'static {
    /// What the user types, e.g. `greet` or `report:send`.
    fn name(&self) -> &'static str;

    /// One line for `help`.
    fn about(&self) -> &'static str {
        ""
    }

    /// Do the work. `args` are the words after the command name.
    fn run(&self, app: &App, args: Args) -> impl Future<Output = Result<()>> + Send;

    /// Like [`Command::run`], with an [`Output`] to write to instead of printing (what
    /// framework crates use, since libraries do not print). The console writes it out when
    /// the command returns. By default it calls [`Command::run`].
    fn run_with_output(
        &self,
        app: &App,
        args: Args,
        out: Output,
    ) -> impl Future<Output = Result<()>> + Send {
        let _ = out;
        self.run(app, args)
    }
}

/// Text a command writes through [`Command::run_with_output`]; the console prints it when the
/// command returns (also when it fails). Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct Output {
    buffer: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Output {
    /// Write one line.
    pub fn line(&self, text: impl AsRef<str>) {
        let mut buffer = self
            .buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        buffer.push_str(text.as_ref());
        buffer.push('\n');
    }

    /// Take everything written so far.
    pub fn take(&self) -> String {
        std::mem::take(
            &mut *self
                .buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

trait ErasedCommand: Send + Sync {
    fn name(&self) -> &'static str;
    fn about(&self) -> &'static str;
    fn run<'a>(&'a self, app: &'a App, args: Args, out: Output) -> BoxFuture<'a, Result<()>>;
}

impl<C: Command> ErasedCommand for C {
    fn name(&self) -> &'static str {
        Command::name(self)
    }

    fn about(&self) -> &'static str {
        Command::about(self)
    }

    fn run<'a>(&'a self, app: &'a App, args: Args, out: Output) -> BoxFuture<'a, Result<()>> {
        Box::pin(Command::run_with_output(self, app, args, out))
    }
}

/// The app's own commands. `app/commands/mod.rs` fills it in its `register` function, which
/// [`AppBuilder::commands`](crate::AppBuilder::commands) calls.
///
/// A command named like a built-in one (`serve`, `migrate` …) is never run: the built-in
/// wins.
#[derive(Default)]
pub struct Commands {
    commands: Vec<Box<dyn ErasedCommand>>,
}

impl std::fmt::Debug for Commands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.commands.iter().map(|c| c.name()))
            .finish()
    }
}

impl Commands {
    /// No commands.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a command.
    pub fn add(&mut self, command: impl Command) -> &mut Self {
        self.commands.push(Box::new(command));
        self
    }

    /// `(name, about)` of every registered command, in order.
    pub fn list(&self) -> Vec<(&'static str, &'static str)> {
        self.commands
            .iter()
            .map(|c| (c.name(), c.about()))
            .collect()
    }

    fn get(&self, name: &str) -> Option<&dyn ErasedCommand> {
        self.commands
            .iter()
            .find(|c| c.name() == name)
            .map(|c| c.as_ref())
    }
}

/// The words after a command name.
///
/// `--name=value` and `--name value` are options, `--name` alone is a flag, everything else
/// is positional. An option followed by a word that does not start with `-` takes that word
/// as its value, so put flags after positional arguments (`greet Ada --loud`) or write
/// values with `=`.
///
/// ```
/// use smeltery_core::console::Args;
///
/// let args = Args::new(["Ada", "--step", "2", "--force", "--class=Users"]);
/// assert_eq!(args.get(0), Some("Ada"));
/// assert_eq!(args.value("step"), Some("2"));
/// assert_eq!(args.value("class"), Some("Users"));
/// assert!(args.flag("force"));
/// assert!(!args.flag("seed"));
/// assert_eq!(args.positional(), ["Ada"]);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    positional: Vec<String>,
    options: Vec<(String, Option<String>)>,
}

impl Args {
    /// Parse the words after the command name.
    pub fn new<I, S>(words: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let words: Vec<String> = words.into_iter().map(Into::into).collect();
        let mut args = Self::default();
        let mut iter = words.into_iter().peekable();
        while let Some(word) = iter.next() {
            if let Some(option) = word.strip_prefix("--").filter(|o| !o.is_empty()) {
                if let Some((name, value)) = option.split_once('=') {
                    args.options.push((name.to_owned(), Some(value.to_owned())));
                } else {
                    let value = iter.next_if(|next| !next.starts_with('-'));
                    args.options.push((option.to_owned(), value));
                }
            } else {
                args.positional.push(word);
            }
        }
        args
    }

    /// The positional argument at `index`.
    pub fn get(&self, index: usize) -> Option<&str> {
        self.positional.get(index).map(String::as_str)
    }

    /// Every positional argument.
    pub fn positional(&self) -> &[String] {
        &self.positional
    }

    /// The value of `--name=value` / `--name value` (the last one when repeated).
    pub fn value(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    /// Whether `--name` was given (with or without a value; `--name=false` is `false`).
    pub fn flag(&self, name: &str) -> bool {
        self.options
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .is_some_and(|(_, v)| !matches!(v.as_deref(), Some("false" | "0" | "no")))
    }
}

/// The built-in commands, as `help` lists them.
const BUILT_INS: &[(&str, &str)] = &[
    (
        "serve",
        "Start the HTTP server (the default; --no-agents: without the background work)",
    ),
    (
        "work",
        "Run the background agents, jobs and schedule without the HTTP server",
    ),
    ("route:list", "List every route"),
    ("migrate", "Run the pending migrations"),
    (
        "migrate:rollback",
        "Revert the last batch of migrations (--step N: the last N)",
    ),
    (
        "migrate:fresh",
        "Drop every table and run all migrations (--seed: then seed)",
    ),
    ("migrate:status", "Show which migrations have run"),
    ("db:seed", "Run the seeders (--class Name: only that one)"),
    (
        "cache:clear",
        "Remove the cache's entries, keeping Watchfire's leases (an argument names the store, e.g. `cache:clear redis`; --all: everything)",
    ),
    (
        "key:generate",
        "Write a new APP_KEY into .env (--show: print it; --force: replace a key in production)",
    ),
    ("storage:link", "Link public/storage to storage/app/public"),
    ("help", "Show this list"),
];

/// Run one command, writing its output to `out` (a user command may also print to stdout).
///
/// `migrate`, `migrate:rollback`, `migrate:fresh` and `db:seed` refuse to run with
/// `APP_ENV=production` unless `--force` is given.
/// `key:generate` keeps an existing `APP_KEY` there unless `--force` is given.
///
/// # Errors
/// The app fails to build or the command fails.
pub async fn dispatch(
    mut builder: AppBuilder,
    args: &[String],
    out: &mut dyn Write,
) -> Result<ExitCode> {
    let command = args.first().map_or("serve", String::as_str);
    let rest = Args::new(args.iter().skip(1).cloned());
    let commands = builder.take_commands();
    let mut serve_commands = builder.take_serve_commands();
    let production = builder.settings().is_production();
    if production
        && matches!(
            command,
            "migrate" | "migrate:rollback" | "migrate:fresh" | "db:seed"
        )
        && !rest.flag("force")
    {
        writeln!(
            out,
            "The app runs in production (APP_ENV=production): `{command}` changes the database.\n\
             Run it again with --force to go ahead."
        )?;
        return Ok(ExitCode::FAILURE);
    }
    let builtin = BUILT_INS.iter().any(|(name, _)| *name == command)
        || matches!(command, "list" | "--help" | "-h");
    let serve_command = if builtin {
        None
    } else {
        serve_commands
            .iter()
            .position(|c| c.name == command)
            .map(|i| serve_commands.swap_remove(i))
    };
    if matches!(command, "serve" | "work") || serve_command.is_some() {
        // Before building: the refusal is the same one `serve_on` / `work` give, reported once.
        crate::server::refuse_testing(builder.settings())?;
    }
    match command {
        _ if serve_command.is_some() => {
            let Some(serve) = serve_command else {
                return Ok(ExitCode::FAILURE);
            };
            let built = builder.build().await?;
            (serve.run)(built, rest).await?;
            Ok(ExitCode::SUCCESS)
        }
        "help" | "list" | "--help" | "-h" => {
            write_help(out, &commands, &serve_commands)?;
            Ok(ExitCode::SUCCESS)
        }
        "serve" => {
            let Built { app, router } = builder.build().await?;
            if rest.flag("no-agents") {
                // A web-only process: the agents, queue workers and scheduler run elsewhere (`work`).
                app.skip_background();
            }
            crate::server::serve(app, router).await?;
            Ok(ExitCode::SUCCESS)
        }
        "work" => {
            let Built { app, .. } = builder.build().await?;
            if crate::server::work(&app).await? {
                Ok(ExitCode::SUCCESS)
            } else {
                writeln!(
                    out,
                    "Nothing to run: this app registers no background work (agents, jobs or schedules)."
                )?;
                Ok(ExitCode::FAILURE)
            }
        }
        "route:list" => {
            let Built { app, .. } = builder.build().await?;
            write_routes(out, app.routes())?;
            Ok(ExitCode::SUCCESS)
        }
        "migrate" => {
            let app = build_with_db(builder).await?;
            let ran = app.migrator().migrate(&app.db()?).await?;
            write_names(out, "Migrated", &ran, "Nothing to migrate.")?;
            Ok(ExitCode::SUCCESS)
        }
        "migrate:rollback" => {
            let steps = match rest.value("step") {
                None => None,
                Some(raw) => Some(raw.parse::<usize>().map_err(|_| {
                    crate::Error::internal(format!("--step must be a number, not `{raw}`"))
                })?),
            };
            let app = build_with_db(builder).await?;
            let done = app.migrator().rollback(&app.db()?, steps).await?;
            write_names(out, "Rolled back", &done, "Nothing to roll back.")?;
            Ok(ExitCode::SUCCESS)
        }
        "migrate:fresh" => {
            let app = build_with_db(builder).await?;
            let db = app.db()?;
            writeln!(out, "Dropped all tables.")?;
            let ran = app.migrator().fresh(&db).await?;
            write_names(out, "Migrated", &ran, "Nothing to migrate.")?;
            if rest.flag("seed") {
                let seeded = app.seeders().run(&db, None).await?;
                write_names(out, "Seeded", &seeded, "No seeders are registered.")?;
            }
            Ok(ExitCode::SUCCESS)
        }
        "migrate:status" => {
            let app = build_with_db(builder).await?;
            let status = app.migrator().status(&app.db()?).await?;
            write_status(out, &status)?;
            Ok(ExitCode::SUCCESS)
        }
        "db:seed" => {
            let app = build_with_db(builder).await?;
            let seeded = app.seeders().run(&app.db()?, rest.value("class")).await?;
            write_names(out, "Seeded", &seeded, "No seeders are registered.")?;
            Ok(ExitCode::SUCCESS)
        }
        "key:generate" => {
            let key = setup::generate_key()?;
            if rest.flag("show") {
                writeln!(out, "{key}")?;
                return Ok(ExitCode::SUCCESS);
            }
            let root = builder.settings().root.clone();
            match setup::write_app_key(&root, &key, production, rest.flag("force"))? {
                setup::KeyWrite::Written(_) => {
                    writeln!(out, "{}", setup::key_written_message())?;
                    Ok(ExitCode::SUCCESS)
                }
                setup::KeyWrite::WrittenInPlace(_) => {
                    writeln!(out, "{}", setup::key_written_message())?;
                    writeln!(out, "{}", setup::KEY_WRITTEN_IN_PLACE)?;
                    Ok(ExitCode::SUCCESS)
                }
                setup::KeyWrite::KeptInProduction => {
                    writeln!(out, "{}", setup::KEY_KEPT_IN_PRODUCTION)?;
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        "storage:link" => {
            let linked = setup::link_storage(&builder.settings().root)?;
            writeln!(out, "{}", linked.message())?;
            Ok(ExitCode::SUCCESS)
        }
        "cache:clear" => {
            let Built { app, .. } = builder.build().await?;
            let cache = match rest.get(0) {
                Some(store) => app.cache().store(store)?,
                None => app.cache(),
            };
            if rest.flag("all") {
                cache.flush_all().await?;
                writeln!(
                    out,
                    "Cleared the `{}` cache store, Watchfire's leases and claims included.",
                    cache.store_name()
                )?;
                return Ok(ExitCode::SUCCESS);
            }
            if cache.store_name() == "memcached" {
                // Memcached cannot list keys: a flush empties the servers, Watchfire's leases with them.
                writeln!(
                    out,
                    "memcached cannot keep Watchfire's leases and schedule claims while it clears the rest.\n\
                     Run it again with --all to empty the servers anyway."
                )?;
                return Ok(ExitCode::FAILURE);
            }
            cache.flush().await?;
            writeln!(out, "Cleared the `{}` cache store.", cache.store_name())?;
            Ok(ExitCode::SUCCESS)
        }
        other => {
            if let Some(user) = commands.get(other) {
                let Built { app, .. } = builder.build().await?;
                // A command may publish to the app's other processes (PUBSUB_DRIVER=database / redis).
                crate::pubsub::start(&app, crate::pubsub::Role::Other).await?;
                let output = Output::default();
                let result = user.run(&app, rest, output.clone()).await;
                out.write_all(output.take().as_bytes())?;
                // What the command left running for the app (queued PubSub messages, a reset mail) finishes
                // within the shutdown budget before the process exits.
                app.shutdown();
                app.tasks().close();
                let _ =
                    tokio::time::timeout(app.settings().shutdown_timeout, app.tasks().wait()).await;
                result?;
                return Ok(ExitCode::SUCCESS);
            }
            writeln!(out, "unknown command `{other}`\n")?;
            write_help(out, &commands, &serve_commands)?;
            Ok(ExitCode::from(2))
        }
    }
}

/// Build the app and make sure it has a database.
async fn build_with_db(builder: AppBuilder) -> Result<App> {
    if builder.settings().database_url.is_empty() {
        return Err(crate::Error::internal(
            "no database is configured: set DATABASE_URL in .env",
        ));
    }
    Ok(builder.build().await?.app)
}

fn write_names(
    out: &mut dyn Write,
    verb: &str,
    names: &[impl AsRef<str>],
    none: &str,
) -> Result<()> {
    if names.is_empty() {
        writeln!(out, "{none}")?;
    }
    for name in names {
        writeln!(out, "{verb}: {}", name.as_ref())?;
    }
    Ok(())
}

fn write_status(out: &mut dyn Write, status: &[MigrationStatus]) -> Result<()> {
    if status.is_empty() {
        writeln!(out, "No migrations are registered.")?;
        return Ok(());
    }
    writeln!(out, "{:<8} {:<6} MIGRATION", "STATUS", "BATCH")?;
    for s in status {
        let (state, batch) = match s.batch {
            Some(batch) => ("Ran", batch.to_string()),
            None => ("Pending", String::new()),
        };
        writeln!(out, "{state:<8} {batch:<6} {}", s.name)?;
    }
    Ok(())
}

fn write_help(
    out: &mut dyn Write,
    commands: &Commands,
    serve_commands: &[crate::app::ServeCommand],
) -> Result<()> {
    let user = commands.list();
    let servers: Vec<(&str, &str)> = serve_commands.iter().map(|c| (c.name, c.about)).collect();
    let width = BUILT_INS
        .iter()
        .chain(servers.iter())
        .chain(user.iter())
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0);
    writeln!(out, "Commands:")?;
    for (name, about) in BUILT_INS.iter().chain(servers.iter()) {
        writeln!(out, "  {name:<width$}  {about}")?;
    }
    if !user.is_empty() {
        writeln!(out, "\nApp commands:")?;
        for (name, about) in &user {
            writeln!(out, "  {name:<width$}  {about}")?;
        }
    }
    Ok(())
}

/// Print the route table: method, path, name, middleware.
///
/// # Errors
/// Writing to `out` fails.
pub fn write_routes(out: &mut dyn Write, routes: &[RouteInfo]) -> Result<()> {
    let rows: Vec<[String; 4]> = routes
        .iter()
        .map(|r| {
            [
                r.methods.join("|"),
                r.path.clone(),
                r.name.clone().unwrap_or_default(),
                r.middleware.join(", "),
            ]
        })
        .collect();
    let header = [
        "METHOD".to_owned(),
        "PATH".to_owned(),
        "NAME".to_owned(),
        "MIDDLEWARE".to_owned(),
    ];
    let mut widths = header.each_ref().map(String::len);
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(&rows) {
        let line = row
            .iter()
            .zip(widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect::<Vec<_>>()
            .join("  ");
        writeln!(out, "{}", line.trim_end())?;
    }
    if rows.is_empty() {
        writeln!(out, "(no routes)")?;
    }
    Ok(())
}

/// Install the global `tracing` subscriber once (later calls do nothing), logging to stderr only.
///
/// [`run`] uses [`logging::init`](crate::logging::init) instead, which adds the `LOG_FILE` log file.
pub fn init_tracing(level: &str) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(crate::logging::level_filter(level))
        .with_target(false)
        .with_ansi(crate::logging::console_ansi())
        .with_writer(crate::logging::EscapedStderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn h() {}

    #[tokio::test]
    async fn route_list_prints_a_table() {
        let builder = AppBuilder::new(Settings::from_env())
            .middleware(
                "auth",
                |req: crate::middleware::Request, next: crate::middleware::Next| next.run(req),
            )
            .routes(|r| {
                r.get("/", h).name("home");
                r.post("/posts", h).name("posts.store").middleware("auth");
            })
            .api_routes(|r| {
                r.get("/health", h);
            });
        let mut out = Vec::new();
        let code = dispatch(builder, &["route:list".into()], &mut out)
            .await
            .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "METHOD    PATH         NAME         MIDDLEWARE");
        assert_eq!(lines[1], "GET|HEAD  /            home");
        assert_eq!(lines[2], "POST      /posts       posts.store  auth");
        assert_eq!(lines[3], "GET|HEAD  /api/health");
    }

    #[tokio::test]
    async fn a_serve_command_gets_the_built_app_and_its_arguments() {
        let ran = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen = std::sync::Arc::clone(&ran);
        let builder = AppBuilder::new(Settings::from_env())
            .routes(|r| {
                r.get("/", h).name("home");
            })
            .serve_command("sockets", "Serve the sockets", move |built, args| {
                let routes = built.app.routes().len();
                *seen.lock().unwrap() = Some((routes, args.value("port").map(str::to_owned)));
                async { Ok(()) }
            })
            .serve_command("serve", "Never run: the built-in wins", |_, _| async {
                Err(crate::Error::internal("ran"))
            });
        let mut out = Vec::new();
        let code = dispatch(builder, &["sockets".into(), "--port=9".into()], &mut out)
            .await
            .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        let (routes, port) = ran.lock().unwrap().clone().unwrap();
        assert!(routes >= 1, "the app was built with its routes");
        assert_eq!(port.as_deref(), Some("9"));

        let builder = AppBuilder::new(Settings::from_env()).serve_command(
            "sockets",
            "Serve the sockets",
            |_, _| async { Ok(()) },
        );
        let mut out = Vec::new();
        dispatch(builder, &["help".into()], &mut out).await.unwrap();
        let help = String::from_utf8(out).unwrap();
        assert!(
            help.contains("  sockets  ") && help.contains("Serve the sockets"),
            "{help}"
        );
    }

    #[tokio::test]
    async fn unknown_command_exits_2() {
        let mut out = Vec::new();
        let code = dispatch(
            AppBuilder::new(Settings::from_env()),
            &["nope".into()],
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(code, ExitCode::from(2));
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("unknown command `nope`")
        );
    }

    fn settings_at(root: &std::path::Path, env: &str) -> Settings {
        let mut settings = Settings::from_env();
        settings.root = root.to_path_buf();
        settings.env = env.to_owned();
        settings
    }

    async fn run_in(settings: Settings, args: &[&str]) -> (ExitCode, String) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let mut out = Vec::new();
        let code = dispatch(AppBuilder::new(settings), &args, &mut out)
            .await
            .unwrap();
        (code, String::from_utf8(out).unwrap())
    }

    #[tokio::test]
    async fn key_generate_writes_env_and_show_only_prints() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join(".env");
        std::fs::write(&env, "APP_NAME=x\nAPP_KEY=\n").unwrap();
        let (code, text) = run_in(
            settings_at(dir.path(), "local"),
            &["key:generate", "--show"],
        )
        .await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(
            text.starts_with("base64:") && text.ends_with('\n'),
            "{text}"
        );
        assert_eq!(
            std::fs::read_to_string(&env).unwrap(),
            "APP_NAME=x\nAPP_KEY=\n"
        );

        let (code, text) = run_in(settings_at(dir.path(), "local"), &["key:generate"]).await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(text.starts_with("APP_KEY written to .env"), "{text}");
        let written = std::fs::read_to_string(&env).unwrap();
        assert!(
            written.starts_with("APP_NAME=x\nAPP_KEY=base64:"),
            "{written}"
        );
    }

    #[tokio::test]
    async fn key_generate_keeps_a_production_key_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join(".env");
        let before = "APP_ENV=production\nAPP_KEY=base64:old\n";
        std::fs::write(&env, before).unwrap();
        let (code, text) = run_in(settings_at(dir.path(), "production"), &["key:generate"]).await;
        assert_eq!(code, ExitCode::FAILURE);
        assert!(text.contains("--force"), "{text}");
        assert_eq!(std::fs::read_to_string(&env).unwrap(), before);

        let (code, _) = run_in(
            settings_at(dir.path(), "production"),
            &["key:generate", "--force"],
        )
        .await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(
            !std::fs::read_to_string(&env)
                .unwrap()
                .contains("base64:old")
        );
    }

    #[tokio::test]
    async fn storage_link_is_a_built_in_command() {
        let dir = tempfile::tempdir().unwrap();
        // An existing `public/storage` is refused with an error, on every OS.
        std::fs::create_dir_all(dir.path().join("public/storage")).unwrap();
        let mut out = Vec::new();
        let err = dispatch(
            AppBuilder::new(settings_at(dir.path(), "local")),
            &["storage:link".into()],
            &mut out,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        #[cfg(unix)]
        {
            std::fs::remove_dir(dir.path().join("public/storage")).unwrap();
            let (code, text) = run_in(settings_at(dir.path(), "local"), &["storage:link"]).await;
            assert_eq!(code, ExitCode::SUCCESS);
            assert_eq!(text, "Linked public/storage to storage/app/public\n");
            assert!(dir.path().join("public/storage").is_symlink());
        }
    }

    #[tokio::test]
    async fn the_setup_commands_never_build_the_app() {
        let dir = tempfile::tempdir().unwrap();
        // An app whose build fails: an unknown session driver.
        let broken = || {
            let mut settings = settings_at(dir.path(), "local");
            settings.session_driver = "no-such-driver".to_owned();
            settings
        };
        let mut out = Vec::new();
        let built = dispatch(AppBuilder::new(broken()), &["route:list".into()], &mut out).await;
        assert!(built.is_err(), "the builder really fails");

        let (code, text) = run_in(broken(), &["key:generate"]).await;
        assert_eq!(code, ExitCode::SUCCESS, "{text}");
        assert!(text.starts_with("APP_KEY written to .env"), "{text}");
        let (code, text) = run_in(broken(), &["key:generate", "--show"]).await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(text.starts_with("base64:"), "{text}");

        // storage:link: on every OS the existing-target path answers without a build.
        std::fs::create_dir_all(dir.path().join("public/storage")).unwrap();
        let mut out = Vec::new();
        let err = dispatch(
            AppBuilder::new(broken()),
            &["storage:link".into()],
            &mut out,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn the_file_level_setup_commands_never_open_the_log_file() {
        let mut settings = Settings::from_env();
        settings.log_file = Some("storage/logs/smeltery.log".into());
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        for command in [
            &["key:generate"][..],
            &["storage:link"],
            &["key:generate", "--force"],
        ] {
            assert_eq!(log_settings(&settings, &args(command)).log_file, None);
        }
        for command in [&["serve"][..], &["migrate", "--force"], &[]] {
            assert!(log_settings(&settings, &args(command)).log_file.is_some());
        }
    }

    #[tokio::test]
    async fn serve_and_work_refuse_the_public_test_key() {
        for command in ["serve", "work"] {
            let mut settings = Settings::from_env();
            settings.env = "testing".into();
            settings.key = String::new();
            let mut out = Vec::new();
            let err = dispatch(AppBuilder::new(settings), &[command.into()], &mut out)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("public test key"), "{command}: {err}");
        }
    }

    #[test]
    fn key_generate_show_needs_no_env_file() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(only_shows_a_key(&args(&["key:generate", "--show"])));
        assert!(!only_shows_a_key(&args(&["key:generate"])));
        assert!(!only_shows_a_key(&args(&["key:generate", "--force"])));
        assert!(!only_shows_a_key(&args(&["serve", "--show"])));
        assert!(!only_shows_a_key(&args(&[])));
    }

    #[tokio::test]
    async fn help_lists_the_setup_commands() {
        let (_, text) = run_in(Settings::from_env(), &["help"]).await;
        assert!(text.contains("key:generate"), "{text}");
        assert!(text.contains("storage:link"), "{text}");
    }
}
