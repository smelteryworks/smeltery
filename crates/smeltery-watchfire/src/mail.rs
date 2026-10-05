//! Mail with Watchfire (the `mail` feature): queued mail ([`QueueMail`], the [`SendMail`] job) and alert mail
//! (`WATCHFIRE_ALERT_MAIL`).

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smeltery_core::html::escape;
use smeltery_core::{App, WeakApp};
use smeltery_mail::{Email, Envelope, Mailable, Mailer};

use crate::alert::{Alert, AlertHook};
use crate::error::AgentError;
use crate::queue::{Job, JobCtx, JobId};
use crate::registry::Watchfire;

/// The job that sends a queued mail: the mail is rendered when it is queued and sent by a queue worker
/// (retried like any job). Registered automatically when the app has both `.mail()` and `.agents(…)`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendMail {
    /// The rendered mail.
    pub email: Email,
}

impl Job for SendMail {
    const NAME: &'static str = "smeltery-send-mail";

    async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
        let mailer = Mailer::of(ctx.app()).map_err(AgentError::msg)?;
        mailer
            .send_email(self.email.clone())
            .await
            .map_err(AgentError::msg)
    }
}

/// `mailer.queue(mail)`: render now, send from a queue worker.
///
/// ```
/// # // The facade's prelude, rebuilt from this crate's dependencies: a dev-dependency on the facade would turn
/// # // on this crate's `mail` feature in every test build (D-140).
/// # mod smeltery {
/// #     pub mod prelude {
/// #         pub use smeltery_core::Result;
/// #         pub use smeltery_mail::{Envelope, Mailable, Mailer};
/// #         pub use smeltery_watchfire::mail::QueueMail as _;
/// #     }
/// # }
/// use smeltery::prelude::*;
/// # #[derive(smeltery_mold_macros::Mold)]
/// # #[mold("mail/welcome", crate = "::smeltery_mold", dir = "../smeltery/resources/views")]
/// # pub struct Welcome {
/// #     pub name: String,
/// #     pub email: String,
/// # }
/// # impl Mailable for Welcome {
/// #     fn envelope(&self) -> Envelope {
/// #         Envelope::new().to(&self.email)
/// #     }
/// # }
///
/// async fn register(mailer: Mailer) -> Result<&'static str> {
///     mailer.queue(Welcome { name: "Ada".into(), email: "ada@example.com".into() }).await?;
///     Ok("queued")
/// }
/// # fn main() {}
/// ```
pub trait QueueMail {
    /// Render `mail` and queue it for the workers.
    ///
    /// # Errors
    /// A template error, or the queue fails (Watchfire is not set up: `.agents(…)`).
    fn queue<M: Mailable>(
        &self,
        mail: M,
    ) -> impl Future<Output = smeltery_core::Result<JobId>> + Send;

    /// Render `mail` and queue it to be sent after `delay`.
    ///
    /// # Errors
    /// See [`QueueMail::queue`].
    fn queue_later<M: Mailable>(
        &self,
        mail: M,
        delay: Duration,
    ) -> impl Future<Output = smeltery_core::Result<JobId>> + Send;
}

impl QueueMail for Mailer {
    async fn queue<M: Mailable>(&self, mail: M) -> smeltery_core::Result<JobId> {
        let email = self.render(mail).await?;
        SendMail { email }.dispatch(self.app()).await
    }

    async fn queue_later<M: Mailable>(
        &self,
        mail: M,
        delay: Duration,
    ) -> smeltery_core::Result<JobId> {
        let email = self.render(mail).await?;
        SendMail { email }.dispatch_later(self.app(), delay).await
    }
}

/// Before launch: register [`SendMail`] and the alert mail hook when the app has a mailer.
pub(crate) fn prepare(app: &App, watchfire: &mut Watchfire, alert_mail: &[String]) {
    if Mailer::of(app).is_err() {
        if !alert_mail.is_empty() {
            tracing::warn!(
                "WATCHFIRE_ALERT_MAIL is set but mail is not installed (`.mail()`); alerts are not mailed"
            );
        }
        return;
    }
    if !watchfire.job_names().contains(&SendMail::NAME) {
        watchfire.job::<SendMail>();
    }
    if !alert_mail.is_empty() {
        watchfire
            .alert_hooks
            .push(alert_hook(app.downgrade(), alert_mail.to_vec()));
    }
}

/// The mail for one alert.
pub(crate) fn alert_email(alert: &Alert, to: &[String]) -> Email {
    let mut envelope = Envelope::new().subject(format!(
        "[{}] Watchfire alert: {} {}",
        alert.app,
        alert.kind.as_str(),
        alert.agent
    ));
    for address in to {
        envelope = envelope.to(address.as_str());
    }
    let mut text = format!(
        "{}\n\nKind: {}\nAgent: {}\n",
        alert.message,
        alert.kind.as_str(),
        alert.agent
    );
    if let Some(job) = &alert.job {
        text.push_str(&format!("Job: {job}\n"));
    }
    text.push_str(&format!("At: {}\n", crate::time::format_utc(alert.at_ms)));
    let html = format!(
        "<p>{}</p><pre>{}</pre>",
        escape(&alert.message),
        escape(&text)
    );
    Email::new(envelope).text(text).html(html)
}

/// The hook holds a [`WeakApp`]: it lives inside the app's Watchfire runtime.
fn alert_hook(app: WeakApp, to: Vec<String>) -> AlertHook {
    crate::alert::hook(move |alert: Alert| {
        let app = app.upgrade();
        let email = alert_email(&alert, &to);
        async move {
            let Some(app) = app else { return };
            let sent = match Mailer::of(&app) {
                Ok(mailer) => mailer.send_email(email).await,
                Err(e) => Err(e),
            };
            if let Err(e) = sent {
                tracing::warn!(error = %e, "cannot mail the alert");
            }
        }
    })
}
