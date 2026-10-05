# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- `Mailable` (a Mold template struct with `envelope`, optional `text`, `attachments`, `html`), `render`,
  `Envelope`, `Address`, `Attachment`, `Email` (serializable rendered mail), `html_to_text`.
- `Mailer` (`of`, `fake`, `send`, `render`, `send_email`, `mailbox`, `use_transport`, `transport_name`,
  `settings`), usable as a handler argument; mail templates render like views and `route()` gives absolute URLs.
- `MailExt::mail()`: reads `MailSettings` at boot and installs a `ResetNotifier`, so password reset links are sent
  as the `ResetPassword` mail, and email verification links as the `VerifyEmail` mail (both templates compiled into
  the crate); `Mailer::fake` sends them too.
- Transports: `SmtpTransport` (lettre 0.11 on Tokio, rustls with ring and the platform verifier; `tls`,
  `starttls`, `none`; per-step and per-send timeouts; `MAIL_TLS_CA` / `MailSettings::tls_ca`, a PEM file of extra
  CA certificates trusted for the SMTP server), `LogTransport` (`LogTransport::new` / `Default`,
  `LogTransport::without_bodies`, `MailSettings::log_bodies`), `FakeTransport` with `Mailbox` assertions; the
  `Transport` trait. `APP_ENV=testing` always uses the fake.
- A send that fails to connect or in the TLS handshake (an untrusted certificate) reports that the connection to the
  mail server failed; a reply from the server reports that the server refused the mail.

### Security
- `MAIL_MAILER=log` writes mail bodies to the log only in local development (`APP_ENV` `local` or `testing` with an
  `APP_URL` on this machine, `Settings::is_local_development`); elsewhere it logs the sender, recipients and subject
  and notes that the body was withheld (a body can carry a password reset link). `.mail()` logs a warning at boot
  when `MAIL_MAILER=log` outside local development: mails are not sent.
- `MAIL_ENCRYPTION=none` with a `MAIL_USERNAME` is refused (the SMTP transport fails to build, so the app does not
  boot) unless `MAIL_HOST` is on this machine: lettre sends the login over a plain connection, so the password
  would cross the network in clear.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
