//! Bellows, the AI-agent support of the [Smeltery](https://github.com/smelteryworks/smeltery) framework.
//!
//! The app command `bellows:mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io) server on
//! stdin / stdout (JSON-RPC 2.0, one message per line) inside the app binary, so a coding agent can ask the app
//! itself about its routes, models, database schema, configuration keys, recent errors and documentation, and can
//! run generators, tests and agent controls. Apps register it with [`BellowsExt::bellows`]; a generated app does
//! so in `bootstrap/app.rs`, and `.mcp.json` points the agent at `smeltery bellows:mcp`.
//!
//! ```
//! use smeltery_bellows::BellowsExt as _;
//! use smeltery_core::AppBuilder;
//!
//! fn build(app: AppBuilder) -> AppBuilder {
//!     app.bellows()
//! }
//! # let _ = build(AppBuilder::new(smeltery_core::config::Settings::from_env()));
//! ```
//!
//! | Tool | Answers |
//! |---|---|
//! | `route_list` | every route: methods, path, name, middleware |
//! | `models` | the models in `app/models/` with their table's columns |
//! | `db_schema` | every table: columns (type, nullable, default, primary key) and indexes |
//! | `config_keys` | the names of the settings in `.env`, `.env.example` and the framework (never values) |
//! | `last_errors` | the latest `ERROR` (and, when asked, `WARN`) lines of the app's log file (`LOG_FILE`, else `storage/logs/*.log`) |
//! | `docs_search` | sections of the Smeltery guide, `CLAUDE.md` and `.bellows/` matching the words |
//! | `run_generator` | runs one `smeltery make:*` command (names, fields and the generators' options as arguments) |
//! | `run_tests` | runs `cargo test` (optionally filtered by a test path) and returns the summary and failures; it compiles and runs the app's code |
//! | `agents_list` | the Watchfire agents of the running app |
//! | `agent_control` | start / stop / pause / resume / restart an agent of the running app |
#![cfg_attr(docsrs, feature(doc_cfg))]

// The README's samples run as doctests without becoming the crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

mod rpc;
mod tools;

use std::path::PathBuf;
use std::time::Duration;

use smeltery_core::console::{Args, Command};
use smeltery_core::{App, AppBuilder, Result};

pub use rpc::{McpServer, PROTOCOL_VERSIONS};

/// `AppBuilder::bellows`: add the `bellows:mcp` command.
pub trait BellowsExt {
    /// Register the `bellows:mcp` app command (the MCP server on stdin / stdout).
    fn bellows(self) -> Self;
}

impl BellowsExt for AppBuilder {
    fn bellows(self) -> Self {
        self.commands(|c| {
            c.add(McpCommand);
        })
    }
}

/// The `bellows:mcp` command.
#[derive(Clone, Copy, Debug, Default)]
pub struct McpCommand;

impl Command for McpCommand {
    fn name(&self) -> &'static str {
        "bellows:mcp"
    }

    fn about(&self) -> &'static str {
        "Run the Bellows MCP server on stdin / stdout (for coding agents)"
    }

    async fn run(&self, app: &App, _args: Args) -> Result<()> {
        let server = McpServer::new(app.clone(), Options::from_app(app));
        let stdin = tokio::io::BufReader::new(tokio::io::stdin());
        server.serve(stdin, tokio::io::stdout()).await
    }
}

/// Where the tools look and how long their subprocesses and HTTP calls may take.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
    /// The app root (`.env`, `app/models/`, `storage/logs/`, `CLAUDE.md`, …).
    pub root: PathBuf,
    /// The app's log file (`LOG_FILE`) that `last_errors` reads; without one it reads `storage/logs/*.log`.
    pub log_file: Option<PathBuf>,
    /// The `smeltery` binary `run_generator` calls (`SMELTERY_BIN`, else `smeltery` on `PATH`).
    pub smeltery_bin: PathBuf,
    /// The `cargo` binary `run_tests` calls (`CARGO`, else `cargo` on `PATH`).
    pub cargo_bin: PathBuf,
    /// How long one generator may run (60 s).
    pub generator_timeout: Duration,
    /// How long `cargo test` may run (`BELLOWS_TEST_TIMEOUT` seconds, 600).
    pub test_timeout: Duration,
    /// How long one call to the running app's Watchfire API may take (10 s).
    pub http_timeout: Duration,
}

impl Options {
    /// The options for `app`: its root and the binaries and timeouts from the environment.
    pub fn from_app(app: &App) -> Self {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let test_secs = smeltery_core::config::env::<u64>("BELLOWS_TEST_TIMEOUT", 600).max(1);
        Self {
            root: app.settings().root.clone(),
            log_file: app.settings().log_file.clone(),
            smeltery_bin: var("SMELTERY_BIN").unwrap_or_else(|| PathBuf::from("smeltery")),
            cargo_bin: var("CARGO").unwrap_or_else(|| PathBuf::from("cargo")),
            generator_timeout: Duration::from_secs(60),
            test_timeout: Duration::from_secs(test_secs),
            http_timeout: Duration::from_secs(10),
        }
    }

    /// Use another app root.
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = root.into();
        self
    }

    /// Read this log file in `last_errors` (`None`: scan `storage/logs/*.log`).
    pub fn log_file(mut self, file: Option<PathBuf>) -> Self {
        self.log_file = file;
        self
    }

    /// Use another `smeltery` binary.
    pub fn smeltery_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.smeltery_bin = bin.into();
        self
    }

    /// Use another `cargo` binary.
    pub fn cargo_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.cargo_bin = bin.into();
        self
    }

    /// Change the `cargo test` timeout.
    pub fn test_timeout(mut self, timeout: Duration) -> Self {
        self.test_timeout = timeout;
        self
    }

    /// Change the generator timeout.
    pub fn generator_timeout(mut self, timeout: Duration) -> Self {
        self.generator_timeout = timeout;
        self
    }
}
