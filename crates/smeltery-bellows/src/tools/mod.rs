//! The MCP tools.

mod agents;
mod docs;
mod files;
mod process;
mod schema;

use serde_json::{Value, json};
use smeltery_core::App;

use crate::Options;

/// What a tool call answers: text for the agent, and whether it is an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) text: String,
    pub(crate) is_error: bool,
}

impl Outcome {
    pub(crate) fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub(crate) fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }

    pub(crate) fn json(value: &Value) -> Self {
        Self::ok(serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()))
    }
}

/// The tools of one server.
#[derive(Debug)]
pub(crate) struct Tools {
    app: App,
    options: Options,
}

const NAMES: &[&str] = &[
    "route_list",
    "models",
    "db_schema",
    "config_keys",
    "last_errors",
    "docs_search",
    "run_generator",
    "run_tests",
    "agents_list",
    "agent_control",
];

impl Tools {
    pub(crate) fn new(app: App, options: Options) -> Self {
        Self { app, options }
    }

    pub(crate) fn exists(name: &str) -> bool {
        NAMES.contains(&name)
    }

    /// The `tools/list` entries.
    pub(crate) fn definitions() -> Value {
        let none = json!({ "type": "object", "properties": {}, "additionalProperties": false });
        json!([
            {
                "name": "route_list",
                "description": "Every route of the app: HTTP methods, path, route name and middleware.",
                "inputSchema": none,
            },
            {
                "name": "models",
                "description": "The models in app/models/ with the columns of their database table.",
                "inputSchema": none,
            },
            {
                "name": "db_schema",
                "description": "The database schema: every table with its columns (type, nullable, default, primary key) and indexes. Pass `table` for one table.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "table": { "type": "string", "description": "Only this table." } },
                    "additionalProperties": false,
                },
            },
            {
                "name": "config_keys",
                "description": "The names of the configuration keys from .env, .env.example and the framework. Values are never returned.",
                "inputSchema": none,
            },
            {
                "name": "last_errors",
                "description": "The latest ERROR lines from the app's log file (LOG_FILE, else storage/logs/*.log). Pass `warnings: true` to include WARN lines.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "How many lines (20)." },
                        "warnings": { "type": "boolean", "description": "Include WARN lines (false)." },
                    },
                    "additionalProperties": false,
                },
            },
            {
                "name": "docs_search",
                "description": "Search the Smeltery guide, the app's CLAUDE.md and .bellows/ files; returns the best matching sections.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "query": { "type": "string", "description": "Words to look for, e.g. `resource controller`." } },
                    "required": ["query"],
                    "additionalProperties": false,
                },
            },
            {
                "name": "run_generator",
                "description": format!("Run one Smeltery generator in the app, e.g. {{\"command\": \"make:model\", \"args\": [\"Post\", \"title:string\", \"-mcr\"]}}. Allowed commands: {}.", process::GENERATORS.join(", ")),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "enum": process::GENERATORS },
                        "args": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 100 } },
                    },
                    "required": ["command"],
                    "additionalProperties": false,
                },
            },
            {
                "name": "run_tests",
                "description": "Run `cargo test` in the app (with a time limit) and return the result summary and the failures. This compiles and runs the app's code, build scripts and tests. Pass `filter` (a test path such as `posts::creates_a_post`) to run only matching tests.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "filter": { "type": "string", "pattern": "^[A-Za-z0-9_:]*$", "maxLength": 200 } },
                    "additionalProperties": false,
                },
            },
            {
                "name": "agents_list",
                "description": "The Watchfire agents of the running app (`smeltery serve` or `smeltery work`) with their state.",
                "inputSchema": none,
            },
            {
                "name": "agent_control",
                "description": "Start, stop, pause, resume or restart one Watchfire agent of the running app.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "action": { "type": "string", "enum": agents::ACTIONS },
                    },
                    "required": ["name", "action"],
                    "additionalProperties": false,
                },
            },
        ])
    }

    /// Run tool `name` with `args`; failures come back as an error outcome, never a panic.
    pub(crate) async fn call(&self, name: &str, args: &Value) -> Outcome {
        let str_arg = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_owned);
        match name {
            "route_list" => schema::route_list(&self.app),
            "models" => schema::models(&self.app, &self.options.root).await,
            "db_schema" => schema::db_schema(&self.app, str_arg("table").as_deref()).await,
            "config_keys" => files::config_keys(&self.options.root),
            "last_errors" => {
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(20)
                    .clamp(1, 200);
                let warnings = args
                    .get("warnings")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                files::last_errors(
                    &self.options,
                    usize::try_from(limit).unwrap_or(20),
                    warnings,
                )
            }
            "docs_search" => match str_arg("query") {
                Some(q) => docs::search(&self.options.root, &q),
                None => Outcome::error("missing `query`"),
            },
            "run_generator" => {
                let Some(command) = str_arg("command") else {
                    return Outcome::error("missing `command`");
                };
                let list: Option<Vec<String>> = match args.get("args") {
                    None | Some(Value::Null) => Some(Vec::new()),
                    Some(Value::Array(a)) => {
                        a.iter().map(|v| v.as_str().map(str::to_owned)).collect()
                    }
                    Some(_) => None,
                };
                let Some(list) = list else {
                    return Outcome::error(
                        "invalid generator arguments: `args` is a list of strings",
                    );
                };
                process::run_generator(&self.options, &command, &list).await
            }
            "run_tests" => process::run_tests(&self.options, str_arg("filter").as_deref()).await,
            "agents_list" => agents::list(&self.app, &self.options).await,
            "agent_control" => match (str_arg("name"), str_arg("action")) {
                (Some(n), Some(a)) => agents::control(&self.app, &self.options, &n, &a).await,
                _ => Outcome::error("`name` and `action` are required"),
            },
            other => Outcome::error(format!("unknown tool `{other}`")),
        }
    }
}
