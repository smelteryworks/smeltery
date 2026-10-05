#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod address;
mod email;
mod mailable;
mod mailer;
mod reset;
mod transport;
mod verify;

pub use address::{Address, Envelope};
pub use email::{Attachment, Email, html_to_text};
pub use mailable::{Mailable, render};
pub use mailer::{MailExt, Mailer};
pub use reset::ResetPassword;
pub use transport::{
    FakeTransport, LogTransport, MailSettings, Mailbox, SentMail, SmtpTransport, Transport,
};
pub use verify::VerifyEmail;
