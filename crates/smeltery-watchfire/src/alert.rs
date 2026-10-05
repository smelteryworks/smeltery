//! Alerts: things a human should know about (a failed agent, a stalled run, a dead-lettered
//! job), delivered to `Watchfire::on_alert` hooks and the `WATCHFIRE_ALERT_WEBHOOK`.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt as _;
use serde::Serialize;
use smeltery_core::BoxFuture;
use tokio::sync::mpsc;

use crate::http::Method;
use crate::runtime::Shared;

/// Alerts waiting for delivery; when full, new alerts are logged and dropped so supervision
/// never waits.
pub(crate) const ALERT_CAPACITY: usize = 64;
/// How long one hook or one webhook delivery may take.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AlertKind {
    /// An agent is `failed`: its run failed under `Restart::Never`, or it hit its restart limit.
    Failed,
    /// A run stopped heartbeating (it is restarted).
    Stalled,
    /// A job failed for good and went to the dead letters.
    DeadLetter,
}

impl AlertKind {
    /// `failed`, `stalled` or `dead_letter`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Stalled => "stalled",
            Self::DeadLetter => "dead_letter",
        }
    }
}

/// One alert, as hooks receive it and the webhook posts it (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Alert {
    /// What happened.
    pub kind: AlertKind,
    /// The app (`APP_NAME`).
    pub app: String,
    /// The agent (for a dead letter: the worker).
    pub agent: String,
    /// The job, for dead letters.
    pub job: Option<String>,
    /// A sentence for humans.
    pub message: String,
    /// When, Unix milliseconds.
    pub at_ms: i64,
}

pub(crate) type AlertHook = Arc<dyn Fn(Alert) -> BoxFuture<'static, ()> + Send + Sync>;

/// Wrap a user closure.
pub(crate) fn hook<F, Fut>(f: F) -> AlertHook
where
    F: Fn(Alert) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |alert| Box::pin(f(alert)))
}

/// The delivery task: one per runtime, owned by its task set, ends after shutdown once the
/// queued alerts are delivered (within the shutdown grace).
pub(crate) async fn deliver(
    shared: Arc<Shared>,
    mut rx: mpsc::Receiver<Alert>,
    hooks: Vec<AlertHook>,
    webhook: Option<String>,
) {
    loop {
        let alert = tokio::select! {
            biased;
            alert = rx.recv() => alert,
            () = shared.shutdown.cancelled() => None,
        };
        let Some(alert) = alert else { break };
        deliver_one(&shared, &alert, &hooks, webhook.as_deref()).await;
    }
    // Shutdown: what is queued still goes out, within the grace the budget leaves.
    rx.close();
    let grace = shared.grace(DELIVERY_TIMEOUT);
    let drain = async {
        while let Some(alert) = rx.recv().await {
            deliver_one(&shared, &alert, &hooks, webhook.as_deref()).await;
        }
    };
    if tokio::time::timeout(grace, drain).await.is_err() {
        tracing::warn!("alerts left undelivered at shutdown");
    }
}

async fn deliver_one(shared: &Shared, alert: &Alert, hooks: &[AlertHook], webhook: Option<&str>) {
    for hook in hooks {
        let call = AssertUnwindSafe(hook(alert.clone())).catch_unwind();
        match tokio::time::timeout(DELIVERY_TIMEOUT, call).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => tracing::warn!(kind = alert.kind.as_str(), "an alert hook panicked"),
            Err(_) => tracing::warn!(kind = alert.kind.as_str(), "an alert hook timed out"),
        }
    }
    if let Some(url) = webhook {
        let sent = shared
            .http
            .request(Method::POST, url)
            .json(alert)
            .timeout(DELIVERY_TIMEOUT)
            .repeatable()
            .send()
            .await;
        match sent {
            Ok(res) if res.status().is_success() => {}
            Ok(res) => tracing::warn!(
                status = res.status().as_u16(),
                host = %webhook_host(url),
                "the alert webhook refused the alert"
            ),
            // The error and the host only: the webhook's path and query are often its secret.
            Err(e) => tracing::warn!(
                error = %without_path(&e.to_string(), url),
                host = %webhook_host(url),
                "cannot deliver the alert webhook"
            ),
        }
    }
}

/// An error text with the webhook's redacted URL (`scheme://host/path`) cut down to its host.
fn without_path(text: &str, url: &str) -> String {
    text.replace(&crate::http::redact_url(url), &webhook_host(url))
}

/// The host (and port) of a webhook URL, for logs: its path and query are often the secret.
pub(crate) fn webhook_host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str().map(|h| {
                u.port()
                    .map_or_else(|| h.to_owned(), |p| format!("{h}:{p}"))
            })
        })
        .unwrap_or_else(|| "<invalid URL>".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_logs_show_the_host_only() {
        assert_eq!(
            webhook_host("https://hooks.example.com/services/T000/B000/SECRET?token=X"),
            "hooks.example.com"
        );
        assert_eq!(webhook_host("http://127.0.0.1:9/x"), "127.0.0.1:9");
        assert_eq!(webhook_host("nonsense SECRET"), "<invalid URL>");
        let url = "https://hooks.example.com/services/T000/B000/SECRET?token=X";
        let text = format!("cannot connect: refused ({})", crate::http::redact_url(url));
        assert_eq!(
            without_path(&text, url),
            "cannot connect: refused (hooks.example.com)"
        );
    }
}
