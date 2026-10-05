//! `agents_list` and `agent_control`: the running app's Watchfire API, like the `agents:*` console commands.

use smeltery_core::App;
use smeltery_watchfire::http::{Http, HttpOptions, Method, ReqwestTransport};

use super::Outcome;
use crate::Options;

/// The actions `agent_control` accepts.
pub(crate) const ACTIONS: &[&str] = &["start", "stop", "pause", "resume", "restart"];

async fn call(app: &App, options: &Options, method: Method, path: &str) -> Outcome {
    // Watchfire's own rule (D-324): plain http only to this machine, so the token never crosses the network
    // unencrypted. Checked before any client is built or request sent.
    let base = match smeltery_watchfire::web::api_base_url(app) {
        Ok(base) => base,
        Err(e) => return Outcome::error(e.to_string()),
    };
    let transport = match ReqwestTransport::new() {
        Ok(t) => t,
        Err(e) => return Outcome::error(format!("no HTTP client: {e}")),
    };
    let http = Http::new(transport, HttpOptions::default(), &[]);
    let url = format!("{base}{path}");
    let mut request = http
        .request(method, &url)
        .timeout(options.http_timeout)
        .retries(0);
    if let Some(token) = smeltery_watchfire::web::api_token(&app.settings().key) {
        request = request.bearer(&token);
    }
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if status.is_success() {
                Outcome::ok(text)
            } else {
                Outcome::error(format!("{status}: {text}"))
            }
        }
        Err(e) => Outcome::error(format!(
            "cannot reach the app's Watchfire API at {url}: is it running (`smeltery serve`, or `smeltery work` \
             with WATCHFIRE_API_ADDR set)? ({e})"
        )),
    }
}

pub(super) async fn list(app: &App, options: &Options) -> Outcome {
    call(app, options, Method::GET, "/agents").await
}

pub(super) async fn control(app: &App, options: &Options, name: &str, action: &str) -> Outcome {
    if !ACTIONS.contains(&action) {
        return Outcome::error(format!(
            "`{action}` is not an action; use one of: {}",
            ACTIONS.join(", ")
        ));
    }
    let valid = !name.is_empty()
        && name.len() <= 70
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '#'));
    if !valid {
        return Outcome::error(format!("`{name}` is not an agent name"));
    }
    let encoded = name.replace('#', "%23");
    call(
        app,
        options,
        Method::POST,
        &format!("/agents/{encoded}/{action}"),
    )
    .await
}
