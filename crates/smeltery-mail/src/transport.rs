//! Transports: SMTP (lettre over rustls), the log, and the fake [`Mailbox`] for tests.

use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lettre::AsyncTransport as _;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, Tokio1Executor};
use smeltery_core::config::{Settings, env, loopback_host};
use smeltery_core::{BoxFuture, Error, Result};

use crate::address::Address;
use crate::email::{Email, html_to_text};

/// Sends rendered mails. Implemented by the SMTP, log and fake transports; an app may register its own with
/// [`Mailer::use_transport`](crate::Mailer::use_transport).
pub trait Transport: Send + Sync + 'static {
    /// A short name for logs (`smtp`, `log`, `fake`).
    fn name(&self) -> &'static str;

    /// Send `email`, from `from` unless the email names its own sender.
    ///
    /// # Errors
    /// The mail could not be delivered.
    fn send<'a>(&'a self, email: &'a Email, from: &'a Address) -> BoxFuture<'a, Result<()>>;

    /// The mailbox, for the fake transport.
    fn mailbox(&self) -> Option<&Mailbox> {
        None
    }
}

/// Mail settings from `.env`.
///
/// | Variable | Default | Meaning |
/// |---|---|---|
/// | `MAIL_MAILER` | `log` | `smtp`, `log`, `fake` (alias `array`); always `fake` under `APP_ENV=testing`; `log` outside local development (`APP_ENV` `local` / `testing` with a loopback `APP_URL`) withholds mail bodies and logs a warning at boot |
/// | `MAIL_HOST` | `127.0.0.1` | SMTP server |
/// | `MAIL_PORT` | `465` for `tls`, `587` for `starttls`, `25` for `none` | SMTP port |
/// | `MAIL_USERNAME` / `MAIL_PASSWORD` | empty | SMTP login (none when the username is empty) |
/// | `MAIL_ENCRYPTION` | `starttls` | `tls` (TLS from the start), `starttls` (required upgrade), `none` (with a `MAIL_USERNAME` only for a `MAIL_HOST` on this machine: a login over plain SMTP elsewhere is refused) |
/// | `MAIL_TLS_CA` | empty | a PEM file of extra CA certificates to trust for the SMTP server (relative to the app root), for a server with a private CA |
/// | `MAIL_TIMEOUT` | `10` | seconds per SMTP step; a whole send gets four times that |
/// | `MAIL_FROM_ADDRESS` | `hello@example.com` | the default sender |
/// | `MAIL_FROM_NAME` | `APP_NAME` | the default sender's name |
#[derive(Clone)]
#[non_exhaustive]
pub struct MailSettings {
    /// `MAIL_MAILER`.
    pub mailer: String,
    /// `MAIL_HOST`.
    pub host: String,
    /// `MAIL_PORT`.
    pub port: u16,
    /// `MAIL_USERNAME`.
    pub username: String,
    /// `MAIL_PASSWORD` (never printed).
    pub password: String,
    /// `MAIL_ENCRYPTION`.
    pub encryption: String,
    /// `MAIL_TLS_CA`, resolved against the app root: extra trusted CA certificates (PEM), next to the
    /// platform's.
    pub tls_ca: Option<PathBuf>,
    /// `MAIL_TIMEOUT`.
    pub timeout: Duration,
    /// `MAIL_FROM_ADDRESS` / `MAIL_FROM_NAME`.
    pub from: Address,
    /// Whether the `log` transport writes mail bodies: only in local development (`APP_ENV` `local` or `testing`
    /// with a loopback `APP_URL`, see [`Settings::is_local_development`]), because a body can carry secrets
    /// (password reset links).
    pub log_bodies: bool,
}

impl std::fmt::Debug for MailSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailSettings")
            .field("mailer", &self.mailer)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("encryption", &self.encryption)
            .field("tls_ca", &self.tls_ca)
            .field("timeout", &self.timeout)
            .field("from", &self.from)
            .field("log_bodies", &self.log_bodies)
            .finish()
    }
}

impl MailSettings {
    /// Read the settings for an app with `settings`.
    pub fn from_env(settings: &Settings) -> Self {
        let encryption = env::<String>("MAIL_ENCRYPTION", "starttls").to_ascii_lowercase();
        let default_port = match encryption.as_str() {
            "tls" | "ssl" => 465,
            "none" => 25,
            _ => 587,
        };
        let name = env::<String>("MAIL_FROM_NAME", "");
        let name = if name.is_empty() {
            settings.name.clone()
        } else {
            name
        };
        Self {
            // Tests never send real mail: `APP_ENV=testing` always uses the fake.
            mailer: if settings.env == "testing" {
                "fake".to_owned()
            } else {
                env::<String>("MAIL_MAILER", "log").to_ascii_lowercase()
            },
            host: env::<String>("MAIL_HOST", "127.0.0.1"),
            port: env::<u16>("MAIL_PORT", default_port),
            username: env::<String>("MAIL_USERNAME", ""),
            password: env::<String>("MAIL_PASSWORD", ""),
            encryption,
            tls_ca: {
                let path = env::<String>("MAIL_TLS_CA", "");
                let path = path.trim();
                (!path.is_empty()).then(|| settings.root.join(path))
            },
            timeout: Duration::from_secs(env::<u64>("MAIL_TIMEOUT", 10).max(1)),
            from: Address::new(env::<String>("MAIL_FROM_ADDRESS", "hello@example.com")).named(name),
            log_bodies: settings.is_local_development(),
        }
    }

    /// The transport these settings name.
    ///
    /// # Errors
    /// An unknown `MAIL_MAILER` or `MAIL_ENCRYPTION`, or TLS that cannot be set up.
    pub fn transport(&self) -> Result<Arc<dyn Transport>> {
        match self.mailer.as_str() {
            "smtp" => Ok(Arc::new(SmtpTransport::new(self)?)),
            "log" => Ok(Arc::new(if self.log_bodies {
                LogTransport::new()
            } else {
                LogTransport::without_bodies()
            })),
            "fake" | "array" => Ok(Arc::new(FakeTransport::default())),
            other => Err(Error::internal(format!(
                "MAIL_MAILER must be `smtp`, `log` or `fake`, not `{other}`"
            ))),
        }
    }
}

/// SMTP through lettre's Tokio transport (rustls with ring). Each step has `MAIL_TIMEOUT`; a whole send four
/// times that.
pub struct SmtpTransport {
    inner: AsyncSmtpTransport<Tokio1Executor>,
    timeout: Duration,
    host: String,
}

impl std::fmt::Debug for SmtpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpTransport")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

impl SmtpTransport {
    /// A transport for `settings` (no connection is made until a mail is sent).
    ///
    /// # Errors
    /// An unknown `MAIL_ENCRYPTION`, a `MAIL_TLS_CA` file that cannot be read or holds no certificate, or TLS
    /// that cannot be set up.
    pub fn new(settings: &MailSettings) -> Result<Self> {
        let tls_params = || {
            let mut builder = TlsParameters::builder(settings.host.clone());
            if let Some(path) = &settings.tls_ca {
                builder = builder.add_root_certificate(read_ca(path)?);
            }
            builder
                .build()
                .map_err(|e| Error::internal(format!("cannot set up TLS for the mail server: {e}")))
        };
        let tls = match settings.encryption.as_str() {
            "tls" | "ssl" => Tls::Wrapper(tls_params()?),
            "starttls" => Tls::Required(tls_params()?),
            // lettre sends AUTH PLAIN / LOGIN over a plain connection too (lettre 0.11.23
            // `src/transport/smtp/client/async_connection.rs:276`): the password would cross the network in clear.
            "none" | "" if !settings.username.is_empty() && !loopback_host(&settings.host) => {
                return Err(Error::internal(format!(
                    "MAIL_ENCRYPTION=none with MAIL_USERNAME would send the SMTP password unencrypted to {}; \
                     use MAIL_ENCRYPTION=tls or starttls (plain SMTP with a login is accepted only for a relay \
                     on this machine)",
                    settings.host
                )));
            }
            "none" | "" => Tls::None,
            other => {
                return Err(Error::internal(format!(
                    "MAIL_ENCRYPTION must be `tls`, `starttls` or `none`, not `{other}`"
                )));
            }
        };
        let mut builder =
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(settings.host.clone())
                .port(settings.port)
                .tls(tls)
                .timeout(Some(settings.timeout));
        if !settings.username.is_empty() {
            builder = builder.credentials(Credentials::new(
                settings.username.clone(),
                settings.password.clone(),
            ));
        }
        Ok(Self {
            inner: builder.build(),
            timeout: settings.timeout,
            host: settings.host.clone(),
        })
    }
}

impl Transport for SmtpTransport {
    fn name(&self) -> &'static str {
        "smtp"
    }

    fn send<'a>(&'a self, email: &'a Email, from: &'a Address) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let message = email.to_message(from)?;
            let budget = self.timeout.saturating_mul(4);
            match tokio::time::timeout(budget, self.inner.send(message)).await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) if e.is_response() || e.is_permanent() || e.is_transient() => {
                    Err(Error::internal(format!(
                        "the mail server {} refused the mail: {e}",
                        self.host
                    )))
                }
                // Connecting, the TLS handshake (an untrusted certificate) or the network failed.
                Ok(Err(e)) => Err(Error::internal(format!(
                    "the connection to the mail server {} failed: {e}",
                    self.host
                ))),
                Err(_) => Err(Error::internal(format!(
                    "the mail server {} did not answer within {} s",
                    self.host,
                    budget.as_secs()
                ))),
            }
        })
    }
}

/// The CA certificates in the PEM file `path` (`MAIL_TLS_CA`). Read once, when the transport is made.
fn read_ca(path: &std::path::Path) -> Result<Certificate> {
    let pem = std::fs::read(path).map_err(|e| {
        Error::internal(format!("MAIL_TLS_CA: cannot read {}: {e}", path.display()))
    })?;
    // `Certificate::from_pem` accepts a file without any certificate; that is a mistake here.
    if !String::from_utf8_lossy(&pem).contains("-----BEGIN CERTIFICATE-----") {
        return Err(Error::internal(format!(
            "MAIL_TLS_CA: {} holds no PEM certificate",
            path.display()
        )));
    }
    Certificate::from_pem(&pem)
        .map_err(|e| Error::internal(format!("MAIL_TLS_CA: {}: {e}", path.display())))
}

/// Writes each mail to the log (`info`, target `smeltery::mail`): sender, recipients, subject and the text
/// part. The development default.
///
/// `MAIL_MAILER=log` uses [`LogTransport::new`] only in local development (`APP_ENV` `local` or `testing` with a
/// loopback `APP_URL`); anywhere else it uses [`LogTransport::without_bodies`], which logs the envelope and
/// subject and notes that the body was withheld, because a body can carry secrets (a password reset link).
#[derive(Debug)]
pub struct LogTransport {
    bodies: bool,
}

impl Default for LogTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl LogTransport {
    /// Log whole mails, text part included.
    pub fn new() -> Self {
        Self { bodies: true }
    }

    /// Log sender, recipients and subject only; the body is withheld.
    pub fn without_bodies() -> Self {
        Self { bodies: false }
    }
}

impl Transport for LogTransport {
    fn name(&self) -> &'static str {
        "log"
    }

    fn send<'a>(&'a self, email: &'a Email, from: &'a Address) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let env = email.envelope();
            let list = |a: &[Address]| {
                a.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let text = if !self.bodies {
                "(body withheld: outside local development (APP_ENV local or testing with a loopback APP_URL) \
                 a mail body can carry secrets such as password reset links; set MAIL_MAILER=smtp to deliver mail)"
                    .to_owned()
            } else {
                match (email.text_body(), email.html_body()) {
                    (Some(t), _) => t.to_owned(),
                    (None, Some(h)) => html_to_text(h),
                    (None, None) => String::new(),
                }
            };
            tracing::info!(
                target: "smeltery::mail",
                from = %env.from_address().unwrap_or(from),
                to = %list(env.to_addresses()),
                cc = %list(env.cc_addresses()),
                bcc = %list(env.bcc_addresses()),
                subject = %email.subject(),
                attachments = email.attachments().len(),
                "mail (log transport)\n{text}"
            );
            Ok(())
        })
    }
}

/// One mail the fake transport received.
#[derive(Clone)]
pub struct SentMail {
    email: Email,
    mailable: Option<Arc<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for SentMail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SentMail")
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

impl SentMail {
    /// The rendered mail.
    pub fn email(&self) -> &Email {
        &self.email
    }

    /// The mailable it came from, when it is a `T` (mails queued or built by hand have none).
    pub fn mailable<T: 'static>(&self) -> Option<&T> {
        self.mailable.as_deref().and_then(|m| m.downcast_ref::<T>())
    }
}

/// The mails the fake transport received, for assertions in tests. Cheap to clone; clones share the list.
///
/// ```
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// use smeltery_mail::{Email, Envelope, Mailbox};
///
/// let mailbox = Mailbox::default();
/// mailbox.record(Email::new(Envelope::new().to("a@b.test").subject("Hi")).text("x"), None);
/// mailbox.assert_sent_to("a@b.test");
/// assert_eq!(mailbox.sent().len(), 1);
/// # });
/// ```
#[derive(Clone, Default)]
pub struct Mailbox {
    sent: Arc<Mutex<Vec<SentMail>>>,
}

impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailbox")
            .field("sent", &self.len())
            .finish()
    }
}

impl Mailbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<SentMail>> {
        self.sent.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keep `email` (and the mailable it came from).
    pub fn record(&self, email: Email, mailable: Option<Arc<dyn Any + Send + Sync>>) {
        self.lock().push(SentMail { email, mailable });
    }

    /// Every mail received, oldest first.
    pub fn sent(&self) -> Vec<SentMail> {
        self.lock().clone()
    }

    /// The rendered mails, oldest first.
    pub fn emails(&self) -> Vec<Email> {
        self.lock().iter().map(|s| s.email.clone()).collect()
    }

    /// How many mails were received.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no mail was received.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Forget every mail.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// The mailables of type `T` received, with their rendered mails.
    pub fn sent_of<T: Clone + 'static>(&self) -> Vec<(T, Email)> {
        self.lock()
            .iter()
            .filter_map(|s| s.mailable::<T>().map(|m| (m.clone(), s.email.clone())))
            .collect()
    }

    /// Panic unless a mailable of type `T` matching `check` was sent.
    ///
    /// # Panics
    /// When none was.
    #[allow(clippy::panic)]
    pub fn assert_sent<T: 'static>(&self, check: impl Fn(&T, &Email) -> bool) {
        let sent = self.lock();
        if !sent
            .iter()
            .any(|s| s.mailable::<T>().is_some_and(|m| check(m, &s.email)))
        {
            panic!(
                "no matching `{}` was sent; sent: {:?}",
                std::any::type_name::<T>(),
                sent.iter()
                    .map(|s| s.email.subject().to_owned())
                    .collect::<Vec<_>>()
            );
        }
    }

    /// Panic unless exactly `count` mailables of type `T` were sent.
    ///
    /// # Panics
    /// On another count.
    #[allow(clippy::panic)]
    pub fn assert_sent_count<T: 'static>(&self, count: usize) {
        let n = self
            .lock()
            .iter()
            .filter(|s| s.mailable::<T>().is_some())
            .count();
        if n != count {
            panic!(
                "expected {count} `{}` mail(s), {n} sent",
                std::any::type_name::<T>()
            );
        }
    }

    /// Panic unless a mail (of any kind) went to `email`.
    ///
    /// # Panics
    /// When none did.
    #[allow(clippy::panic)]
    pub fn assert_sent_to(&self, email: &str) {
        if !self.lock().iter().any(|s| s.email.has_recipient(email)) {
            panic!("no mail was sent to {email}");
        }
    }

    /// Panic if any mail was sent.
    ///
    /// # Panics
    /// When one was.
    #[allow(clippy::panic)]
    pub fn assert_nothing_sent(&self) {
        let sent = self.lock();
        if !sent.is_empty() {
            panic!(
                "expected no mail, {} sent: {:?}",
                sent.len(),
                sent.iter()
                    .map(|s| s.email.subject().to_owned())
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// Keeps every mail in a [`Mailbox`] instead of sending it (`MAIL_MAILER=fake`, the default under
/// `APP_ENV=testing`).
#[derive(Debug, Default)]
pub struct FakeTransport {
    mailbox: Mailbox,
}

impl Transport for FakeTransport {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn send<'a>(&'a self, email: &'a Email, _from: &'a Address) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.mailbox.record(email.clone(), None);
            Ok(())
        })
    }

    fn mailbox(&self) -> Option<&Mailbox> {
        Some(&self.mailbox)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_hide_the_password_and_pick_ports() {
        let mut app = Settings::from_env();
        app.env = "testing".into();
        let mut s = MailSettings::from_env(&app);
        s.password = "hunter2".into();
        let debug = format!("{s:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("<redacted>"));
        s.mailer = "nope".into();
        assert!(s.transport().is_err());
        s.mailer = "smtp".into();
        s.encryption = "weird".into();
        assert!(s.transport().is_err());
        s.encryption = "none".into();
        assert_eq!(s.transport().unwrap().name(), "smtp");
        s.encryption = "tls".into();
        assert_eq!(s.transport().unwrap().name(), "smtp");
        s.mailer = "array".into();
        assert!(s.transport().unwrap().mailbox().is_some());
    }

    #[test]
    fn a_login_over_plain_smtp_is_refused_unless_the_relay_is_local() {
        let mut app = Settings::from_env();
        app.env = "testing".into();
        let mut s = MailSettings::from_env(&app);
        s.mailer = "smtp".into();
        s.encryption = "none".into();
        s.username = "postmaster@example.com".into();
        s.password = "hunter2".into();
        s.host = "smtp.example.com".into();
        let err = s.transport().err().unwrap().to_string();
        assert!(err.contains("unencrypted"), "{err}");
        assert!(!err.contains("hunter2"), "{err}");
        // A relay on this machine, or no login at all: plain SMTP is fine.
        for host in ["127.0.0.1", "localhost", "::1"] {
            s.host = host.into();
            assert!(s.transport().is_ok(), "{host}");
        }
        s.host = "smtp.example.com".into();
        s.username = String::new();
        assert!(s.transport().is_ok());
        // With encryption the login is fine anywhere.
        s.username = "postmaster@example.com".into();
        s.encryption = "starttls".into();
        assert!(s.transport().is_ok());
    }

    #[test]
    fn a_bad_tls_ca_file_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = Settings::from_env();
        app.env = "testing".into();
        let mut s = MailSettings::from_env(&app);
        s.mailer = "smtp".into();
        s.encryption = "tls".into();
        s.tls_ca = Some(dir.path().join("missing.pem"));
        let err = s.transport().err().unwrap().to_string();
        assert!(err.contains("MAIL_TLS_CA: cannot read"), "{err}");
        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "not a certificate\n").unwrap();
        s.tls_ca = Some(empty);
        let err = s.transport().err().unwrap().to_string();
        assert!(err.contains("holds no PEM certificate"), "{err}");
        // Plain SMTP never reads the file.
        s.encryption = "none".into();
        assert!(s.transport().is_ok());
    }
}
