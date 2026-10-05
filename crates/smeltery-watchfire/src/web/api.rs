//! The JSON API and the SSE stream under `/_watchfire/api`.

use std::convert::Infallible;

use axum::Json;
use axum::extract::{Path, Query};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use smeltery_core::App;
use tokio::sync::broadcast::error::RecvError;

use super::{ApiAccess, json_error};
use crate::error::Error;
use crate::queue::{DeadLetter, Queue};
use crate::runtime::Agents;
use crate::status::{AgentStatus, RunRecord};

/// Every route once, for the app's router and the headless one.
macro_rules! each_route {
    ($add:ident, $target:ident) => {
        $add!($target, get, "/agents", list_agents);
        $add!($target, get, "/agents/{name}", agent_detail);
        $add!($target, get, "/agents/{name}/logs", agent_logs);
        $add!($target, post, "/agents/{name}/{action}", agent_command);
        $add!($target, get, "/runs", runs);
        $add!($target, get, "/jobs", jobs);
        $add!($target, post, "/jobs/dead/{id}/retry", retry_dead);
        $add!($target, delete, "/jobs/dead/{id}", delete_dead);
        $add!($target, get, "/schedule", schedule);
        $add!($target, get, "/events", events);
    };
}

/// The routes on the app's router (`api_routes_at("/_watchfire/api", …)`).
pub(crate) fn routes(r: &mut smeltery_core::routing::Router) {
    macro_rules! add {
        ($r:ident, $method:ident, $path:literal, $handler:ident) => {
            $r.$method($path, $handler);
        };
    }
    each_route!(add, r);
}

/// The same routes as an Axum router (for the headless API server).
pub(crate) fn axum_router() -> axum::Router<App> {
    let mut router = axum::Router::new();
    macro_rules! add {
        ($r:ident, $method:ident, $path:literal, $handler:ident) => {
            $r = $r.route($path, axum::routing::$method($handler));
        };
    }
    each_route!(add, router);
    router
}

fn agents(app: &App) -> Option<Agents> {
    app.service::<Agents>().map(|a| (*a).clone())
}

fn queue(app: &App) -> Option<Queue> {
    app.service::<Queue>().map(|q| (*q).clone())
}

fn unavailable() -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Watchfire is not running in this process",
    )
}

fn no_queue() -> Response {
    json_error(StatusCode::SERVICE_UNAVAILABLE, "no queue is set up")
}

/// An [`Error`] as an API answer: 404 unknown, 409 conflicting state, 503 shutting down,
/// 500 otherwise.
pub(crate) fn error_response(error: &Error) -> Response {
    let status = status_of(error);
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::error!(error = %error, "Watchfire API error");
    }
    json_error(status, error.to_string())
}

/// The HTTP status for an [`Error`].
pub(crate) fn status_of(error: &Error) -> StatusCode {
    match error {
        Error::UnknownAgent { .. } => StatusCode::NOT_FOUND,
        Error::AlreadyRunning { .. }
        | Error::NotRunning { .. }
        | Error::Paused { .. }
        | Error::NotPaused { .. }
        | Error::Standby { .. }
        | Error::Duplicate { .. } => StatusCode::CONFLICT,
        Error::ShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn result<T: Serialize>(value: Result<T, Error>) -> Response {
    match value {
        Ok(value) => Json(value).into_response(),
        Err(e) => error_response(&e),
    }
}

async fn list_agents(_: ApiAccess, app: App) -> Response {
    match crate::remote::agent_view(&app).await {
        Some(list) => Json(list).into_response(),
        None => unavailable(),
    }
}

/// `GET /api/agents/{name}`.
#[derive(Serialize)]
struct Detail {
    status: AgentStatus,
    runs: Vec<RunRecord>,
}

async fn agent_detail(_: ApiAccess, app: App, Path(name): Path<String>) -> Response {
    let Some(list) = crate::remote::agent_view(&app).await else {
        return unavailable();
    };
    let Some(status) = list.into_iter().find(|s| s.name == name) else {
        return error_response(&Error::UnknownAgent { name });
    };
    let runs = crate::remote::runs_view(&app, Some(&name), 20)
        .await
        .unwrap_or_else(|| Ok(Vec::new()));
    result(runs.map(|runs| Detail { status, runs }))
}

/// `GET /api/agents/{name}/logs`: log lines live in the memory of the process that runs the agent; from another
/// process they are read through the commands table.
async fn agent_logs(_: ApiAccess, app: App, Path(name): Path<String>) -> Response {
    match crate::remote::logs_view(&app, &name).await {
        Some(Ok(lines)) => Json(lines).into_response(),
        Some(Err((code, message))) => json_error(
            StatusCode::from_u16(code).unwrap_or(StatusCode::CONFLICT),
            message,
        ),
        None => unavailable(),
    }
}

async fn agent_command(
    _: ApiAccess,
    app: App,
    Path((name, action)): Path<(String, String)>,
) -> Response {
    use crate::remote::Control;
    match crate::remote::control(&app, &name, &action).await {
        Control::UnknownAction => json_error(
            StatusCode::NOT_FOUND,
            format!("unknown action `{action}`: use start, stop, pause, resume or restart"),
        ),
        Control::NotRunning => unavailable(),
        Control::Local(outcome) => result(outcome),
        Control::Remote(Ok(status)) => Json(status).into_response(),
        Control::Remote(Err((code, e))) => json_error(
            StatusCode::from_u16(code).unwrap_or(StatusCode::CONFLICT),
            e,
        ),
        // Taken: the holder is carrying it out (a slow stop); accepted, the outcome follows in the agent's state.
        taken @ Control::Taken(_) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "message": taken.notice(&name, &action) })),
        )
            .into_response(),
        queued @ Control::Queued => {
            json_error(StatusCode::GATEWAY_TIMEOUT, queued.notice(&name, &action))
        }
        full @ Control::QueueFull(_) => {
            json_error(StatusCode::TOO_MANY_REQUESTS, full.notice(&name, &action))
        }
        failed @ Control::StoreError(_) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            failed.notice(&name, &action),
        ),
    }
}

#[derive(Deserialize)]
struct RunsQuery {
    agent: Option<String>,
    limit: Option<u32>,
}

async fn runs(_: ApiAccess, app: App, Query(q): Query<RunsQuery>) -> Response {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let agent = q.agent.filter(|a| !a.is_empty());
    match crate::remote::runs_view(&app, agent.as_deref(), limit).await {
        Some(runs) => result(runs),
        None => unavailable(),
    }
}

/// `GET /api/jobs`.
#[derive(Serialize)]
struct Jobs {
    driver: &'static str,
    pending: u64,
    reserved: u64,
    dead: u64,
    dead_letters: Vec<DeadLetter>,
}

async fn jobs(_: ApiAccess, app: App) -> Response {
    let queue = match queue(&app) {
        Some(q) => q,
        None => return no_queue(),
    };
    let stats = match queue.stats().await {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };
    result(queue.dead_letters(50).await.map(|dead_letters| Jobs {
        driver: queue.driver(),
        pending: stats.pending,
        reserved: stats.reserved,
        dead: stats.dead,
        dead_letters,
    }))
}

async fn retry_dead(_: ApiAccess, app: App, Path(id): Path<i64>) -> Response {
    let queue = match queue(&app) {
        Some(q) => q,
        None => return no_queue(),
    };
    match queue.retry_dead(id).await {
        Ok(Some(job_id)) => Json(serde_json::json!({ "job_id": job_id })).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, format!("no dead letter #{id}")),
        Err(e) => error_response(&e),
    }
}

async fn delete_dead(_: ApiAccess, app: App, Path(id): Path<i64>) -> Response {
    let queue = match queue(&app) {
        Some(q) => q,
        None => return no_queue(),
    };
    match queue.delete_dead(id).await {
        Ok(true) => Json(serde_json::json!({ "deleted": id })).into_response(),
        Ok(false) => json_error(StatusCode::NOT_FOUND, format!("no dead letter #{id}")),
        Err(e) => error_response(&e),
    }
}

async fn schedule(_: ApiAccess, app: App) -> Response {
    match crate::remote::schedule_view(&app).await {
        Some(schedule) => Json(schedule).into_response(),
        None => unavailable(),
    }
}

/// `GET /api/events`: `snapshot` (every agent), then `status` per change and `event` per
/// emit; ends when Watchfire shuts down.
async fn events(_: ApiAccess, app: App) -> Response {
    let agents = match agents(&app) {
        Some(a) => a,
        None => return unavailable(),
    };
    // Subscribe first, so nothing between the snapshot and the stream is lost.
    let statuses = agents.subscribe();
    let emitted = agents.subscribe_events();
    let shutdown = agents.shutdown_token();
    let state = Stream {
        agents,
        snapshot: true,
        statuses,
        emitted,
        shutdown,
    };
    let stream = futures_util::stream::unfold(state, |mut st| async move {
        let event = st.next().await?;
        Some((Ok::<_, Infallible>(event), st))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

struct Stream {
    agents: Agents,
    snapshot: bool,
    statuses: tokio::sync::broadcast::Receiver<AgentStatus>,
    emitted: tokio::sync::broadcast::Receiver<crate::agent::Event>,
    shutdown: tokio_util::sync::CancellationToken,
}

fn sse(name: &str, data: &impl Serialize) -> SseEvent {
    SseEvent::default()
        .event(name)
        .json_data(data)
        .unwrap_or_else(|_| SseEvent::default().event(name).data("null"))
}

impl Stream {
    async fn next(&mut self) -> Option<SseEvent> {
        if std::mem::take(&mut self.snapshot) {
            return Some(sse("snapshot", &self.agents.list()));
        }
        loop {
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => return None,
                status = self.statuses.recv() => match status {
                    Ok(status) => return Some(sse("status", &status)),
                    // Fell behind: a fresh snapshot replaces the missed changes.
                    Err(RecvError::Lagged(_)) => return Some(sse("snapshot", &self.agents.list())),
                    Err(RecvError::Closed) => return None,
                },
                event = self.emitted.recv() => match event {
                    Ok(event) => return Some(sse("event", &event)),
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return None,
                },
            }
        }
    }
}
