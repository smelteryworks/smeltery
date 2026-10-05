//! The dashboard: `GET /_watchfire`, a Mold page compiled into this crate, its command
//! forms (`POST /_watchfire/agents/{name}/{action}`, web routes with sessions and CSRF) and its
//! stylesheet (`GET /_watchfire/assets/watchfire.css`, embedded in this crate).

use axum::extract::Path;
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use smeltery_core::App;
use smeltery_core::session::Session;
use smeltery_mold::{Engine, Error as MoldError, Host, Template};

use super::{DashboardAccess, PREFIX};
use crate::runtime::Agents;
use crate::status::{AgentStatus, Health, RunRecord};
use crate::time::format_utc;

/// The page's data; every value is text the template prints.
#[derive(Serialize, smeltery_mold_macros::Mold)]
#[mold("watchfire/dashboard", crate = "smeltery_mold", dir = "views")]
pub(crate) struct Page {
    pub(crate) app_name: String,
    /// `APP_ENV`, shown as a badge.
    pub(crate) env: String,
    /// `local`, `production` or `other`: the badge's colour.
    pub(crate) env_kind: String,
    /// The stylesheet URL's `v` query ([`asset_version`]).
    pub(crate) asset_version: String,
    /// This crate's version, in the footer.
    pub(crate) version: String,
    pub(crate) running: bool,
    /// Watchfire does not run here, but the shared tables show the agents of other processes.
    pub(crate) elsewhere: bool,
    /// Sparks are installed: the panels are live components.
    pub(crate) live: bool,
    /// A stop / restart confirmation asked for with `?agent=…&confirm=…` is open: the page is rendered without
    /// Sparks and without the 5 s reload, so the confirmation stays until it is answered.
    pub(crate) confirming: bool,
    /// The agents panel's own notice (empty on the page; the live panel shows action results).
    pub(crate) notice: String,
    pub(crate) summary: AgentSummary,
    pub(crate) agents: Vec<AgentRow>,
    pub(crate) has_queue: bool,
    pub(crate) queue: QueueRow,
    pub(crate) dead_letters: Vec<DeadRow>,
    pub(crate) schedule: Vec<ScheduleRow>,
    /// The earliest next run of the schedule (empty when nothing is scheduled) and its task.
    pub(crate) next_run: String,
    pub(crate) next_name: String,
}

/// The agent counts of the summary tiles.
#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct AgentSummary {
    pub(crate) total: String,
    /// Running or starting.
    pub(crate) running: String,
    pub(crate) paused: String,
    /// Failed or backing off.
    pub(crate) failing: String,
    pub(crate) standby: String,
}

/// A button of an agent's row.
#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct ActionButton {
    /// The action (`start`, `stop`, …).
    pub(crate) name: String,
    pub(crate) label: String,
    /// Asks for a confirmation first (stop, restart).
    pub(crate) danger: bool,
    /// What the confirmation says the action does.
    pub(crate) warning: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct AgentRow {
    pub(crate) name: String,
    /// The name for URLs (`#` encoded).
    pub(crate) path: String,
    /// The name for a query string (percent-encoded).
    pub(crate) query: String,
    /// The name as a quoted argument of a Sparks action call (`\` and `'` escaped for `sparks.js`).
    pub(crate) arg: String,
    pub(crate) state: String,
    /// The state for people (`backing off`).
    pub(crate) state_label: String,
    pub(crate) health: String,
    pub(crate) last_heartbeat: String,
    pub(crate) restarts: String,
    pub(crate) runs: String,
    pub(crate) backoff: String,
    pub(crate) last_error: String,
    pub(crate) recent: Vec<RunRow>,
    /// Where it runs, when not in this process.
    pub(crate) place: String,
    /// The buttons its state offers.
    pub(crate) actions: Vec<ActionButton>,
    /// The action whose confirmation is open (live panel only), or empty.
    pub(crate) confirming: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct RunRow {
    pub(crate) id: String,
    pub(crate) outcome: String,
    pub(crate) started: String,
    pub(crate) duration: String,
    pub(crate) job: String,
    pub(crate) error: String,
    pub(crate) counters: String,
    /// The process that ran it.
    pub(crate) process: String,
    /// Set on the first run of a process other than the one of the run above (run ids count per process).
    pub(crate) sep: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct QueueRow {
    pub(crate) driver: String,
    pub(crate) pending: String,
    pub(crate) reserved: String,
    pub(crate) dead: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct DeadRow {
    pub(crate) id: String,
    pub(crate) job: String,
    pub(crate) attempts: String,
    pub(crate) error: String,
    pub(crate) failed: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct ScheduleRow {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) expression: String,
    pub(crate) next: String,
}

/// The page renders from the code compiled into this crate in debug and release builds alike
/// (the app's runtime engine reads the app's own `resources/views`, where this template is
/// not). The equality test proves the runtime interpreter gives the same bytes.
pub(crate) struct Compiled(pub(crate) Page);

impl Template for Compiled {
    const NAME: &'static str = Page::NAME;

    fn render_runtime(&self, host: &dyn Host) -> Result<String, MoldError> {
        self.0.render_compiled(host)
    }

    fn render_runtime_with(&self, _engine: &Engine, host: &dyn Host) -> Result<String, MoldError> {
        self.0.render_compiled(host)
    }

    fn render_compiled(&self, host: &dyn Host) -> Result<String, MoldError> {
        self.0.render_compiled(host)
    }
}

pub(crate) fn routes(r: &mut smeltery_core::routing::Router) {
    r.get(PREFIX, page).name("watchfire.dashboard");
    r.post("/_watchfire/agents/{name}/{action}", act)
        .name("watchfire.agents.action");
}

/// The dashboard's stylesheet, embedded in the crate (it needs nothing from the app).
pub(crate) const WATCHFIRE_CSS: &str = include_str!("../../assets/watchfire.css");

/// Where the stylesheet is served (an API-style route without sessions: it is the same for everyone and holds no
/// data, so its responses carry no cookie and can be cached).
pub(crate) const ASSETS_PREFIX: &str = "/_watchfire/assets";

/// FNV-1a of the stylesheet, so its URL changes whenever its bytes do (also between builds of one version).
const CSS_HASH: u32 = {
    let mut rest = WATCHFIRE_CSS.as_bytes();
    let mut hash: u32 = 0x811c_9dc5;
    while let Some((byte, tail)) = rest.split_first() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        rest = tail;
    }
    hash
};

/// The stylesheet URL's `v` query: the crate version and a hash of the file.
pub(crate) fn asset_version() -> String {
    format!("{}-{CSS_HASH:08x}", env!("CARGO_PKG_VERSION"))
}

pub(crate) fn asset_routes(r: &mut smeltery_core::routing::Router) {
    r.get("/watchfire.css", stylesheet)
        .name("watchfire.stylesheet");
}

/// `GET /_watchfire/assets/watchfire.css`: cached for a year (the URL carries [`asset_version`]), like Sparks'
/// runtime. Anyone may fetch it (it holds no data), except that `WATCHFIRE_DASHBOARD=off` answers 404 like the
/// rest of the dashboard.
async fn stylesheet(app: App) -> Response {
    if super::settings(&app).dashboard == super::Access::Off {
        return smeltery_core::Error::not_found().into_response();
    }
    (
        [
            (http::header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                http::header::CACHE_CONTROL,
                "public, max-age=31536000, immutable",
            ),
        ],
        WATCHFIRE_CSS,
    )
        .into_response()
}

/// The buttons an agent in `state` offers; the destructive ones (stop, restart) ask first.
pub(crate) fn actions_for(state: &str) -> Vec<ActionButton> {
    let button = |name: &str, label: &str| ActionButton {
        name: name.to_owned(),
        label: label.to_owned(),
        danger: false,
        warning: String::new(),
    };
    let stop = ActionButton {
        danger: true,
        warning: "Its current run is cancelled and it stays stopped until started.".to_owned(),
        ..button("stop", "Stop")
    };
    let restart = ActionButton {
        danger: true,
        warning: "Its current run is cancelled and it starts again.".to_owned(),
        ..button("restart", "Restart")
    };
    match state {
        "running" | "starting" | "backing_off" => vec![button("pause", "Pause"), restart, stop],
        "paused" => vec![button("resume", "Resume"), restart, stop],
        "stopped" | "completed" | "failed" => vec![button("start", "Start")],
        "stopping" => Vec::new(),
        // `standby` and anything else: every command (the process holding the agent decides).
        _ => vec![
            button("start", "Start"),
            button("pause", "Pause"),
            button("resume", "Resume"),
            restart,
            stop,
        ],
    }
}

/// The counts of the summary tiles.
pub(crate) fn summarize(rows: &[AgentRow]) -> AgentSummary {
    let count = |states: &[&str]| {
        rows.iter()
            .filter(|r| states.contains(&r.state.as_str()))
            .count()
            .to_string()
    };
    AgentSummary {
        total: rows.len().to_string(),
        running: count(&["running", "starting"]),
        paused: count(&["paused"]),
        failing: count(&["failed", "backing_off"]),
        standby: count(&["standby"]),
    }
}

/// The schedule's earliest next run (`YYYY-MM-DD HH:MM:SS` sorts as text) and its task's name; empty when
/// nothing is scheduled.
pub(crate) fn next_up(schedule: &[ScheduleRow]) -> (String, String) {
    schedule
        .iter()
        .filter(|s| s.next != "never" && !s.next.is_empty())
        .min_by(|a, b| a.next.cmp(&b.next))
        .map(|s| (s.next.clone(), s.name.clone()))
        .unwrap_or_default()
}

fn env_kind(env: &str) -> &'static str {
    match env {
        "local" => "local",
        "production" => "production",
        _ => "other",
    }
}

fn duration(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// The heartbeat column: `Health` only says whether the last heartbeat arrived within the timeout.
fn health(h: Health) -> &'static str {
    match h {
        Health::Healthy => "on time",
        Health::Stalled => "stalled",
        _ => "-",
    }
}

/// Percent-encode everything but the unreserved characters, for a query value.
pub(crate) fn query_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A quoted argument of a Sparks action call: `sparks.js` reads `\x` as `x` inside quotes.
pub(crate) fn call_arg(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Mark where the runs of another process begin (run ids start again in every process, D-186).
fn mark_processes(runs: &mut [RunRow]) {
    let mut previous: Option<String> = None;
    for run in runs.iter_mut() {
        if let Some(prev) = &previous
            && *prev != run.process
            && !run.process.is_empty()
        {
            run.sep = format!("earlier runs, process {}", run.process);
        }
        previous = Some(run.process.clone());
    }
}

fn run_row(run: RunRecord) -> RunRow {
    RunRow {
        id: run.run_id.to_string(),
        outcome: run.outcome.as_str().to_owned(),
        started: format_utc(run.started_at_ms),
        duration: run
            .ended_at_ms
            .map_or_else(|| "-".to_owned(), |end| duration(end - run.started_at_ms)),
        job: run.job.unwrap_or_default(),
        error: run.error.unwrap_or_default(),
        counters: run
            .counters
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" "),
        process: run.process,
        sep: String::new(),
    }
}

async fn agent_row(app: &App, status: AgentStatus) -> AgentRow {
    let mut recent: Vec<RunRow> = crate::remote::runs_view(app, Some(&status.name), 5)
        .await
        .and_then(Result::ok)
        .unwrap_or_default()
        .into_iter()
        .map(run_row)
        .collect();
    mark_processes(&mut recent);
    let local = app
        .service::<Agents>()
        .and_then(|a| a.status(&status.name).ok())
        .is_some_and(|s| s.state != crate::status::AgentState::Standby);
    let place = match (&status.held_by, local) {
        (_, true) => String::new(),
        (Some(holder), false) => format!("runs in {holder}"),
        (None, false) => "no process holds it (its last recorded state)".to_owned(),
    };
    let backoff = match (status.backoff_ms, status.next_restart_at_ms) {
        (Some(ms), Some(at)) => format!("{} (restart at {})", duration(ms), format_utc(at)),
        _ => String::new(),
    };
    let state = status.state.as_str();
    AgentRow {
        path: status.name.replace('%', "%25").replace('#', "%23"),
        query: query_encode(&status.name),
        arg: call_arg(&status.name),
        state: state.to_owned(),
        state_label: state.replace('_', " "),
        actions: actions_for(state),
        confirming: String::new(),
        health: health(status.health).to_owned(),
        last_heartbeat: status.last_heartbeat_ms.map(format_utc).unwrap_or_default(),
        restarts: status.restarts.to_string(),
        runs: status.runs.to_string(),
        backoff,
        last_error: status.last_error.clone().unwrap_or_default(),
        name: status.name,
        recent,
        place,
    }
}

/// The agents panel's data: whether there are agents to show (here or in other processes), and a row per agent.
pub(crate) async fn load_agents(app: &App) -> (bool, Vec<AgentRow>) {
    let Some(statuses) = crate::remote::agent_view(app).await else {
        return (false, Vec::new());
    };
    let mut rows = Vec::new();
    for status in statuses {
        rows.push(agent_row(app, status).await);
    }
    (true, rows)
}

/// The queue panel's data: queue counts and dead letters (when there is a queue) and the schedule.
pub(crate) async fn load_queue(app: &App) -> (bool, QueueRow, Vec<DeadRow>, Vec<ScheduleRow>) {
    let schedule = crate::remote::schedule_view(app)
        .await
        .map(|infos| {
            infos
                .into_iter()
                .map(|s| ScheduleRow {
                    name: s.name,
                    kind: s.kind.to_owned(),
                    expression: s.expression,
                    next: s.next_run_ms.map_or_else(|| "never".to_owned(), format_utc),
                })
                .collect()
        })
        .unwrap_or_default();
    let Some(q) = app.service::<crate::queue::Queue>() else {
        return (false, QueueRow::default(), Vec::new(), schedule);
    };
    let stats = q.stats().await.unwrap_or_default();
    let dead = q
        .dead_letters(10)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|d| DeadRow {
            id: d.id.to_string(),
            job: d.job,
            attempts: d.attempts.to_string(),
            error: d.error,
            failed: format_utc(d.failed_at_ms),
        })
        .collect();
    let row = QueueRow {
        driver: q.driver().to_owned(),
        pending: stats.pending.to_string(),
        reserved: stats.reserved.to_string(),
        dead: stats.dead.to_string(),
    };
    (true, row, dead, schedule)
}

/// Collect the page data. With live panels the components load their own data.
///
/// `confirm` (agent, action) opens that agent's stop / restart confirmation when the agent shows that button; the page
/// is then rendered without the live panels and without the reload.
pub(crate) async fn page_data(app: &App, confirm: Option<(String, String)>) -> Page {
    let mut live = super::live::enabled(app);
    let running = app.service::<Agents>().is_some();
    let elsewhere = !running && crate::remote::remote_of(app).await.is_some();
    let mut confirming = false;
    let mut loaded = None;
    if let Some((agent, action)) = confirm {
        let mut agents = load_agents(app).await.1;
        if let Some(row) = agents
            .iter_mut()
            .find(|a| a.name == agent && a.actions.iter().any(|x| x.danger && x.name == action))
        {
            row.confirming = action;
            confirming = true;
            live = false;
        }
        loaded = Some(agents);
    }
    let (agents, (has_queue, queue, dead_letters, schedule)) = if live {
        (
            Vec::new(),
            (false, QueueRow::default(), Vec::new(), Vec::new()),
        )
    } else {
        let agents = match loaded {
            Some(agents) => agents,
            None => load_agents(app).await.1,
        };
        (agents, load_queue(app).await)
    };
    let env = app.settings().env.clone();
    let (next_run, next_name) = next_up(&schedule);
    Page {
        app_name: app.settings().name.clone(),
        env_kind: env_kind(&env).to_owned(),
        env,
        asset_version: asset_version(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        running,
        elsewhere,
        live,
        confirming,
        notice: String::new(),
        summary: summarize(&agents),
        agents,
        has_queue,
        queue,
        dead_letters,
        schedule,
        next_run,
        next_name,
    }
}

/// The page's query: `?agent=<name>&confirm=<stop|restart>` opens a confirmation without JavaScript.
#[derive(Deserialize)]
struct PageQuery {
    agent: Option<String>,
    confirm: Option<String>,
}

async fn page(
    _: DashboardAccess,
    app: App,
    query: Result<axum::extract::Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    // A query that does not decode (a repeated key, say) is ignored: the dashboard always renders.
    let confirm = query
        .ok()
        .and_then(|axum::extract::Query(q)| q.agent.zip(q.confirm));
    let mut response = smeltery_core::view::view(Compiled(page_data(&app, confirm).await));
    let headers = response.headers_mut();
    headers.insert(
        http::header::CACHE_CONTROL,
        http::HeaderValue::from_static("no-store"),
    );
    // The page holds one-click controls (and the confirmation page a destructive one at a known spot): no other
    // site may frame it.
    headers.insert(
        http::header::X_FRAME_OPTIONS,
        http::HeaderValue::from_static("DENY"),
    );
    headers.insert(
        http::header::CONTENT_SECURITY_POLICY,
        http::HeaderValue::from_static("frame-ancestors 'none'"),
    );
    response
}

async fn act(
    _: DashboardAccess,
    app: App,
    session: Session,
    Path((name, action)): Path<(String, String)>,
) -> Response {
    let message = crate::remote::control(&app, &name, &action)
        .await
        .notice(&name, &action);
    session.flash("status", message);
    Redirect::to(PREFIX).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHost;

    impl Host for TestHost {
        fn csrf_token(&self) -> Option<&str> {
            Some("tok123")
        }
        fn session(&self, key: &str) -> Option<String> {
            (key == "status").then(|| "worker: stopped.".to_owned())
        }
    }

    fn sample() -> Page {
        Page {
            app_name: "Demo <app>".into(),
            env: "local".into(),
            env_kind: "local".into(),
            asset_version: asset_version(),
            version: "0.1.0".into(),
            summary: AgentSummary {
                total: "1".into(),
                failing: "1".into(),
                ..AgentSummary::default()
            },
            next_run: "2026-10-03 10:05:00".into(),
            next_name: "cleanup".into(),
            running: true,
            elsewhere: false,
            live: false,
            confirming: false,
            notice: String::new(),
            agents: vec![AgentRow {
                name: "fetcher#0".into(),
                path: "fetcher%230".into(),
                query: query_encode("fetcher#0"),
                arg: call_arg("fetcher#0"),
                state: "backing_off".into(),
                state_label: "backing off".into(),
                actions: actions_for("backing_off"),
                confirming: String::new(),
                health: "-".into(),
                last_heartbeat: "2026-10-03 10:00:00".into(),
                restarts: "3".into(),
                runs: "4".into(),
                backoff: "1.5s (restart at 2026-10-03 10:00:02)".into(),
                last_error: "boom & bust".into(),
                recent: vec![RunRow {
                    id: "4".into(),
                    outcome: "failed".into(),
                    started: "2026-10-03 10:00:00".into(),
                    duration: "12ms".into(),
                    job: String::new(),
                    error: "boom & bust".into(),
                    counters: "pages=3".into(),
                    process: "host:1-ab".into(),
                    sep: String::new(),
                }],
                place: "runs in host:1-ab".into(),
            }],
            has_queue: true,
            queue: QueueRow {
                driver: "memory".into(),
                pending: "1".into(),
                reserved: "0".into(),
                dead: "1".into(),
            },
            dead_letters: vec![DeadRow {
                id: "1".into(),
                job: "send".into(),
                attempts: "3".into(),
                error: "no".into(),
                failed: "2026-10-03 10:00:00".into(),
            }],
            schedule: vec![ScheduleRow {
                name: "cleanup".into(),
                kind: "call".into(),
                expression: "every 5m".into(),
                next: "2026-10-03 10:05:00".into(),
            }],
        }
    }

    #[test]
    fn runtime_and_compiled_rendering_are_byte_identical() {
        let page = sample();
        let compiled = page.render_compiled(&TestHost).unwrap();
        // The derive's own engine reads `views/` of this crate.
        let runtime = page.render_runtime(&TestHost).unwrap();
        assert_eq!(runtime, compiled);
        assert!(compiled.contains(r#"<meta http-equiv="refresh" content="5">"#));
        assert!(compiled.contains("Demo &lt;app&gt;"));
        assert!(compiled.contains("boom &amp; bust"));
        assert!(compiled.contains(r#"action="/_watchfire/agents/fetcher%230/pause""#));
        assert!(compiled.contains(r#"name="_token" value="tok123""#));
        assert!(compiled.contains("worker: stopped."));
        assert!(!compiled.contains("<script"));
        assert!(
            !compiled.contains("http://") && !compiled.contains("https://"),
            "no CDN"
        );
        // Its own stylesheet, versioned by content; no inline styles without Sparks.
        assert!(compiled.contains(&format!(
            r#"<link rel="stylesheet" href="/_watchfire/assets/watchfire.css?v={}">"#,
            asset_version()
        )));
        assert!(!compiled.contains("<style"));
        // The header, the tiles, the state pill.
        assert!(compiled.contains(r#"<h1 class="wf-title">Watchfire</h1>"#));
        assert!(
            compiled
                .contains(r#"<span class="wf-env" data-env="local" title="APP_ENV">local</span>"#)
        );
        assert!(
            compiled.contains(
                r#"<dt>Failed or backing off</dt><dd><span class="wf-tile-value">1</span>"#
            )
        );
        assert!(
            compiled
                .contains(r#"<span class="wf-pill" data-state="backing_off">backing off</span>"#)
        );
        assert!(
            compiled
                .contains(r#"<span class="wf-tile-value wf-tile-time">2026-10-03 10:05:00</span>"#)
        );
        // Stop and restart ask first: the form sits inside a closed confirmation.
        // Without JavaScript it is a link to the page with the confirmation open; with Sparks, `ask`.
        assert!(!compiled.contains(r#"action="/_watchfire/agents/fetcher%230/stop""#));
        assert!(compiled.contains(
            r#"href="/_watchfire?agent=fetcher%230&amp;confirm=stop#wf-confirm" aria-label="Stop fetcher#0""#
        ));
        assert!(!compiled.contains("<details") && !compiled.contains("wf-confirm-box"));
        assert!(
            compiled.contains(
                r#"wire:click.prevent="ask(&#x27;fetcher#0&#x27;, &#x27;restart&#x27;)""#
            ) || compiled.contains(r#"wire:click.prevent="ask('fetcher#0', 'restart')""#)
        );
        assert!(
            !compiled.contains("/fetcher%230/start\""),
            "no start for a backing-off agent"
        );
        // The wrapper renders the compiled code even when handed another engine.
        let other = Engine::new(std::env::temp_dir());
        assert_eq!(
            Compiled(sample())
                .render_runtime_with(&other, &TestHost)
                .unwrap(),
            compiled
        );
        let empty = Page {
            agents: Vec::new(),
            running: false,
            has_queue: false,
            dead_letters: Vec::new(),
            schedule: Vec::new(),
            ..sample()
        };
        assert_eq!(
            empty.render_runtime(&TestHost).unwrap(),
            empty.render_compiled(&TestHost).unwrap()
        );
        let empty = empty.render_compiled(&TestHost).unwrap();
        assert!(empty.contains(r#"<p class="wf-empty-title">No agents</p>"#));
        assert!(empty.contains("Nothing scheduled."));
        assert!(empty.contains("Watchfire is not running in this process"));
        // An open confirmation, a run elsewhere, a production badge: still the same bytes in both modes.
        let mut other = sample();
        other.elsewhere = true;
        other.running = false;
        other.env = "production".into();
        other.env_kind = "production".into();
        other.dead_letters.clear();
        other.agents[0].confirming = "stop".into();
        other.confirming = true;
        let compiled = other.render_compiled(&TestHost).unwrap();
        assert_eq!(other.render_runtime(&TestHost).unwrap(), compiled);
        // The confirmation holds the real form; the page does not reload itself meanwhile.
        assert!(compiled.contains(r#"<div class="wf-confirm-box" id="wf-confirm" tabindex="-1""#));
        assert!(compiled.contains(r#"action="/_watchfire/agents/fetcher%230/stop""#));
        assert!(compiled.contains(
            r#"<a class="wf-btn" href="/_watchfire" wire:click.prevent="cancel" autofocus>Cancel</a>"#
        ));
        assert!(!compiled.contains("http-equiv=\"refresh\""));
        assert!(compiled.contains("Paused for a confirmation"));
        assert_eq!(compiled.matches("wf-confirm-box").count(), 1);
        // Focus lands on Cancel, never on the destructive button.
        assert_eq!(compiled.matches("autofocus").count(), 1);
        assert!(!compiled.contains("wf-btn-danger\" autofocus"));
        assert!(compiled.contains(r#"data-env="production""#));
        assert!(compiled.contains("No dead letters."));
        assert!(compiled.contains("Watchfire does not run in this process"));
    }

    #[test]
    fn values_from_agents_and_jobs_are_escaped_everywhere() {
        let bad = r#"<img src=x onerror=alert(1)>"'><svg onload=alert(2)>"#;
        let mut page = sample();
        let row = &mut page.agents[0];
        row.name = bad.into();
        row.path = bad.into();
        row.query = query_encode(bad);
        row.arg = call_arg(bad);
        row.last_error = bad.into();
        row.place = bad.into();
        row.recent[0].error = bad.into();
        row.recent[0].job = bad.into();
        row.recent[0].counters = bad.into();
        row.recent[0].sep = bad.into();
        page.dead_letters[0].job = bad.into();
        page.dead_letters[0].error = bad.into();
        page.schedule[0].name = bad.into();
        page.schedule[0].expression = bad.into();
        page.next_name = bad.into();
        page.app_name = bad.into();
        let compiled = page.render_compiled(&TestHost).unwrap();
        assert_eq!(page.render_runtime(&TestHost).unwrap(), compiled);
        let mut confirming = page;
        confirming.agents[0].confirming = "stop".into();
        confirming.confirming = true;
        let open = confirming.render_compiled(&TestHost).unwrap();
        assert_eq!(confirming.render_runtime(&TestHost).unwrap(), open);
        for html in [&compiled, &open] {
            assert!(
                !html.contains("<img") && !html.contains("<svg onload"),
                "{html}"
            );
            assert!(!html.contains(r#""'>"#));
            assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
        }
    }

    #[test]
    fn names_are_encoded_for_queries_and_action_arguments() {
        assert_eq!(query_encode("fetcher#0"), "fetcher%230");
        assert_eq!(query_encode("a&b=c d/é"), "a%26b%3Dc%20d%2F%C3%A9");
        assert_eq!(query_encode("ok-_.~9"), "ok-_.~9");
        assert_eq!(call_arg(r"it's \ fine"), r"it\'s \\ fine");
    }

    #[test]
    fn runs_of_another_process_are_marked() {
        let run = |id: &str, process: &str| RunRow {
            id: id.into(),
            process: process.into(),
            ..RunRow::default()
        };
        let mut runs = vec![
            run("4", "b"),
            run("3", "b"),
            run("18", "a"),
            run("17", "a"),
            run("2", ""),
        ];
        mark_processes(&mut runs);
        let seps: Vec<&str> = runs.iter().map(|r| r.sep.as_str()).collect();
        assert_eq!(seps, ["", "", "earlier runs, process a", "", ""]);
    }

    #[test]
    fn each_state_offers_its_buttons_and_the_destructive_ones_ask() {
        let names = |state: &str| {
            actions_for(state)
                .into_iter()
                .map(|a| (a.name, a.danger))
                .collect::<Vec<_>>()
        };
        let s = |n: &str, d: bool| (n.to_owned(), d);
        assert_eq!(
            names("running"),
            [s("pause", false), s("restart", true), s("stop", true)]
        );
        assert_eq!(names("backing_off"), names("running"));
        assert_eq!(
            names("paused"),
            [s("resume", false), s("restart", true), s("stop", true)]
        );
        for state in ["stopped", "completed", "failed"] {
            assert_eq!(names(state), [s("start", false)], "{state}");
        }
        assert!(names("stopping").is_empty());
        assert_eq!(names("standby").len(), crate::remote::ACTIONS.len());
        for a in actions_for("standby") {
            assert!(crate::remote::ACTIONS.contains(&a.name.as_str()));
            assert_eq!(a.danger, !a.warning.is_empty());
        }
    }

    #[test]
    fn summary_tiles_count_states_and_find_the_next_run() {
        let row = |state: &str| AgentRow {
            state: state.into(),
            ..AgentRow::default()
        };
        let rows = [
            row("running"),
            row("starting"),
            row("paused"),
            row("failed"),
            row("backing_off"),
            row("standby"),
            row("stopped"),
        ];
        let s = summarize(&rows);
        assert_eq!(
            [s.total, s.running, s.paused, s.failing, s.standby],
            ["7", "2", "1", "2", "1"]
        );
        let task = |name: &str, next: &str| ScheduleRow {
            name: name.into(),
            next: next.into(),
            ..ScheduleRow::default()
        };
        assert_eq!(
            next_up(&[
                task("never", "never"),
                task("late", "2026-10-04 03:00:00"),
                task("soon", "2026-10-03 23:59:59"),
            ]),
            ("2026-10-03 23:59:59".to_owned(), "soon".to_owned())
        );
        assert_eq!(
            next_up(&[task("x", "never")]),
            (String::new(), String::new())
        );
    }

    #[test]
    fn the_stylesheet_stands_alone() {
        // Nothing from outside: no imports, no remote URLs, no web fonts.
        for needle in ["@import", "url(", "http://", "https://", "@font-face"] {
            assert!(!WATCHFIRE_CSS.contains(needle), "{needle}");
        }
        for needle in [
            "prefers-color-scheme: dark",
            "prefers-reduced-motion: reduce",
            ":focus-visible",
            "--wf-molten-700: #c72e16",
            r#".wf-ask::after {
  content: "\2026";"#,
        ] {
            assert!(WATCHFIRE_CSS.contains(needle), "{needle}");
        }
        let v = asset_version();
        assert!(v.starts_with(concat!(env!("CARGO_PKG_VERSION"), "-")));
        assert_eq!(v.len(), env!("CARGO_PKG_VERSION").len() + 9);
    }
}
