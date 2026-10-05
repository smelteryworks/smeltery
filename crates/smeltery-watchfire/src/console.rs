//! Console commands that control the running app through its API: `agents:list`,
//! `agents:start|stop|pause|resume|restart <name>`, `agents:logs <name>`; and `agents:token`, which prints the
//! API token for other clients.

use std::time::Duration;

use smeltery_core::console::{Args, Command, Commands, Output};
use smeltery_core::{App, Error, Result};

use crate::http::{Http, HttpError, HttpOptions, Method, Redirects, ReqwestTransport, Response};
use crate::time::format_utc;

/// How long a console call to the running app may take.
const TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn register(c: &mut Commands) {
    c.add(ListCommand);
    for action in ["start", "stop", "pause", "resume", "restart"] {
        c.add(ActionCommand { action });
    }
    c.add(LogsCommand);
    c.add(TokenCommand);
}

/// Terminal-safe text from the app: control characters (ANSI and OSC escape sequences start with one) and the
/// bidirectional overrides become `U+FFFD`, so a log line cannot rewrite the operator's terminal. Tabs stay.
pub(crate) fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            let bidi = matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
            if (c.is_control() && c != '\t') || bidi {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

fn encode(name: &str) -> String {
    name.replace('%', "%25")
        .replace('#', "%23")
        .replace('/', "%2F")
}

async fn call(app: &App, method: Method, path: &str) -> Result<Response> {
    let base = crate::web::api_base_url(app)?;
    let transport = ReqwestTransport::new().map_err(|e| Error::internal(e.to_string()))?;
    let http = Http::new(transport, HttpOptions::default(), &[]);
    let url = format!("{base}{path}");
    // The token goes to this address only, never along a redirect.
    let mut request = http
        .request(method, &url)
        .timeout(TIMEOUT)
        .retries(0)
        .redirects(Redirects::None);
    if let Some(token) = crate::web::api_token(&app.settings().key) {
        request = request.bearer(&token);
    }
    let response = request.send().await.map_err(|e| match e {
        HttpError::Connect(_) | HttpError::Timeout(_) => Error::internal(format!(
            "cannot reach the app at {base}: is it running? Start it with `smeltery serve`, or with \
             `smeltery work` and WATCHFIRE_API_ADDR set ({e})"
        )),
        other => Error::internal(format!("calling {url}: {other}")),
    })?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let message = response
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or_else(|| status.to_string());
    if status == http::StatusCode::UNAUTHORIZED {
        return Err(Error::internal(format!(
            "the app refused the API token ({message}): the command and the app must share APP_KEY"
        )));
    }
    Err(Error::internal(message))
}

async fn decode<T: serde::de::DeserializeOwned>(response: Response) -> Result<T> {
    response
        .json()
        .await
        .map_err(|e| Error::internal(format!("unexpected answer from the app: {e}")))
}

/// The status as the API sends it (`AgentStatus` serializes these fields).
#[derive(serde::Deserialize)]
struct StatusRow {
    name: String,
    state: String,
    health: String,
    restarts: u64,
    runs: u64,
    last_heartbeat_ms: Option<i64>,
    next_restart_at_ms: Option<i64>,
}

struct ListCommand;

impl Command for ListCommand {
    fn name(&self) -> &'static str {
        "agents:list"
    }

    fn about(&self) -> &'static str {
        "List the running app's agents (through its API)"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(&self, app: &App, _args: Args, out: Output) -> Result<()> {
        let rows: Vec<StatusRow> = decode(call(app, Method::GET, "/agents").await?).await?;
        if rows.is_empty() {
            out.line("No agents.");
            return Ok(());
        }
        let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
        out.line(format!(
            "{:<width$}  {:<11}  {:<8}  {:>8}  {:>6}  {:<19}  NEXT RESTART (UTC)",
            "NAME", "STATE", "HEALTH", "RESTARTS", "RUNS", "LAST HEARTBEAT (UTC)"
        ));
        for r in rows {
            let line = format!(
                "{:<width$}  {:<11}  {:<8}  {:>8}  {:>6}  {:<20}  {}",
                printable(&r.name),
                printable(&r.state),
                printable(&r.health),
                r.restarts,
                r.runs,
                r.last_heartbeat_ms.map(format_utc).unwrap_or_default(),
                r.next_restart_at_ms.map(format_utc).unwrap_or_default()
            );
            out.line(line.trim_end());
        }
        Ok(())
    }
}

struct ActionCommand {
    action: &'static str,
}

impl Command for ActionCommand {
    fn name(&self) -> &'static str {
        match self.action {
            "start" => "agents:start",
            "stop" => "agents:stop",
            "pause" => "agents:pause",
            "resume" => "agents:resume",
            _ => "agents:restart",
        }
    }

    fn about(&self) -> &'static str {
        match self.action {
            "start" => "Start an agent of the running app",
            "stop" => "Stop an agent of the running app",
            "pause" => "Pause an agent of the running app (no restarts until resumed)",
            "resume" => "Resume a paused agent of the running app",
            _ => "Restart an agent of the running app",
        }
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(&self, app: &App, args: Args, out: Output) -> Result<()> {
        let name = args
            .get(0)
            .ok_or_else(|| Error::internal(format!("usage: {} <name>", self.name())))?;
        let path = format!("/agents/{}/{}", encode(name), self.action);
        let status: StatusRow = decode(call(app, Method::POST, &path).await?).await?;
        out.line(format!(
            "{}: {}",
            printable(&status.name),
            printable(&status.state)
        ));
        Ok(())
    }
}

/// A log line as the API sends it.
#[derive(serde::Deserialize)]
struct LogRow {
    at_ms: i64,
    level: String,
    run_id: u64,
    message: String,
}

struct LogsCommand;

impl Command for LogsCommand {
    fn name(&self) -> &'static str {
        "agents:logs"
    }

    fn about(&self) -> &'static str {
        "Show an agent's last log lines (from the running app)"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(&self, app: &App, args: Args, out: Output) -> Result<()> {
        let name = args
            .get(0)
            .ok_or_else(|| Error::internal("usage: agents:logs <name>"))?;
        let path = format!("/agents/{}/logs", encode(name));
        let lines: Vec<LogRow> = decode(call(app, Method::GET, &path).await?).await?;
        if lines.is_empty() {
            out.line(format!("No log lines for `{name}`."));
        }
        for l in lines {
            out.line(format!(
                "{}  {:<5}  run {:<4}  {}",
                format_utc(l.at_ms),
                printable(&l.level),
                l.run_id,
                printable(&l.message)
            ));
        }
        Ok(())
    }
}

/// `agents:token`: print the API token (derived from `APP_KEY`) for clients other than the console commands. It
/// goes to standard output only, never to the log, and the key never appears on a command line.
struct TokenCommand;

impl Command for TokenCommand {
    fn name(&self) -> &'static str {
        "agents:token"
    }

    fn about(&self) -> &'static str {
        "Print the Watchfire API token (derived from APP_KEY)"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(&self, app: &App, _args: Args, out: Output) -> Result<()> {
        let token = crate::web::api_token(&app.settings().key).ok_or_else(|| {
            Error::internal(
                "APP_KEY is missing or shorter than 32 bytes: there is no Watchfire API token \
                 (run `key:generate`)",
            )
        })?;
        out.line(token);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_neutralises_terminal_escapes() {
        assert_eq!(printable("plain\ttext ✓"), "plain\ttext ✓");
        let hostile = "a\u{1b}]0;pwned\u{7}b\u{1b}[2Jc\r\nd\u{9b}e\u{202E}f\u{0}";
        let shown = printable(hostile);
        assert!(
            !shown.chars().any(|c| c.is_control() && c != '\t'),
            "{shown:?}"
        );
        assert!(!shown.contains('\u{202E}'));
        assert_eq!(shown.chars().filter(|c| *c == '\u{FFFD}').count(), 8);
    }
}
