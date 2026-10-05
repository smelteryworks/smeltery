//! `POST /_sparks/update`: apply model updates and action calls, answer the re-rendered components.

use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Deserialize;
use smeltery_core::auth::Auth;
use smeltery_core::session::Session;
use smeltery_core::{App, Error, Result};

use crate::PROTOCOL_VERSION;
use crate::runtime::{Call, Runtime, UpdateRequest};
use crate::snapshot::{self, expired};

#[derive(Debug, Deserialize)]
struct Request {
    v: u32,
    components: Vec<ComponentRequest>,
}

#[derive(Debug, Deserialize)]
struct ComponentRequest {
    snapshot: String,
    #[serde(default)]
    updates: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    calls: Vec<CallRequest>,
}

#[derive(Debug, Deserialize)]
struct CallRequest {
    method: String,
    #[serde(default)]
    params: Vec<serde_json::Value>,
}

/// The handler of `POST /_sparks/update` (a web route: session, `Auth` and the CSRF check apply).
pub(crate) async fn update(app: App, session: Session, auth: Auth, body: Bytes) -> Response {
    match run(&app, session, auth, &body).await {
        Ok(json) => axum::Json(json).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn run(app: &App, session: Session, auth: Auth, body: &[u8]) -> Result<serde_json::Value> {
    let runtime = Runtime::of(app)?;
    let request: Request = serde_json::from_slice(body).map_err(|_| {
        tracing::warn!("Sparks update rejected: malformed body");
        Error::bad_request("malformed Sparks request")
    })?;
    if request.v != PROTOCOL_VERSION {
        tracing::warn!(
            version = request.v,
            "Sparks update rejected: protocol version"
        );
        return Err(expired());
    }
    // The caps come first: an oversized request costs no snapshot verification and runs nothing.
    let limits = runtime.limits;
    if request.components.len() > limits.max_components {
        tracing::warn!(
            components = request.components.len(),
            "Sparks update rejected: too many components"
        );
        return Err(too_large(format!(
            "at most {} component(s) per request",
            limits.max_components
        )));
    }
    if let Some(c) = request
        .components
        .iter()
        .find(|c| c.calls.len() > limits.max_calls)
    {
        tracing::warn!(
            calls = c.calls.len(),
            "Sparks update rejected: too many calls"
        );
        return Err(too_large(format!(
            "at most {} call(s) per component",
            limits.max_calls
        )));
    }

    // Every component is checked (snapshot, its session and user, allow-lists, guards) before any of them runs.
    let ttl = runtime.snapshot_ttl(app);
    let mut checked = Vec::with_capacity(request.components.len());
    for component in request.components {
        let opened = snapshot::open(app, &component.snapshot).inspect_err(|_| {
            tracing::warn!("Sparks update rejected: invalid snapshot");
        })?;
        let Some(entry) = runtime.entry(opened.memo.name.as_str()) else {
            tracing::warn!(component = %opened.memo.name, "Sparks update rejected: unknown component");
            return Err(Error::not_found());
        };
        opened.memo.check(&session, &auth, ttl)?;
        let calls: Vec<Call> = component
            .calls
            .into_iter()
            .map(|c| Call {
                method: c.method,
                params: c.params,
            })
            .collect();
        entry.check(&component.updates, &calls, &auth)?;
        checked.push((entry, opened, component.updates, calls));
    }

    let mut out = Vec::with_capacity(checked.len());
    for (entry, opened, updates, calls) in checked {
        let id = opened.memo.id.clone();
        let result = entry
            .update(UpdateRequest {
                runtime: runtime.clone(),
                app: app.clone(),
                session: session.clone(),
                auth: auth.clone(),
                opened,
                updates,
                calls,
            })
            .await;
        match result {
            Ok(response) => out.push(response),
            // Nothing ran before it: the error is the answer.
            Err(e) if out.is_empty() => return Err(e),
            // Earlier components already changed things: their results are answered, with this one's error; the
            // components after it do not run.
            Err(e) => {
                out.push(failed_entry(&id, e));
                break;
            }
        }
    }
    Ok(serde_json::json!({ "components": out }))
}

fn too_large(message: String) -> Error {
    Error::http(http::StatusCode::PAYLOAD_TOO_LARGE, message)
}

/// The response entry of a component whose update failed after other components of the request ran:
/// `{"id": …, "error": {"status": 500, "message": "Internal Server Error"}}` (internal details only in the log).
fn failed_entry(id: &str, error: Error) -> serde_json::Value {
    let status = error.status();
    let message = match &error {
        Error::Http { message, .. } => message.clone(),
        other => {
            tracing::error!(error = %other, "Sparks update failed");
            status.canonical_reason().unwrap_or("Error").to_owned()
        }
    };
    serde_json::json!({
        "id": id,
        "error": { "status": status.as_u16(), "message": message },
    })
}
