//! Mail through an app: rendering, the fake mailbox, the log transport, SMTP against an in-process server,
//! and the password reset and email verification mails.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smeltery_core::auth::passwords::ResetNotifier;
use smeltery_core::auth::verification::VerificationNotifier;
use smeltery_core::testing::TestApp;
use smeltery_core::{AppBuilder, Result};
use smeltery_mail::{
    Address, Attachment, Email, Envelope, LogTransport, MailExt, MailSettings, Mailable, Mailer,
    ResetPassword, SmtpTransport, VerifyEmail, render,
};
use smeltery_mold::{Engine, Host, NoHost, Template};
use smeltery_mold_macros::Mold;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

#[derive(Clone, Debug, Mold)]
#[mold(
    "mail/welcome",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
pub struct Welcome {
    pub name: String,
    pub plan: String,
    pub email: String,
}

impl Mailable for Welcome {
    fn envelope(&self) -> Envelope {
        Envelope::new()
            .to(Address::new(&self.email).named(&self.name))
            .bcc("audit@example.com")
            .reply_to("support@example.com")
            .subject(format!("Welcome, {}!", self.name))
    }
}

#[derive(Mold)]
#[mold(
    "mail/welcome-text",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
struct WelcomeText {
    name: String,
}

/// The same mail with its own text template and an attachment.
#[derive(Clone, Debug, Mold)]
#[mold(
    "mail/welcome",
    crate = "smeltery_mold",
    dir = "tests/app/resources/views"
)]
pub struct WelcomeWithText {
    pub name: String,
    pub plan: String,
    pub email: String,
}

impl Mailable for WelcomeWithText {
    fn envelope(&self) -> Envelope {
        Envelope::new()
            .from("Billing <billing@example.com>")
            .to(self.email.as_str())
            .subject("Your plan")
    }

    fn text(
        &self,
        engine: &Engine,
        host: &dyn Host,
    ) -> std::result::Result<Option<String>, smeltery_mold::Error> {
        render(
            &WelcomeText {
                name: self.name.clone(),
            },
            engine,
            host,
        )
        .map(Some)
    }

    fn attachments(&self) -> Result<Vec<Attachment>> {
        Ok(vec![Attachment::from_bytes(
            "plan.csv",
            b"plan\npro\n".to_vec(),
        )])
    }
}

async fn dashboard() -> &'static str {
    "dash"
}

async fn send_welcome(mailer: Mailer) -> Result<&'static str> {
    mailer
        .send(Welcome {
            name: "Ada".into(),
            plan: "pro".into(),
            email: "ada@example.com".into(),
        })
        .await?;
    Ok("sent")
}

fn app() -> TestApp {
    TestApp::new(|mut b: AppBuilder| {
        b.settings_mut().root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        b.settings_mut().url = "https://app.test/".into();
        b.settings_mut().name = "Demo".into();
        b.mail().routes(|r| {
            r.get("/dashboard", dashboard).name("dashboard");
            r.get("/welcome", send_welcome);
        })
    })
}

fn welcome() -> Welcome {
    Welcome {
        name: "Ada".into(),
        plan: "pro".into(),
        email: "ada@example.com".into(),
    }
}

#[test]
fn both_render_modes_are_identical() {
    let engine = Engine::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app/resources/views"),
    );
    let w = welcome();
    // `route()` needs a host with routes; the plain host renders everything else.
    let err = w.render_compiled(&NoHost).unwrap_err().to_string();
    assert!(err.contains("route"), "{err}");
    let reset = ResetPassword {
        app_name: "Demo".into(),
        email: "a@b.test".into(),
        url: "https://app.test/reset-password/t?email=a%40b.test".into(),
        minutes: 60,
    };
    let crate_views = Engine::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("views"));
    assert_eq!(
        reset.render_runtime_with(&crate_views, &NoHost).unwrap(),
        reset.render_compiled(&NoHost).unwrap()
    );
    let text = WelcomeText { name: "Ada".into() };
    assert_eq!(
        text.render_runtime_with(&engine, &NoHost).unwrap(),
        text.render_compiled(&NoHost).unwrap()
    );
}

#[test]
fn a_handler_sends_through_the_fake_mailbox() {
    let app = app();
    let mailer = Mailer::of(app.app()).unwrap();
    assert_eq!(
        mailer.transport_name(),
        "fake",
        "APP_ENV=testing never sends real mail"
    );
    let mailbox = mailer.mailbox().unwrap();
    mailbox.assert_nothing_sent();

    assert_eq!(app.get("/welcome").text(), "sent");
    mailbox.assert_sent_count::<Welcome>(1);
    mailbox.assert_sent::<Welcome>(|m, e| m.name == "Ada" && e.has_recipient("audit@example.com"));
    mailbox.assert_sent_to("ada@example.com");
    let (_, email) = mailbox.sent_of::<Welcome>().pop().unwrap();
    assert_eq!(email.subject(), "Welcome, Ada!");
    let html = email.html_body().unwrap();
    assert!(html.contains("<h1>Welcome, Ada!</h1>"), "{html}");
    assert!(
        html.contains("<a href=\"https://app.test/dashboard\">"),
        "route() is absolute in mail: {html}"
    );
    assert_eq!(
        email.text_body().unwrap(),
        "Welcome, Ada!\n\nYour plan: PRO.\n\nOpen your dashboard (https://app.test/dashboard)"
    );
    assert!(email.kind().ends_with("Welcome"));
    assert_eq!(
        email.envelope().reply_to_addresses()[0].email(),
        "support@example.com"
    );

    mailbox.clear();
    assert!(mailbox.is_empty());
}

#[test]
fn text_templates_and_attachments() {
    let app = app();
    let mailer = Mailer::of(app.app()).unwrap();
    let mailbox = mailer.mailbox().unwrap();
    app.block_on(mailer.send(WelcomeWithText {
        name: "Bo".into(),
        plan: "free".into(),
        email: "bo@example.com".into(),
    }))
    .unwrap();
    let email = &mailbox.emails()[0];
    assert_eq!(email.text_body(), Some("Hi Bo, welcome aboard.\n"));
    assert_eq!(email.attachments()[0].name(), "plan.csv");
    assert_eq!(email.attachments()[0].mime(), "text/csv");
    assert_eq!(
        email.envelope().from_address().unwrap().email(),
        "billing@example.com"
    );
    // A hand-built mail goes through the same transport.
    app.block_on(
        mailer
            .send_email(Email::new(Envelope::new().to("x@example.com").subject("Plain")).text("t")),
    )
    .unwrap();
    assert_eq!(mailbox.len(), 2);
    assert!(mailbox.sent()[1].mailable::<Welcome>().is_none());
}

#[test]
fn fake_installs_mail_on_an_app_without_it() {
    let app = TestApp::new(|b| b);
    assert!(Mailer::of(app.app()).is_err());
    let mailbox = Mailer::fake(app.app());
    let mailer = Mailer::of(app.app()).unwrap();
    app.block_on(
        mailer.send_email(Email::new(Envelope::new().to("a@b.test").subject("x")).text("y")),
    )
    .unwrap();
    assert_eq!(mailbox.len(), 1);
}

#[test]
fn verification_links_become_mail_rendered_the_same_in_both_modes() {
    let app = app();
    let mailbox = Mailer::of(app.app()).unwrap().mailbox().unwrap();
    let notifier = app
        .app()
        .service::<Arc<dyn VerificationNotifier>>()
        .expect(".mail() installs the verification notifier");
    let url = "https://app.test/email/verify/7/abc?expires=1&signature=sig";
    app.block_on(notifier.send(app.app(), "ada@example.com", url))
        .unwrap();
    mailbox.assert_sent::<VerifyEmail>(|m, e| {
        m.email == "ada@example.com"
            && m.minutes == app.app().settings().verification_expire.as_secs() / 60
            && e.has_recipient("ada@example.com")
            && e.subject() == "Verify your Demo email address"
            && e.html_body()
                .unwrap()
                .contains(&format!("href=\"{}\"", url.replace('&', "&amp;")))
            && e.text_body()
                .unwrap()
                .contains(&format!("Verify email address ({url})"))
    });
    let mail = VerifyEmail {
        app_name: "Demo".into(),
        email: "a@b.test".into(),
        url: url.into(),
        minutes: 60,
    };
    let crate_views = Engine::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("views"));
    assert_eq!(
        mail.render_runtime_with(&crate_views, &NoHost).unwrap(),
        mail.render_compiled(&NoHost).unwrap()
    );
}

#[test]
fn password_reset_links_become_mail() {
    let app = app();
    let mailbox = Mailer::of(app.app()).unwrap().mailbox().unwrap();
    let notifier = app.app().service::<Arc<dyn ResetNotifier>>().unwrap();
    let url = "https://app.test/reset-password/tok123?email=ada%40example.com";
    app.block_on(notifier.send(app.app(), "ada@example.com", url))
        .unwrap();
    mailbox.assert_sent::<ResetPassword>(|m, e| {
        m.email == "ada@example.com"
            && m.minutes == 60
            && e.subject() == "Reset your Demo password"
            && e.html_body()
                .unwrap()
                .contains(&format!("href=\"{}\"", url.replace('&', "&amp;")))
            && e.text_body()
                .unwrap()
                .contains(&format!("Reset password ({url})"))
    });
}

/// A `MakeWriter` collecting log output.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn capture() -> (Logs, tracing::subscriber::DefaultGuard) {
    let logs = Logs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (logs, guard)
}

#[test]
fn the_log_transport_writes_the_mail() {
    let app = app();
    let mailer = Mailer::of(app.app()).unwrap();
    mailer.use_transport(Arc::new(LogTransport::new()));
    let (logs, _guard) = capture();
    app.block_on(mailer.send(welcome())).unwrap();
    let out = logs.text();
    assert!(out.contains("smeltery::mail"), "{out}");
    assert!(out.contains("subject=Welcome, Ada!"), "{out}");
    assert!(out.contains("to=Ada <ada@example.com>"), "{out}");
    assert!(
        out.contains("Open your dashboard (https://app.test/dashboard)"),
        "{out}"
    );
    assert!(mailer.mailbox().is_none());
}

/// Boot an app with `.mail()` under `APP_ENV=env` (`MAIL_MAILER` unset: `log`), send a reset
/// mail carrying `token` through the log transport; the log.
fn reset_mail_log(env: &str, token: &str) -> String {
    reset_mail_log_at(env, "http://127.0.0.1:8000", token)
}

/// [`reset_mail_log`] with `APP_URL=url`.
fn reset_mail_log_at(env: &str, url: &str, token: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (logs, guard) = capture();
    rt.block_on(async {
        let mut builder = AppBuilder::new(smeltery_core::config::Settings::from_env()).mail();
        builder.settings_mut().env = env.to_owned();
        builder.settings_mut().url = url.to_owned();
        builder.settings_mut().database_url = String::new();
        let app = builder.build().await.unwrap().app;
        let mailer = Mailer::of(&app).unwrap();
        assert_eq!(mailer.transport_name(), "log");
        mailer
            .send(ResetPassword {
                app_name: "Demo".into(),
                email: "ada@example.com".into(),
                url: format!("https://app.example/reset-password/{token}?email=ada%40example.com"),
                minutes: 60,
            })
            .await
            .unwrap();
    });
    drop(guard);
    logs.text()
}

#[test]
fn the_log_transport_withholds_bodies_outside_local_and_testing() {
    assert!(
        std::env::var("MAIL_MAILER").is_err(),
        "the test needs MAIL_MAILER unset"
    );
    const TOKEN: &str = "Zr4nd0mResetT0kenZr4nd0mResetT0kenZr4nd0mResetT0kenZr4nd0mRe";
    for env in ["production", "staging"] {
        let out = reset_mail_log(env, TOKEN);
        assert!(
            !out.contains(TOKEN),
            "{env}: the reset token reached the log:\n{out}"
        );
        assert!(out.contains("to=ada@example.com"), "{env}: {out}");
        assert!(out.contains("subject="), "{env}: {out}");
        assert!(out.contains("body withheld"), "{env}: {out}");
    }
    let out = reset_mail_log("local", TOKEN);
    assert!(out.contains(TOKEN), "local shows the whole mail: {out}");
}

#[test]
fn the_log_transport_withholds_bodies_when_app_url_is_not_this_machine() {
    // A server whose .env lacks or mistypes APP_ENV still has its public APP_URL.
    const TOKEN: &str = "Zr4nd0mResetT0kenForAPublicUrlZr4nd0mResetT0kenForAPublicUrl";
    // (`testing` always uses the fake mailer, so only `local` reaches the log transport.)
    let out = reset_mail_log_at("local", "https://app.example.com", TOKEN);
    assert!(
        !out.contains(TOKEN),
        "the reset token reached the log: {out}"
    );
    assert!(out.contains("body withheld"), "{out}");
}

/// Boot an app with `.mail()` under `APP_ENV=env` (`MAIL_MAILER` unset: `log`); the boot log.
fn boot_log(env: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (logs, guard) = capture();
    rt.block_on(async {
        let mut builder = AppBuilder::new(smeltery_core::config::Settings::from_env()).mail();
        builder.settings_mut().env = env.to_owned();
        builder.settings_mut().database_url = String::new();
        builder.build().await.unwrap();
    });
    drop(guard);
    logs.text()
}

#[test]
fn the_log_mailer_outside_local_warns_at_boot() {
    assert!(
        std::env::var("MAIL_MAILER").is_err(),
        "the test needs MAIL_MAILER unset"
    );
    for env in ["production", "staging"] {
        let out = boot_log(env);
        assert!(
            out.contains("WARN") && out.contains("MAIL_MAILER=log"),
            "{env}: {out}"
        );
    }
    let out = boot_log("local");
    assert!(!out.contains("MAIL_MAILER=log"), "{out}");
}

// ---------------------------------------------------------------- SMTP

/// What the in-process SMTP server saw.
#[derive(Clone, Default)]
struct Seen {
    commands: Arc<Mutex<Vec<String>>>,
    auth: Arc<Mutex<Vec<String>>>,
    messages: Arc<Mutex<Vec<String>>>,
}

/// A tiny SMTP server on 127.0.0.1:0: EHLO, AUTH PLAIN/LOGIN (accepting `user` / `pass` only), MAIL, RCPT,
/// DATA, QUIT. `silent` accepts connections and never answers.
async fn smtp_server(silent: bool) -> (u16, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Seen::default();
    let s = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let s = s.clone();
            tokio::spawn(async move {
                if silent {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    drop(socket);
                    return;
                }
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
                let mut login_user: Option<String> = None;
                let mut login_step = 0;
                while let Ok(Some(line)) = lines.next_line().await {
                    let upper = line.to_ascii_uppercase();
                    if login_step > 0 {
                        use base64::Engine as _;
                        let decoded = String::from_utf8(
                            base64::engine::general_purpose::STANDARD
                                .decode(line.trim())
                                .unwrap_or_default(),
                        )
                        .unwrap_or_default();
                        if login_step == 1 {
                            login_user = Some(decoded);
                            login_step = 2;
                            write.write_all(b"334 UGFzc3dvcmQ6\r\n").await.unwrap();
                        } else {
                            login_step = 0;
                            let creds =
                                format!("{}:{decoded}", login_user.take().unwrap_or_default());
                            let ok = creds == "user:pass";
                            s.auth.lock().unwrap().push(creds);
                            write
                                .write_all(if ok { b"235 ok\r\n" } else { b"535 no\r\n" })
                                .await
                                .unwrap();
                        }
                        continue;
                    }
                    s.commands
                        .lock()
                        .unwrap()
                        .push(upper.split(' ').next().unwrap_or_default().to_owned());
                    let reply: &[u8] = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        b"250-fake\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n"
                    } else if let Some(rest) = upper.strip_prefix("AUTH PLAIN") {
                        use base64::Engine as _;
                        let arg = line
                            .get(line.len() - rest.len()..)
                            .unwrap_or_default()
                            .trim();
                        let raw = base64::engine::general_purpose::STANDARD
                            .decode(arg)
                            .unwrap_or_default();
                        let parts: Vec<String> = raw
                            .split(|b| *b == 0)
                            .map(|p| String::from_utf8_lossy(p).into_owned())
                            .collect();
                        let creds = format!(
                            "{}:{}",
                            parts.get(1).cloned().unwrap_or_default(),
                            parts.get(2).cloned().unwrap_or_default()
                        );
                        let ok = creds == "user:pass";
                        s.auth.lock().unwrap().push(creds);
                        if ok { b"235 ok\r\n" } else { b"535 no\r\n" }
                    } else if upper.starts_with("AUTH LOGIN") {
                        login_step = 1;
                        b"334 VXNlcm5hbWU6\r\n"
                    } else if upper.starts_with("DATA") {
                        write.write_all(b"354 go\r\n").await.unwrap();
                        let mut message = String::new();
                        while let Ok(Some(l)) = lines.next_line().await {
                            if l == "." {
                                break;
                            }
                            message.push_str(&l);
                            message.push('\n');
                        }
                        s.messages.lock().unwrap().push(message);
                        b"250 queued\r\n"
                    } else if upper.starts_with("QUIT") {
                        write.write_all(b"221 bye\r\n").await.unwrap();
                        return;
                    } else if upper.starts_with("MAIL")
                        || upper.starts_with("RCPT")
                        || upper.starts_with("RSET")
                        || upper.starts_with("NOOP")
                    {
                        b"250 ok\r\n"
                    } else {
                        b"500 what\r\n"
                    };
                    write.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, seen)
}

fn smtp_settings(
    app: &TestApp,
    port: u16,
    user: &str,
    password: &str,
    timeout: Duration,
) -> MailSettings {
    let mut s = MailSettings::from_env(app.app().settings());
    s.mailer = "smtp".into();
    s.host = "127.0.0.1".into();
    s.port = port;
    s.encryption = "none".into();
    s.username = user.into();
    s.password = password.into();
    s.timeout = timeout;
    s
}

#[test]
fn smtp_delivers_with_authentication() {
    let app = app();
    let mailer = Mailer::of(app.app()).unwrap();
    app.block_on(async {
        let (port, seen) = smtp_server(false).await;
        let settings = smtp_settings(&app, port, "user", "pass", Duration::from_secs(5));
        mailer.use_transport(Arc::new(SmtpTransport::new(&settings).unwrap()));
        assert_eq!(mailer.transport_name(), "smtp");
        mailer.send(welcome()).await.unwrap();
        assert_eq!(seen.auth.lock().unwrap().as_slice(), ["user:pass"]);
        let commands = seen.commands.lock().unwrap().clone();
        assert!(commands.starts_with(&["EHLO".to_owned()]), "{commands:?}");
        assert_eq!(
            commands.iter().filter(|c| *c == "RCPT").count(),
            2,
            "to + bcc: {commands:?}"
        );
        let message = seen.messages.lock().unwrap()[0].clone();
        assert!(message.contains("Subject: Welcome, Ada!"), "{message}");
        assert!(message.contains("To: Ada <ada@example.com>"), "{message}");
        assert!(
            !message.contains("audit@example.com"),
            "bcc stays out of the headers"
        );
        assert!(message.contains("multipart/alternative"), "{message}");
    });
}

#[test]
fn smtp_failures_are_errors_without_secrets() {
    let app = app();
    let mailer = Mailer::of(app.app()).unwrap();
    let (logs, _guard) = capture();
    app.block_on(async {
        let (port, seen) = smtp_server(false).await;
        let settings = smtp_settings(
            &app,
            port,
            "user",
            "wrong-Secret-123",
            Duration::from_secs(5),
        );
        mailer.use_transport(Arc::new(SmtpTransport::new(&settings).unwrap()));
        let err = mailer.send(welcome()).await.unwrap_err().to_string();
        assert!(err.contains("refused"), "{err}");
        assert!(!err.contains("wrong-Secret-123"), "{err}");
        assert_eq!(seen.messages.lock().unwrap().len(), 0);
    });
    let out = logs.text();
    assert!(out.contains("mail not sent"), "{out}");
    assert!(!out.contains("wrong-Secret-123"), "{out}");
}

#[test]
fn smtp_gives_up_on_a_silent_server() {
    let app = app();
    let mailer = Mailer::of(app.app()).unwrap();
    app.block_on(async {
        let (port, _) = smtp_server(true).await;
        let settings = smtp_settings(&app, port, "", "", Duration::from_secs(1));
        mailer.use_transport(Arc::new(SmtpTransport::new(&settings).unwrap()));
        let started = Instant::now();
        let err = mailer.send(welcome()).await.unwrap_err().to_string();
        // Four times MAIL_TIMEOUT at most.
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "{:?}",
            started.elapsed()
        );
        assert!(err.contains("did not answer within 4 s"), "{err}");
    });
}

#[test]
fn the_app_is_freed_after_mail_was_sent() {
    let app = app();
    assert_eq!(app.get("/welcome").text(), "sent");
    let weak = app.app().downgrade();
    drop(app);
    assert!(
        weak.upgrade().is_none(),
        "a mail service kept the app alive"
    );
}
