//! The `InvoicePaid` mail; its HTML body is `resources/views/mail/invoice_paid.mold.html`.

use smeltery::prelude::*;

/// Fields are the template's variables. Send it with `mailer.send(InvoicePaid { … }).await?`.
#[derive(Mold)]
#[mold("mail/invoice_paid")]
pub struct InvoicePaid {
    /// The recipient's name.
    pub name: String,
    /// The recipient's address.
    pub email: String,
}

impl Mailable for InvoicePaid {
    fn envelope(&self) -> Envelope {
        Envelope::new()
            .to(self.email.as_str())
            .subject("Invoice paid")
    }
}
