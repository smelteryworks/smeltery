//! [`Mailable`]: a mail class, a Mold template struct that also says who the mail goes to.

use smeltery_core::App;
use smeltery_mold::{Engine, Host, Template};

use crate::address::Envelope;
use crate::email::Attachment;

/// A mail class: a `#[derive(Mold)]` struct (its fields are the template's variables, the template is the HTML
/// body) that also names its recipients and subject.
///
/// ```
/// use smeltery::prelude::*;
///
/// #[derive(Mold)]
/// #[mold("mail/welcome")]               // resources/views/mail/welcome.mold.html
/// pub struct Welcome {
///     pub name: String,
///     pub email: String,
/// }
///
/// impl Mailable for Welcome {
///     fn envelope(&self) -> Envelope {
///         Envelope::new().to(&self.email).subject(format!("Welcome, {}!", self.name))
///     }
/// }
///
/// async fn register(mailer: Mailer) -> Result<&'static str> {
///     mailer.send(Welcome { name: "Ada".into(), email: "ada@example.com".into() }).await?;
///     Ok("sent")
/// }
/// # fn main() {}
/// ```
///
/// The plain-text part is generated from the HTML ([`html_to_text`](crate::html_to_text)) unless
/// [`Mailable::text`] returns one (for example from a second template, rendered with [`render`]).
pub trait Mailable: Template + Send + Sync + 'static {
    /// Recipients, subject, and optionally the sender.
    fn envelope(&self) -> Envelope;

    /// The plain-text part; `None` (the default) derives it from the HTML.
    ///
    /// ```
    /// # use smeltery::prelude::*;
    /// # use smeltery::mold::{self, Engine, Host};
    /// # #[derive(Mold)]
    /// # #[mold("mail/welcome")]
    /// # pub struct Welcome {
    /// #     pub name: String,
    /// #     pub email: String,
    /// # }
    /// # #[derive(Mold)]
    /// # #[mold("mail/welcome_text")]
    /// # pub struct WelcomeText {
    /// #     pub name: String,
    /// # }
    /// # impl Mailable for Welcome {
    /// #     fn envelope(&self) -> Envelope {
    /// #         Envelope::new().to(&self.email)
    /// #     }
    /// fn text(&self, engine: &Engine, host: &dyn Host) -> Result<Option<String>, mold::Error> {
    ///     smeltery::mail::render(&WelcomeText { name: self.name.clone() }, engine, host).map(Some)
    /// }
    /// # }
    /// # fn main() {}
    /// ```
    ///
    /// # Errors
    /// A template error.
    fn text(
        &self,
        engine: &Engine,
        host: &dyn Host,
    ) -> Result<Option<String>, smeltery_mold::Error> {
        let _ = (engine, host);
        Ok(None)
    }

    /// Files to attach.
    ///
    /// # Errors
    /// A file cannot be read.
    fn attachments(&self) -> smeltery_core::Result<Vec<Attachment>> {
        Ok(Vec::new())
    }

    /// The HTML part. The default renders the template like a view: with the runtime engine (hot reload) in
    /// debug builds and the compiled template in release builds.
    ///
    /// # Errors
    /// A template error.
    fn html(&self, engine: &Engine, host: &dyn Host) -> Result<String, smeltery_mold::Error> {
        render(self, engine, host)
    }
}

/// Render template `t` in the build's mode: the runtime engine `engine` in debug builds, the compiled template
/// in release builds (the same bytes either way).
///
/// # Errors
/// A template error.
pub fn render<T: Template + ?Sized>(
    t: &T,
    engine: &Engine,
    host: &dyn Host,
) -> Result<String, smeltery_mold::Error> {
    if cfg!(debug_assertions) {
        t.render_runtime_with(engine, host)
    } else {
        t.render_compiled(host)
    }
}

/// The host mail templates render with: no request, and `route()` gives absolute URLs (`APP_URL` + path),
/// since a mail is read outside the site.
pub(crate) struct MailHost {
    pub(crate) app: App,
}

impl Host for MailHost {
    fn route(&self, name: &str, params: &[(String, String)]) -> Result<String, String> {
        let params: Vec<(&str, &str)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let path = self.app.url(name, &params).map_err(|e| e.to_string())?;
        Ok(format!(
            "{}{path}",
            self.app.settings().url.trim_end_matches('/')
        ))
    }
}
