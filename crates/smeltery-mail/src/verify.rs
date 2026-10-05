//! [`VerifyEmail`]: the email verification mail.

use smeltery_mold::{Engine, Host, Template};
use smeltery_mold_macros::Mold;

use crate::address::Envelope;
use crate::mailable::Mailable;

/// The email verification mail `Auth::send_verification_email` sends when mail is installed. Its
/// template is part of this crate (compiled in), so it renders the same in every build.
#[derive(Clone, Debug, PartialEq, Eq, Mold)]
#[mold("mail/verify-email", crate = "smeltery_mold", dir = "views")]
pub struct VerifyEmail {
    /// `APP_NAME`.
    pub app_name: String,
    /// The recipient.
    pub email: String,
    /// The verification link.
    pub url: String,
    /// How long the link is valid.
    pub minutes: u64,
}

impl Mailable for VerifyEmail {
    fn envelope(&self) -> Envelope {
        Envelope::new()
            .to(self.email.as_str())
            .subject(format!("Verify your {} email address", self.app_name))
    }

    fn html(&self, _engine: &Engine, host: &dyn Host) -> Result<String, smeltery_mold::Error> {
        // The template lives in this crate, not in the app's views: always the compiled code.
        self.render_compiled(host)
    }
}
