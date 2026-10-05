# Skill: send a mail

1. Generate the mail: `smeltery make:mail InvoicePaid`. It creates `app/mail/invoice_paid.rs` (a struct with
   `#[derive(Mold)]` implementing `Mailable`) and `resources/views/mail/invoice_paid.mold.html` (the HTML body; the
   struct's fields are its variables, `route("name")` gives absolute links), and adds `pub mod invoice_paid;` to
   `app/mail/mod.rs`.
2. Add the fields the mail needs and set the recipient and subject in `envelope()`:
   `Envelope::new().to(self.email.as_str()).subject("Your invoice is paid")`.
3. Send it from a handler with a `mailer: smeltery::mail::Mailer` argument:
   `mailer.send(InvoicePaid { … }).await?;` or from the queue with `mailer.queue(InvoicePaid { … }).await?;`.
4. Locally `MAIL_MAILER=log` writes each mail to the log; set `MAIL_MAILER=smtp` and the `MAIL_*` keys in `.env` to
   send for real.
5. Test it: tests run with a fake mailbox, so after the request check
   `Mailer::of(app.app()).unwrap().mailbox().unwrap().assert_sent::<InvoicePaid>(|mail, email| email.has_recipient("…"))`.
6. Run `smeltery test`.
