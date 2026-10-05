//! [`Email`]: a rendered message (what transports send), [`Attachment`]s, and the HTML-to-text fallback.

use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use smeltery_core::{Error, Result};

use crate::address::{Address, Envelope};

/// A file attached to a mail.
///
/// ```
/// use smeltery_mail::Attachment;
///
/// let a = Attachment::from_bytes("report.csv", b"a,b\n1,2\n".to_vec());
/// assert_eq!(a.name(), "report.csv");
/// assert_eq!(a.mime(), "text/csv");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    name: String,
    mime: String,
    #[serde(serialize_with = "b64_ser", deserialize_with = "b64_de")]
    data: Vec<u8>,
}

fn b64_ser<S: Serializer>(data: &[u8], s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(data))
}

fn b64_de<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<u8>, D::Error> {
    let text = String::deserialize(d)?;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(serde::de::Error::custom)
}

impl Attachment {
    /// `data` named `name`; the content type comes from the extension.
    pub fn from_bytes(name: impl Into<String>, data: Vec<u8>) -> Self {
        let name = name.into();
        let mime = mime_for(&name).to_owned();
        Self { name, mime, data }
    }

    /// The file at `path`, named by its file name. Reads the file (mailables are rendered on a blocking
    /// thread, so `attachments()` may call it).
    ///
    /// # Errors
    /// The file cannot be read.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read(path)
            .map_err(|e| Error::internal(format!("cannot attach {}: {e}", path.display())))?;
        let name = path.file_name().map_or_else(
            || "attachment".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        Ok(Self::from_bytes(name, data))
    }

    /// The same attachment with content type `mime`.
    #[must_use]
    pub fn with_mime(mut self, mime: impl Into<String>) -> Self {
        self.mime = mime.into();
        self
    }

    /// The file name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The content type.
    pub fn mime(&self) -> &str {
        &self.mime
    }

    /// The bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// A content type from a file name's extension (`application/octet-stream` when unknown).
pub(crate) fn mime_for(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "zip" => "application/zip",
        "ics" => "text/calendar",
        _ => "application/octet-stream",
    }
}

/// A rendered mail: envelope, HTML and text parts, attachments. Transports send it; the fake mailbox keeps it;
/// it serializes, so a queued mail is an `Email`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Email {
    envelope: Envelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default)]
    attachments: Vec<Attachment>,
    /// The mailable's type name, empty for mails built by hand.
    #[serde(default)]
    kind: String,
}

impl Email {
    /// A mail with `envelope` and no body yet.
    ///
    /// ```
    /// use smeltery_mail::{Email, Envelope};
    ///
    /// let mail = Email::new(Envelope::new().to("ops@example.com").subject("Disk full"))
    ///     .text("The disk is 98% full.");
    /// assert_eq!(mail.text_body(), Some("The disk is 98% full."));
    /// ```
    pub fn new(envelope: Envelope) -> Self {
        Self {
            envelope,
            html: None,
            text: None,
            attachments: Vec::new(),
            kind: String::new(),
        }
    }

    /// Set the HTML part.
    #[must_use]
    pub fn html(mut self, html: impl Into<String>) -> Self {
        self.html = Some(html.into());
        self
    }

    /// Set the plain-text part.
    #[must_use]
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Add an attachment.
    #[must_use]
    pub fn attach(mut self, attachment: Attachment) -> Self {
        self.attachments.push(attachment);
        self
    }

    pub(crate) fn with_kind(mut self, kind: &str) -> Self {
        self.kind = kind.to_owned();
        self
    }

    /// The envelope.
    pub fn envelope(&self) -> &Envelope {
        &self.envelope
    }

    /// The subject.
    pub fn subject(&self) -> &str {
        &self.envelope.subject
    }

    /// The HTML part.
    pub fn html_body(&self) -> Option<&str> {
        self.html.as_deref()
    }

    /// The plain-text part.
    pub fn text_body(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// The attachments.
    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    /// The type name of the mailable it was rendered from (`std::any::type_name`), empty for mails built by
    /// hand.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Whether `email` is a recipient (`to`, `cc` or `bcc`).
    pub fn has_recipient(&self, email: &str) -> bool {
        self.envelope.has_recipient(email)
    }

    /// The lettre message, from `default_from` unless the envelope names a sender.
    pub(crate) fn to_message(&self, default_from: &Address) -> Result<lettre::Message> {
        use lettre::message::{Mailbox, MultiPart, SinglePart, header::ContentType};

        fn mailbox(a: &Address) -> Result<Mailbox> {
            let email = a.email().parse().map_err(|e| {
                Error::internal(format!("invalid mail address `{}`: {e}", a.email()))
            })?;
            Ok(Mailbox::new(a.name().map(str::to_owned), email))
        }
        let env = &self.envelope;
        if env.to.is_empty() && env.cc.is_empty() && env.bcc.is_empty() {
            return Err(Error::internal("a mail needs at least one recipient"));
        }
        let mut builder = lettre::Message::builder()
            .from(mailbox(env.from.as_ref().unwrap_or(default_from))?)
            .subject(env.subject.clone());
        for a in &env.to {
            builder = builder.to(mailbox(a)?);
        }
        for a in &env.cc {
            builder = builder.cc(mailbox(a)?);
        }
        for a in &env.bcc {
            builder = builder.bcc(mailbox(a)?);
        }
        for a in &env.reply_to {
            builder = builder.reply_to(mailbox(a)?);
        }
        let text = self.text.clone().unwrap_or_default();
        let body = match &self.html {
            Some(html) => MultiPart::alternative_plain_html(text, html.clone()),
            None => MultiPart::mixed().singlepart(SinglePart::plain(text)),
        };
        let body = if self.attachments.is_empty() {
            body
        } else {
            let mut mixed = MultiPart::mixed().multipart(body);
            for a in &self.attachments {
                let content_type = ContentType::parse(&a.mime).unwrap_or(
                    ContentType::parse("application/octet-stream").map_err(Error::other)?,
                );
                mixed = mixed.singlepart(
                    lettre::message::Attachment::new(a.name.clone())
                        .body(a.data.clone(), content_type),
                );
            }
            mixed
        };
        builder
            .multipart(body)
            .map_err(|e| Error::internal(format!("cannot build the mail: {e}")))
    }
}

/// Plain text from HTML: tags dropped, block ends as line breaks, links as `text (url)`, entities decoded,
/// `<head>`, `<style>` and `<script>` left out.
///
/// ```
/// let text = smeltery_mail::html_to_text(
///     "<h1>Hi &amp; welcome</h1><p>Click <a href=\"https://x.test/a\">here</a>.</p><ul><li>one</li><li>two</li></ul>",
/// );
/// assert_eq!(text, "Hi & welcome\n\nClick here (https://x.test/a).\n\n- one\n- two");
/// ```
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    let mut skip_until: Option<&str> = None;
    let mut href: Vec<Option<String>> = Vec::new();
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skip_until.is_none() {
                push_text(&mut out, rest);
            }
            break;
        };
        if skip_until.is_none() {
            push_text(&mut out, rest.get(..lt).unwrap_or_default());
        }
        let after = rest.get(lt..).unwrap_or_default();
        let gt = after.find('>').unwrap_or(after.len().saturating_sub(1));
        let tag = after.get(1..gt).unwrap_or_default();
        rest = after.get(gt + 1..).unwrap_or_default();
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if let Some(end) = skip_until {
            if closing && name == end {
                skip_until = None;
            }
            continue;
        }
        match (name.as_str(), closing) {
            ("head" | "style" | "script" | "title", false) => {
                skip_until = Some(match name.as_str() {
                    "head" => "head",
                    "style" => "style",
                    "script" => "script",
                    _ => "title",
                });
            }
            ("br", _) => out.push('\n'),
            ("li", false) => {
                newline(&mut out, 1);
                out.push_str("- ");
            }
            (
                "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol" | "table"
                | "tr" | "hr" | "blockquote",
                _,
            ) => newline(&mut out, 2),
            ("li", true) => newline(&mut out, 1),
            ("td" | "th", true) => out.push(' '),
            ("a", false) => href.push(attr(tag, "href")),
            ("a", true) => {
                if let Some(Some(url)) = href.pop()
                    && !url.starts_with('#')
                    && !out.trim_end().ends_with(&url)
                {
                    out.push_str(" (");
                    out.push_str(&url);
                    out.push(')');
                }
            }
            _ => {}
        }
    }
    // Trim every line and collapse blank runs.
    let mut text = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !text.is_empty() {
            text.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        text.push_str(&line);
    }
    text
}

fn newline(out: &mut String, n: usize) {
    let have = out.chars().rev().take_while(|c| *c == '\n').count();
    for _ in have..n {
        out.push('\n');
    }
}

fn push_text(out: &mut String, text: &str) {
    // Source line breaks are spaces in HTML.
    let decoded = decode_entities(&text.replace(['\n', '\r', '\t'], " "));
    out.push_str(&decoded);
}

/// The value of attribute `name` in a tag's inside.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))? + name.len() + 1;
    let rest = tag.get(at..)?;
    let value = match rest.chars().next()? {
        q @ ('"' | '\'') => rest.get(1..)?.split(q).next()?,
        _ => rest.split(|c: char| c.is_whitespace()).next()?,
    };
    Some(decode_entities(value))
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_from_html() {
        let html = "<html><head><title>T</title><style>p{color:red}</style></head><body>\n<p>Hello\n  <b>Ada</b>,</p>\
<table><tr><td>a</td><td>b</td></tr></table><p><a href='https://x.test/reset?a=1&amp;b=2'>Reset</a></p>\
<p><a href=\"https://x.test\">https://x.test</a><br>Bye</p><script>alert(1)</script></body></html>";
        assert_eq!(
            html_to_text(html),
            "Hello Ada,\n\na b\n\nReset (https://x.test/reset?a=1&b=2)\n\nhttps://x.test\nBye"
        );
        assert_eq!(html_to_text("plain &lt;text&gt;"), "plain <text>");
        assert_eq!(html_to_text("<p>unclosed"), "unclosed");
    }

    #[test]
    fn mime_and_attachments_round_trip() {
        assert_eq!(mime_for("a.PDF"), "application/pdf");
        assert_eq!(mime_for("noext"), "application/octet-stream");
        let mail = Email::new(Envelope::new().to("a@b.test").subject("s"))
            .text("t")
            .attach(Attachment::from_bytes("x.bin", vec![0, 1, 255]));
        let json = serde_json::to_string(&mail).unwrap();
        assert!(json.contains("\"data\":\"AAH/\""), "{json}");
        assert_eq!(serde_json::from_str::<Email>(&json).unwrap(), mail);
    }

    #[test]
    fn message_needs_valid_addresses_and_a_recipient() {
        let from = Address::new("app@example.com").named("App");
        let ok = Email::new(
            Envelope::new()
                .to("a@b.test")
                .bcc("hidden@b.test")
                .subject("Hi"),
        )
        .html("<p>x</p>")
        .text("x");
        let msg = ok.to_message(&from).unwrap();
        let raw = String::from_utf8(msg.formatted()).unwrap();
        assert!(raw.contains("Subject: Hi"), "{raw}");
        assert!(raw.contains("From: App <app@example.com>"), "{raw}");
        assert!(!raw.contains("hidden@b.test"), "bcc is not a header: {raw}");
        assert_eq!(msg.envelope().to().len(), 2);
        assert!(
            Email::new(Envelope::new().subject("x"))
                .to_message(&from)
                .is_err()
        );
        assert!(
            Email::new(Envelope::new().to("not an address"))
                .to_message(&from)
                .is_err()
        );
    }
}
