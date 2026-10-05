//! [`Mailer`]: renders mailables and hands them to the configured transport; [`MailExt::mail`] installs it.

use std::sync::{Arc, PoisonError, RwLock};

use smeltery_core::auth::passwords::ResetNotifier;
use smeltery_core::auth::verification::VerificationNotifier;
use smeltery_core::{App, AppBuilder, BoxFuture, Error, Result};

use crate::email::{Email, html_to_text};
use crate::mailable::{MailHost, Mailable};
use crate::reset::ResetPassword;
use crate::transport::{FakeTransport, MailSettings, Mailbox, Transport};
use crate::verify::VerifyEmail;

/// The installed mail service of an app.
pub(crate) struct MailCore {
    settings: MailSettings,
    transport: RwLock<Arc<dyn Transport>>,
}

impl MailCore {
    fn transport(&self) -> Arc<dyn Transport> {
        Arc::clone(
            &self
                .transport
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

/// Sends mail: a handler argument (`mailer: Mailer`), or `Mailer::of(&app)` elsewhere.
///
/// ```
/// # use smeltery_core::Result;
/// # use smeltery_core::http::{Form, Redirect};
/// # use smeltery_mail::{Envelope, Mailable, Mailer};
/// # #[derive(serde::Deserialize)]
/// # struct InviteForm { email: String }
/// # struct Invitation { email: String }
/// # impl smeltery_mold::Template for Invitation {
/// #     const NAME: &'static str = "mail/invitation";
/// #     fn render_runtime(&self, _: &dyn smeltery_mold::Host) -> Result<String, smeltery_mold::Error> { Ok(String::new()) }
/// #     fn render_compiled(&self, _: &dyn smeltery_mold::Host) -> Result<String, smeltery_mold::Error> { Ok(String::new()) }
/// # }
/// # impl Mailable for Invitation {
/// #     fn envelope(&self) -> Envelope { Envelope::new().to(self.email.as_str()) }
/// # }
/// async fn invite(mailer: Mailer, Form(form): Form<InviteForm>) -> Result<Redirect> {
///     mailer.send(Invitation { email: form.email }).await?;
///     Ok(Redirect::to("/team"))
/// }
/// ```
#[derive(Clone)]
pub struct Mailer {
    app: App,
    core: Arc<MailCore>,
}

impl std::fmt::Debug for Mailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailer")
            .field("transport", &self.core.transport().name())
            .finish_non_exhaustive()
    }
}

impl Mailer {
    /// The mailer of `app`.
    ///
    /// # Errors
    /// Mail is not installed (`.mail()` in `bootstrap/app.rs`).
    pub fn of(app: &App) -> Result<Self> {
        let core = app.service::<Arc<MailCore>>().ok_or_else(|| {
            Error::internal("mail is not installed: call `.mail()` in bootstrap/app.rs")
        })?;
        Ok(Self {
            app: app.clone(),
            core: Arc::clone(&core),
        })
    }

    /// Replace `app`'s transport with a fake and return its [`Mailbox`] (installs mail when it is not
    /// installed). For tests.
    pub fn fake(app: &App) -> Mailbox {
        let transport = Arc::new(FakeTransport::default());
        let mailbox = transport.mailbox().cloned().unwrap_or_default();
        match Self::of(app) {
            Ok(mailer) => mailer.use_transport(transport),
            Err(_) => {
                let mut settings = MailSettings::from_env(app.settings());
                settings.mailer = "fake".to_owned();
                app.insert_service(Arc::new(MailCore {
                    settings,
                    transport: RwLock::new(transport),
                }));
                app.insert_service::<Arc<dyn ResetNotifier>>(Arc::new(MailResetNotifier));
                app.insert_service::<Arc<dyn VerificationNotifier>>(Arc::new(
                    MailVerificationNotifier,
                ));
            }
        }
        mailbox
    }

    /// Send through `transport` from now on (for every handle of this app).
    pub fn use_transport(&self, transport: Arc<dyn Transport>) {
        *self
            .core
            .transport
            .write()
            .unwrap_or_else(PoisonError::into_inner) = transport;
    }

    /// The fake transport's mailbox, when the transport is the fake.
    pub fn mailbox(&self) -> Option<Mailbox> {
        self.core.transport().mailbox().cloned()
    }

    /// The transport's name: `smtp`, `log`, `fake` or a custom one.
    pub fn transport_name(&self) -> &'static str {
        self.core.transport().name()
    }

    /// The settings read from `.env`.
    pub fn settings(&self) -> &MailSettings {
        &self.core.settings
    }

    /// The app.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Render `mail` and send it.
    ///
    /// # Errors
    /// A template error, an attachment that cannot be read, an invalid address, or a transport failure (the
    /// SMTP server refused or did not answer in time).
    pub async fn send<M: Mailable>(&self, mail: M) -> Result<()> {
        let (email, mail) = self.render_keeping(mail).await?;
        let transport = self.core.transport();
        if let Some(mailbox) = transport.mailbox() {
            mailbox.record(email, Some(Arc::new(mail)));
            return Ok(());
        }
        self.deliver(transport.as_ref(), &email).await
    }

    /// Render `mail` into an [`Email`] without sending it (what a queued mail carries).
    ///
    /// # Errors
    /// A template error or an attachment that cannot be read.
    pub async fn render<M: Mailable>(&self, mail: M) -> Result<Email> {
        Ok(self.render_keeping(mail).await?.0)
    }

    /// Send an already rendered (or hand-built) mail.
    ///
    /// # Errors
    /// An invalid address or a transport failure.
    pub async fn send_email(&self, email: Email) -> Result<()> {
        let transport = self.core.transport();
        self.deliver(transport.as_ref(), &email).await
    }

    async fn deliver(&self, transport: &dyn Transport, email: &Email) -> Result<()> {
        let result = transport.send(email, &self.core.settings.from).await;
        if let Err(e) = &result {
            tracing::warn!(
                target: "smeltery::mail",
                transport = transport.name(),
                subject = %email.subject(),
                error = %e,
                "mail not sent"
            );
        }
        result
    }

    /// Render on a blocking thread (templates may be read from disk) and give the mailable back.
    async fn render_keeping<M: Mailable>(&self, mail: M) -> Result<(Email, M)> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || {
            let host = MailHost { app: app.clone() };
            let template_error =
                |e: smeltery_mold::Error| Error::internal(format!("mail template error: {e}"));
            let html = mail.html(app.views(), &host).map_err(template_error)?;
            let text = mail
                .text(app.views(), &host)
                .map_err(template_error)?
                .unwrap_or_else(|| html_to_text(&html));
            let mut email = Email::new(mail.envelope())
                .html(html)
                .text(text)
                .with_kind(std::any::type_name::<M>());
            for attachment in mail.attachments()? {
                email = email.attach(attachment);
            }
            Ok((email, mail))
        })
        .await
        .map_err(|e| Error::internal(format!("rendering a mail panicked: {e}")))?
    }
}

impl axum::extract::FromRequestParts<App> for Mailer {
    type Rejection = Error;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> Result<Self, Self::Rejection> {
        Self::of(app)
    }
}

/// Installs mail on an [`AppBuilder`]: `.mail()` in `bootstrap/app.rs`.
pub trait MailExt: Sized {
    /// Read the mail settings from `.env` at boot, register the [`Mailer`] service, and send password reset
    /// links as a [`ResetPassword`] mail and email verification links as a [`VerifyEmail`] mail (instead of
    /// logging them).
    fn mail(self) -> Self;
}

impl MailExt for AppBuilder {
    fn mail(self) -> Self {
        self.service::<Arc<dyn ResetNotifier>>(Arc::new(MailResetNotifier))
            .service::<Arc<dyn VerificationNotifier>>(Arc::new(MailVerificationNotifier))
            .on_boot(|app| async move {
                let settings = MailSettings::from_env(app.settings());
                let transport = settings.transport()?;
                tracing::debug!(target: "smeltery::mail", transport = transport.name(), "mail ready");
                // Mails are not delivered at all here; outside local development the log transport
                // also withholds bodies (they can carry reset links), so say so loudly at boot.
                if transport.name() == "log" && !app.settings().is_local_development() {
                    tracing::warn!(
                        target: "smeltery::mail",
                        env = %app.settings().env,
                        "MAIL_MAILER=log: mails are not sent; the log gets their sender, recipients and subject only (bodies are withheld outside local development: APP_ENV local or testing with a loopback APP_URL); set MAIL_MAILER=smtp"
                    );
                }
                app.insert_service(Arc::new(MailCore {
                    settings,
                    transport: RwLock::new(transport),
                }));
                Ok(())
            })
    }
}

/// Sends password reset links as [`ResetPassword`] mails.
struct MailResetNotifier;

impl ResetNotifier for MailResetNotifier {
    fn send<'a>(&'a self, app: &'a App, email: &'a str, url: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let minutes = smeltery_core::auth::passwords::TOKEN_LIFETIME.as_secs() / 60;
            Mailer::of(app)?
                .send(ResetPassword {
                    app_name: app.settings().name.clone(),
                    email: email.to_owned(),
                    url: url.to_owned(),
                    minutes,
                })
                .await
        })
    }
}

/// Sends email verification links as [`VerifyEmail`] mails.
struct MailVerificationNotifier;

impl VerificationNotifier for MailVerificationNotifier {
    fn send<'a>(&'a self, app: &'a App, email: &'a str, url: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let minutes = app.settings().verification_expire.as_secs() / 60;
            Mailer::of(app)?
                .send(VerifyEmail {
                    app_name: app.settings().name.clone(),
                    email: email.to_owned(),
                    url: url.to_owned(),
                    minutes,
                })
                .await
        })
    }
}
