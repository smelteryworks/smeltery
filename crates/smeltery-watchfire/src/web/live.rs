//! The live dashboard: two Spark components (`watchfire.agents`, `watchfire.queue`), registered when the app
//! installs both Watchfire and Sparks, and refreshed by Watchfire's status changes through `Broadcast`.

use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smeltery_core::{App, BoxFuture, Error, Result};
use smeltery_mold::{Engine, Host, Template};
use smeltery_sparks::{ActionInfo, Actions, Broadcast, Spark, SparkCtx, UploadRule};
use tokio::sync::broadcast::error::RecvError;

use super::Access;
use super::dashboard::{
    AgentRow, AgentSummary, DeadRow, QueueRow, ScheduleRow, load_agents, load_queue, next_up,
    summarize,
};
use crate::runtime::Agents;

/// The agents panel's component name.
pub const AGENTS: &str = "watchfire.agents";
/// The queue and schedule panel's component name.
pub const QUEUE: &str = "watchfire.queue";
/// At most one refresh of the agents panel per this interval, however busy the supervisor is.
pub(crate) const THROTTLE: Duration = Duration::from_millis(500);
/// The queue panel refreshes on this interval while a page is connected (the queue has no change events).
pub(crate) const QUEUE_EVERY: Duration = Duration::from_secs(5);

/// The marker service: the live panels are registered on this app.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Live;

/// Whether the dashboard's panels are live components on this app.
pub(crate) fn enabled(app: &App) -> bool {
    app.service::<Live>().is_some()
}

/// Register the panels when the app installed Sparks (from Watchfire's boot hook).
pub(crate) fn register(app: &App) -> Result<()> {
    if smeltery_sparks::extend(app, |s| {
        s.add::<AgentsPanel>();
        s.add::<QueuePanel>();
    })? {
        app.insert_service(Live);
    }
    Ok(())
}

/// The dashboard's access rule ([`Access`]) for a component request (the update endpoint is a web route, so the
/// visitor's sign-in, session and address come with it): `off` answers 404; the local-development rule needs local
/// mode, a loopback address and the mark the page set in this browser's session after checking the request's
/// headers; otherwise a guest gets 401 and a signed-in user the gate does not admit 403.
async fn gate(ctx: &SparkCtx) -> Result<()> {
    let app = ctx.app();
    if super::settings(app).dashboard == Access::Off {
        return Err(Error::not_found());
    }
    let auth = ctx.auth();
    let loopback = auth
        .and_then(|a| a.ip().parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    let marked = ctx
        .session()
        .and_then(|s| s.get::<bool>(super::LOCAL_MARK))
        .unwrap_or(false);
    if super::local_mode(app) && loopback && marked {
        return Ok(());
    }
    match auth.filter(|a| a.check()) {
        None => Err(Error::unauthorized()),
        Some(auth) => {
            if super::gate_allows(app, auth).await? {
                Ok(())
            } else {
                Err(Error::forbidden())
            }
        }
    }
}

/// The live agents table. State: the rows it shows and their counts (reloaded before every render), the last
/// action's notice, and the stop / restart waiting for a confirmation (kept in the state, so a refresh pushed
/// meanwhile keeps it open).
#[derive(Serialize, Deserialize, Default, smeltery_mold_macros::Mold)]
#[mold("watchfire/agents", crate = "smeltery_mold", dir = "views")]
pub(crate) struct AgentsPanel {
    pub(crate) agents: Vec<AgentRow>,
    pub(crate) summary: AgentSummary,
    pub(crate) notice: String,
    #[serde(default)]
    pub(crate) confirm_agent: String,
    #[serde(default)]
    pub(crate) confirm_action: String,
}

/// The live queue and schedule panel.
#[derive(Serialize, Deserialize, Default, smeltery_mold_macros::Mold)]
#[mold("watchfire/queue", crate = "smeltery_mold", dir = "views")]
pub(crate) struct QueuePanel {
    pub(crate) has_queue: bool,
    pub(crate) queue: QueueRow,
    pub(crate) dead_letters: Vec<DeadRow>,
    pub(crate) schedule: Vec<ScheduleRow>,
    pub(crate) next_run: String,
    pub(crate) next_name: String,
}

// The panels render the template compiled into this crate in every build mode (the engine Sparks passes reads
// the app's own views, where these templates are not; D-047).
impl Spark for AgentsPanel {
    const NAME: &'static str = AGENTS;
    const MODEL: &'static [&'static str] = &[];
    const UPLOADS: &'static [UploadRule] = &[];
    const STREAM: bool = true;

    fn render_view(
        &self,
        _engine: &Engine,
        host: &dyn Host,
    ) -> std::result::Result<String, smeltery_mold::Error> {
        self.render_compiled(host)
    }
}

impl Spark for QueuePanel {
    const NAME: &'static str = QUEUE;
    const MODEL: &'static [&'static str] = &[];
    const UPLOADS: &'static [UploadRule] = &[];
    const STREAM: bool = true;

    fn render_view(
        &self,
        _engine: &Engine,
        host: &dyn Host,
    ) -> std::result::Result<String, smeltery_mold::Error> {
        self.render_compiled(host)
    }
}

impl AgentsPanel {
    async fn act(&mut self, ctx: &SparkCtx, name: String, action: String) -> Result<()> {
        gate(ctx).await?;
        let outcome = crate::remote::control(ctx.app(), &name, &action).await;
        match outcome {
            crate::remote::Control::UnknownAction => {
                return Err(Error::bad_request(format!("unknown action `{action}`")));
            }
            crate::remote::Control::NotRunning => {
                return Err(Error::http(
                    http::StatusCode::SERVICE_UNAVAILABLE,
                    "Watchfire is not running in this process",
                ));
            }
            other => self.notice = other.notice(&name, &action),
        }
        self.confirm_agent.clear();
        self.confirm_action.clear();
        Ok(())
    }

    /// Open the confirmation of a destructive action (stop, restart) for one agent; asking again closes it.
    async fn ask(&mut self, ctx: &SparkCtx, name: String, action: String) -> Result<()> {
        gate(ctx).await?;
        if !matches!(action.as_str(), "stop" | "restart") {
            return Err(Error::bad_request(format!(
                "`{action}` needs no confirmation"
            )));
        }
        if self.confirm_agent == name && self.confirm_action == action {
            self.confirm_agent.clear();
            self.confirm_action.clear();
        } else {
            self.confirm_agent = name;
            self.confirm_action = action;
        }
        Ok(())
    }

    /// Close the open confirmation.
    async fn cancel(&mut self, ctx: &SparkCtx) -> Result<()> {
        gate(ctx).await?;
        self.confirm_agent.clear();
        self.confirm_action.clear();
        Ok(())
    }
}

impl Actions for AgentsPanel {
    const ACTIONS: &'static [ActionInfo] = &[
        ActionInfo::new("act", &[]),
        ActionInfo::new("ask", &[]),
        ActionInfo::new("cancel", &[]),
    ];

    fn call<'a>(
        &'a mut self,
        method: &'a str,
        params: Vec<serde_json::Value>,
        ctx: &'a mut SparkCtx,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match method {
                "act" => {
                    smeltery_sparks::__private::param_count(&params, 2, method)?;
                    let name: String = smeltery_sparks::__private::param(&params, 0, method)?;
                    let action: String = smeltery_sparks::__private::param(&params, 1, method)?;
                    self.act(ctx, name, action).await
                }
                "ask" => {
                    smeltery_sparks::__private::param_count(&params, 2, method)?;
                    let name: String = smeltery_sparks::__private::param(&params, 0, method)?;
                    let action: String = smeltery_sparks::__private::param(&params, 1, method)?;
                    self.ask(ctx, name, action).await
                }
                "cancel" => {
                    smeltery_sparks::__private::param_count(&params, 0, method)?;
                    self.cancel(ctx).await
                }
                _ => Err(smeltery_sparks::__private::unknown_action(method)),
            }
        })
    }

    /// A stream token (the page's subscription to pushed refreshes) only for a viewer the dashboard gate admits, the
    /// same rule as every update: a guest or a refused user learns nothing, not even when agents change.
    fn stream_hook<'a>(&'a mut self, ctx: &'a mut SparkCtx) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move { Ok(gate(ctx).await.is_ok()) })
    }

    fn rendering_hook<'a>(&'a mut self, ctx: &'a mut SparkCtx) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            gate(ctx).await?;
            self.agents = load_agents(ctx.app()).await.1;
            self.summary = summarize(&self.agents);
            // The open confirmation follows its agent; it closes when the agent or the button is gone.
            let open = self.agents.iter_mut().find(|a| {
                a.name == self.confirm_agent
                    && a.actions.iter().any(|x| x.name == self.confirm_action)
            });
            match open {
                Some(row) => row.confirming.clone_from(&self.confirm_action),
                None => {
                    self.confirm_agent.clear();
                    self.confirm_action.clear();
                }
            }
            Ok(())
        })
    }
}

impl Actions for QueuePanel {
    const ACTIONS: &'static [ActionInfo] = &[];

    fn call<'a>(
        &'a mut self,
        method: &'a str,
        _params: Vec<serde_json::Value>,
        _ctx: &'a mut SparkCtx,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Err(smeltery_sparks::__private::unknown_action(method)) })
    }

    /// A stream token (the page's subscription to pushed refreshes) only for a viewer the dashboard gate admits, the
    /// same rule as every update: a guest or a refused user learns nothing, not even when agents change.
    fn stream_hook<'a>(&'a mut self, ctx: &'a mut SparkCtx) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move { Ok(gate(ctx).await.is_ok()) })
    }

    fn rendering_hook<'a>(&'a mut self, ctx: &'a mut SparkCtx) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            gate(ctx).await?;
            let (has_queue, queue, dead_letters, schedule) = load_queue(ctx.app()).await;
            self.has_queue = has_queue;
            self.queue = queue;
            self.dead_letters = dead_letters;
            (self.next_run, self.next_name) = next_up(&schedule);
            self.schedule = schedule;
            Ok(())
        })
    }
}

/// Push refreshes to open dashboards: the agents panel at most once per [`THROTTLE`] after status changes, the
/// queue panel every [`QUEUE_EVERY`]; only while a page is connected. Owned by Watchfire's task set; ends on
/// shutdown.
pub(crate) async fn push(agents: Agents, broadcast: Broadcast) {
    let mut statuses = agents.subscribe();
    let shutdown = agents.shutdown_token();
    let mut tick = tokio::time::interval(THROTTLE);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let every = u32::try_from(QUEUE_EVERY.as_millis() / THROTTLE.as_millis())
        .unwrap_or(10)
        .max(1);
    let mut ticks = 0_u32;
    let mut dirty = false;
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            changed = statuses.recv() => match changed {
                Ok(_) | Err(RecvError::Lagged(_)) => dirty = true,
                Err(RecvError::Closed) => break,
            },
            _ = tick.tick() => {
                ticks = ticks.wrapping_add(1);
                if broadcast.streams() == 0 {
                    dirty = false;
                    continue;
                }
                if std::mem::take(&mut dirty) {
                    broadcast.to(AGENTS).refresh();
                }
                if ticks.is_multiple_of(every) {
                    broadcast.to(QUEUE).refresh();
                }
            }
        }
    }
}

/// How often the panels check the shared tables for changes made by other processes (while a page is connected).
pub(crate) const SHARED_EVERY: Duration = Duration::from_secs(2);

/// The task watching the shared tables, owned by the app (it ends with the app's shutdown token).
pub(crate) struct SharedWatch(#[allow(dead_code)] tokio::task::JoinHandle<()>);

impl Drop for SharedWatch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Refresh the agents panel when the shared tables change (agents in other processes), every [`SHARED_EVERY`] while
/// a page is connected.
pub(crate) fn watch_shared(app: &App) {
    if !enabled(app) {
        return;
    }
    let Some(broadcast) = Broadcast::of(app) else {
        return;
    };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let weak = app.downgrade();
    let shutdown = app.shutdown_token().clone();
    let task = runtime.spawn(async move {
        let mut last = None;
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(SHARED_EVERY) => {}
            }
            if broadcast.streams() == 0 {
                continue;
            }
            let Some(app) = weak.upgrade() else { return };
            let remote = crate::remote::remote_of(&app).await;
            drop(app);
            let Some(remote) = remote else { continue };
            let seen = remote.fingerprint().await;
            if last.is_some_and(|l| l != seen) {
                broadcast.to(AGENTS).refresh();
                broadcast.to(QUEUE).refresh();
            }
            last = Some(seen);
        }
    });
    app.insert_service(SharedWatch(task));
}

/// Start [`push`] when the panels are live.
pub(crate) fn start(app: &App, agents: &Agents) {
    if !enabled(app) {
        return;
    }
    if let Some(broadcast) = Broadcast::of(app) {
        agents.spawn_owned(push(agents.clone(), broadcast));
    }
}

#[cfg(test)]
mod tests {
    use super::super::dashboard::actions_for;
    use super::*;

    struct TestHost;

    impl Host for TestHost {
        fn csrf_token(&self) -> Option<&str> {
            Some("tok")
        }
    }

    #[test]
    fn panels_render_identically_in_both_modes() {
        let agents = AgentsPanel {
            agents: vec![AgentRow {
                name: "fetcher#0".into(),
                path: "fetcher%230".into(),
                state: "running".into(),
                arg: "fetcher#0".into(),
                actions: actions_for("running"),
                health: "on time".into(),
                restarts: "0".into(),
                runs: "1".into(),
                ..AgentRow::default()
            }],
            notice: "worker: stopped.".into(),
            ..AgentsPanel::default()
        };
        let compiled = agents.render_compiled(&TestHost).unwrap();
        assert_eq!(agents.render_runtime(&TestHost).unwrap(), compiled);
        assert!(
            compiled.contains(r#"wire:submit="act(&#x27;fetcher#0&#x27;, &#x27;pause&#x27;)""#)
                || compiled.contains(r#"wire:submit="act('fetcher#0', 'pause')""#),
            "{compiled}"
        );
        // Stop asks first: a link to the confirmation, which the live panel opens with `ask`.
        assert!(
            compiled
                .contains(r#"wire:click.prevent="ask(&#x27;fetcher#0&#x27;, &#x27;stop&#x27;)""#)
                || compiled.contains(r#"wire:click.prevent="ask('fetcher#0', 'stop')""#),
            "{compiled}"
        );
        assert!(compiled.contains("worker: stopped."));
        // The heartbeat column says what `Health` measures.
        assert!(compiled.contains(">Heartbeat</th>"));
        assert!(compiled.contains(r#"data-health="on time""#));
        let queue = QueuePanel {
            has_queue: true,
            queue: QueueRow {
                driver: "memory".into(),
                pending: "2".into(),
                ..QueueRow::default()
            },
            ..QueuePanel::default()
        };
        assert_eq!(
            queue.render_runtime(&TestHost).unwrap(),
            queue.render_compiled(&TestHost).unwrap()
        );
    }

    fn row(name: &str, state: &str) -> AgentRow {
        AgentRow {
            name: name.into(),
            path: name.replace('#', "%23"),
            query: super::super::dashboard::query_encode(name),
            arg: super::super::dashboard::call_arg(name),
            state: state.into(),
            state_label: state.replace('_', " "),
            actions: actions_for(state),
            health: "on time".into(),
            restarts: "0".into(),
            runs: "1".into(),
            ..AgentRow::default()
        }
    }

    fn panel(agents: Vec<AgentRow>, notice: &str) -> String {
        let p = AgentsPanel {
            summary: summarize(&agents),
            agents,
            notice: notice.into(),
            ..AgentsPanel::default()
        };
        let html = p.render_compiled(&TestHost).unwrap();
        assert_eq!(p.render_runtime(&TestHost).unwrap(), html);
        // The root element Sparks puts around a component's view.
        // (The stream token only matters to the stream URL, not to the morph.)
        format!(
            r#"<div wire:id="a1" wire:name="watchfire.agents" wire:stream="test-token">{html}</div>"#
        )
    }

    /// A pushed refresh replaces the panel through the Sparks runtime's morph (`sparks.js`): the new markup must
    /// come out exactly as rendered, with each agent's rows updated in place (keyed by `wire:key`), the confirmation
    /// opened by the server's `open` and the summary counts changed. Needs Node and the workspace's
    /// `smeltery-sparks/js` (SKIPPED otherwise, like the Sparks runtime's own Node tests).
    #[test]
    fn a_refresh_morphs_the_panel_in_place() {
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let sparks = crate_dir.join("../smeltery-sparks/js");
        if !sparks.join("sparks.js").is_file() || !sparks.join("test/dom.mjs").is_file() {
            eprintln!("SKIPPED: smeltery-sparks/js is not next to this crate");
            return;
        }
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("SKIPPED: Node is not installed");
            return;
        }
        let before = panel(
            vec![row("worker", "running"), row("fetcher#0", "running")],
            "",
        );
        let mut confirming = row("fetcher#0", "running");
        confirming.confirming = "stop".into();
        confirming.last_heartbeat = "2026-10-04 10:00:00".into();
        let after = panel(
            vec![
                confirming,
                row("worker", "stopped"),
                row("late", "standby"),
                row(r"it's a \ test", "running"),
            ],
            "worker: stopped.",
        );
        assert!(after.contains(r#"<div class="wf-confirm-box" id="wf-confirm" tabindex="-1""#));
        assert!(!before.contains("wf-confirm-box"));
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (
            dir.path().join("before.html"),
            dir.path().join("after.html"),
        );
        std::fs::write(&a, &before).unwrap();
        std::fs::write(&b, &after).unwrap();
        let run = std::process::Command::new("node")
            .arg(crate_dir.join("tests/js/panel_morph.mjs"))
            .arg(&sparks)
            .arg(&a)
            .arg(&b)
            .arg("agent-fetcher#0")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    }
}
