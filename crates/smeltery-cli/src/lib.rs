//! The `smeltery` command of the [Smeltery](https://github.com/smelteryworks/smeltery) framework.
//!
//! This crate is what `cargo install smeltery` runs: the `smeltery` binary calls [`main`]. It creates apps
//! (`smeltery new`), runs them in development with restarts on change (`smeltery serve`), builds and tests them,
//! manages `.env` keys and the public storage link, and forwards every other command (`migrate`, `route:list`, ...)
//! to the app binary with `cargo run`.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod bellows;
mod cmd;
mod files;
mod frontend;
mod generator;
mod key;
mod make;
mod new;
mod serve;
mod tailwind;
mod templates;
mod ui;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// The `smeltery` command line.
#[derive(Debug, Parser)]
#[command(
    name = "smeltery",
    version,
    about = "The Smeltery framework command line"
)]
struct Cli {
    /// Plain output: no colours, banner or spinner (also when NO_COLOR is set or stdout is not a terminal).
    #[arg(long, global = true)]
    no_color: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new Smeltery app.
    New(new::NewArgs),
    /// Run the app in development, restarting it when Rust code changes (with the Vite dev server in React / Vue
    /// apps, the Tailwind watcher in Mold apps).
    Serve {
        /// Serve HTTP only: no agents, queue workers or scheduler (run `work` for them).
        #[arg(long)]
        no_agents: bool,
    },
    /// Build the assets and the release binary: `npm run build` in React / Vue apps, the CSS with Tailwind in Mold
    /// apps (when installed: `tailwind:install`, `TAILWIND_BIN` or PATH).
    Build,
    /// Run the app's tests (`cargo test`); extra arguments are passed through.
    Test {
        /// Arguments passed to `cargo test`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Write a new APP_KEY into `.env`.
    #[command(name = "key:generate")]
    KeyGenerate {
        /// Print the key instead of writing it.
        #[arg(long)]
        show: bool,
        /// Replace an existing key under APP_ENV=production (signs every user out).
        #[arg(long)]
        force: bool,
    },
    /// Link `public/storage` to `storage/app/public`.
    #[command(name = "storage:link")]
    StorageLink,
    /// Download the pinned Tailwind CSS standalone binary into the per-user Smeltery folder (`serve` and `build` use
    /// it).
    #[command(name = "tailwind:install")]
    TailwindInstall,
    /// Create a model, optionally with its migration, controller, factory and seeder.
    #[command(name = "make:model")]
    MakeModel(make::ModelArgs),
    /// Create a controller (`--resource`: the seven resource actions, their views or React / Vue pages, and routes).
    #[command(name = "make:controller")]
    MakeController(make::ControllerArgs),
    /// Create a migration.
    #[command(name = "make:migration")]
    MakeMigration(make::MigrationArgs),
    /// Create a middleware function.
    #[command(name = "make:middleware")]
    MakeMiddleware(make::MiddlewareArgs),
    /// Create a seeder.
    #[command(name = "make:seeder")]
    MakeSeeder(make::SeederArgs),
    /// Create a model factory.
    #[command(name = "make:factory")]
    MakeFactory(make::FactoryArgs),
    /// Create a console command.
    #[command(name = "make:command")]
    MakeCmd(make::CommandArgs),
    /// Add Bellows files to an app: `.mcp.json`, `.bellows/skills/`, `.bellows/guidelines.md`.
    #[command(name = "bellows:install")]
    BellowsInstall(bellows::InstallArgs),
    /// Create a mail class with its Mold template.
    #[command(name = "make:mail")]
    MakeMail(make::MailArgs),
    /// Create a Spark (live component) with its view and register it (Mold apps).
    #[command(name = "make:spark")]
    MakeSpark(make::SparkArgs),
    /// Create a page with its controller and route (React / Vue apps).
    #[command(name = "make:page")]
    MakePage(make::PageArgs),
    /// Create a Watchfire agent and register it.
    #[command(name = "make:agent")]
    MakeAgent(make::AgentArgs),
    /// Create a queued job and register it.
    #[command(name = "make:job")]
    MakeJob(make::JobArgs),
    /// Add the migration of the `pubsub_messages` table (PubSub's `database` driver) to the app.
    #[command(name = "pubsub:install")]
    PubsubInstall,
    /// Add Hallmark's API tokens to an app with authentication: the migration, the token routes and controllers,
    /// their tests, the daily prune with Watchfire (the `bootstrap/app.rs` lines are printed).
    #[command(name = "hallmark:install")]
    HallmarkInstall,
    /// Add full-text search (Prospect) to a web app: `app/providers/search.rs`, where `make:model --searchable`
    /// registers models (the `bootstrap/app.rs` and `.env` lines are printed).
    #[command(name = "prospect:install")]
    ProspectInstall,
    /// Any other command is run by the app binary (`cargo run --quiet -- <command> <args>`).
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// Runs the `smeltery` command with the process arguments.
///
/// Errors are printed to stderr as `error: …` and turn into a failing exit code.
///
/// ```no_run
/// fn main() -> std::process::ExitCode {
///     smeltery_cli::main()
/// }
/// ```
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let ui = ui::Ui::detect(cli.no_color);
    match run(cli.command, ui) {
        Ok(code) => code,
        Err(err) => {
            if ui.styled() {
                eprintln!("  {}", ui.badged(ui::Badge::Error, &format!("{err:#}")));
            } else {
                eprintln!("error: {err:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command, ui: ui::Ui) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    match command {
        Command::New(args) => new::run(args, &cwd, ui).map(|()| ExitCode::SUCCESS),
        Command::Serve { no_agents } => serve::run(&cwd, no_agents).map(|()| ExitCode::SUCCESS),
        Command::Build => cmd::build(&cwd, ui),
        Command::Test { args } => cmd::test(&cwd, &args),
        Command::KeyGenerate { show, force } => key::run(&cwd, show, force),
        Command::StorageLink => cmd::storage_link(&cwd).map(|()| ExitCode::SUCCESS),
        Command::TailwindInstall => tailwind::run(ui),
        Command::MakeModel(a) => {
            make::run(&cwd, |c| make::model(c, &a)).map(|()| ExitCode::SUCCESS)
        }
        Command::MakeController(a) => {
            make::run(&cwd, |c| make::controller(c, &a)).map(|()| ExitCode::SUCCESS)
        }
        Command::MakeMigration(a) => {
            make::run(&cwd, |c| make::migration(c, &a)).map(|()| ExitCode::SUCCESS)
        }
        Command::MakeMiddleware(a) => {
            make::run(&cwd, |c| make::middleware(c, &a)).map(|()| ExitCode::SUCCESS)
        }
        Command::MakeSeeder(a) => {
            make::run(&cwd, |c| make::seeder(c, &a)).map(|()| ExitCode::SUCCESS)
        }
        Command::MakeFactory(a) => {
            make::run(&cwd, |c| make::factory(c, &a)).map(|()| ExitCode::SUCCESS)
        }
        Command::MakeCmd(a) => {
            make::run(&cwd, |c| make::command(c, &a)).map(|()| ExitCode::SUCCESS)
        }
        Command::BellowsInstall(a) => bellows::run(&cwd, &a).map(|()| ExitCode::SUCCESS),
        Command::MakeMail(a) => make::run(&cwd, |c| make::mail(c, &a)).map(|()| ExitCode::SUCCESS),
        Command::MakeSpark(a) => {
            make::run(&cwd, |c| make::spark(c, &a)).map(|()| ExitCode::SUCCESS)
        }
        Command::MakePage(a) => make::run(&cwd, |c| make::page(c, &a)).map(|()| ExitCode::SUCCESS),
        Command::MakeAgent(a) => {
            make::run(&cwd, |c| make::agent(c, &a)).map(|()| ExitCode::SUCCESS)
        }
        Command::MakeJob(a) => make::run(&cwd, |c| make::job(c, &a)).map(|()| ExitCode::SUCCESS),
        Command::PubsubInstall => make::run(&cwd, make::pubsub_install).map(|()| ExitCode::SUCCESS),
        Command::ProspectInstall => {
            make::run(&cwd, make::prospect_install).map(|()| ExitCode::SUCCESS)
        }
        Command::HallmarkInstall => {
            make::run(&cwd, make::hallmark_install).map(|()| ExitCode::SUCCESS)
        }
        Command::External(args) => cmd::delegate(&cwd, &args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn help_lists_no_color() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("--no-color"), "{help}");
        let mut cmd = Cli::command();
        cmd.build();
        let new_help = cmd
            .find_subcommand_mut("new")
            .map(|c| c.render_long_help().to_string())
            .unwrap_or_default();
        assert!(new_help.contains("--no-color"), "{new_help}");
        // New SQLite apps keep their database in `database/` (D-162).
        assert!(
            new_help.contains("SQLite file in `database/`"),
            "{new_help}"
        );
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn unknown_commands_are_delegated() {
        let cli = Cli::try_parse_from(["smeltery", "route:list", "--json"]).ok();
        let Some(Cli {
            command: Command::External(args),
            ..
        }) = cli
        else {
            unreachable!("expected an external command");
        };
        assert_eq!(args, ["route:list", "--json"]);
    }

    #[test]
    fn test_passes_arguments_through() {
        let cli = Cli::try_parse_from(["smeltery", "test", "home", "--", "--nocapture"]).ok();
        let Some(Cli {
            command: Command::Test { args },
            ..
        }) = cli
        else {
            unreachable!("expected the test command");
        };
        assert_eq!(args, ["home", "--", "--nocapture"]);
    }
}
