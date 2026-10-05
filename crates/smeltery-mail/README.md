# smeltery-mail

Mail for the [Smeltery](https://github.com/smelteryworks/smeltery) framework: mail classes whose HTML body is a Mold
template, a `Mailer` service, and three transports: SMTP (lettre on Tokio, rustls with ring), the log, and a fake
mailbox for tests. Apps use it through the facade as `smeltery::mail` and install it with `.mail()`; the full
guide is the "Mail" section of the Smeltery README.

```rust
use smeltery::prelude::*;

#[derive(Mold)]
#[mold("mail/welcome")]                 // resources/views/mail/welcome.mold.html
pub struct Welcome {
    pub name: String,
    pub email: String,
}

impl Mailable for Welcome {
    fn envelope(&self) -> Envelope {
        Envelope::new().to(self.email.as_str()).subject(format!("Welcome, {}!", self.name))
    }
}

async fn send(mailer: Mailer) -> Result<&'static str> {
    mailer.send(Welcome { name: "Ada".into(), email: "ada@example.com".into() }).await?;
    Ok("sent")
}
# fn main() {}
```

What it has:

- `Mailable` (`envelope`, optional `text`, `attachments`, `html`), `Envelope` (`to`, `cc`, `bcc`, `reply_to`,
  `from`, `subject`), `Address`, `Attachment`, `Email` (a rendered mail, serializable) and `html_to_text`.
- `Mailer` (`send`, `render`, `send_email`, `mailbox`, `use_transport`, `Mailer::of`, `Mailer::fake`), a handler
  argument; `MailExt::mail()` installs it and sends password reset links as the `ResetPassword` mail and email
  verification links as the `VerifyEmail` mail.
- `MailSettings` from `MAIL_MAILER`, `MAIL_HOST`, `MAIL_PORT`, `MAIL_USERNAME`, `MAIL_PASSWORD`,
  `MAIL_ENCRYPTION`, `MAIL_TLS_CA`, `MAIL_TIMEOUT`, `MAIL_FROM_ADDRESS`, `MAIL_FROM_NAME`.
- SMTP over implicit TLS (`MAIL_ENCRYPTION=tls`) or STARTTLS with rustls (ring) and the platform's certificate
  verifier. `MAIL_TLS_CA` names a PEM file of extra CA certificates to trust, for a server with a private CA.
- `SmtpTransport` (a timeout per step and per send), `LogTransport` (bodies only under `APP_ENV` `local` or
  `testing`), `FakeTransport` with its `Mailbox`
  (`assert_sent`, `assert_sent_count`, `assert_sent_to`, `assert_nothing_sent`, `sent_of`), and the `Transport`
  trait for other transports.

Licensed under either of Apache License 2.0 or MIT license at your option.
