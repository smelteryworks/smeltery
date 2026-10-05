//! [`Address`] and [`Envelope`]: who a mail goes to, and its subject.

use serde::{Deserialize, Serialize};

/// A mail address with an optional display name: `"ada@example.com"` or `"Ada <ada@example.com>"`.
///
/// ```
/// use smeltery_mail::Address;
///
/// let a: Address = "Ada Lovelace <ada@example.com>".into();
/// assert_eq!(a.email(), "ada@example.com");
/// assert_eq!(a.name(), Some("Ada Lovelace"));
/// assert_eq!(a.to_string(), "Ada Lovelace <ada@example.com>");
/// assert_eq!(Address::new("bob@example.com").named("Bob").to_string(), "Bob <bob@example.com>");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl Address {
    /// An address without a display name.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into().trim().to_owned(),
            name: None,
        }
    }

    /// The same address with display name `name`.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        self.name = (!name.trim().is_empty()).then(|| name.trim().to_owned());
        self
    }

    /// The mail address.
    pub fn email(&self) -> &str {
        &self.email
    }

    /// The display name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Parse `"Name <email>"` or a bare address.
    fn parse(text: &str) -> Self {
        let text = text.trim();
        if let Some(open) = text.rfind('<')
            && let Some(rest) = text.get(open + 1..)
            && let Some(email) = rest.strip_suffix('>')
        {
            let name = text
                .get(..open)
                .unwrap_or_default()
                .trim()
                .trim_matches('"');
            return Self::new(email).named(name);
        }
        Self::new(text)
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{name} <{}>", self.email),
            None => f.write_str(&self.email),
        }
    }
}

impl From<&str> for Address {
    fn from(text: &str) -> Self {
        Self::parse(text)
    }
}

impl From<String> for Address {
    fn from(text: String) -> Self {
        Self::parse(&text)
    }
}

impl From<&String> for Address {
    fn from(text: &String) -> Self {
        Self::parse(text)
    }
}

impl From<(&str, &str)> for Address {
    /// `(email, name)`.
    fn from((email, name): (&str, &str)) -> Self {
        Self::new(email).named(name)
    }
}

/// Who a mail goes to, who it is from, and its subject: what [`Mailable::envelope`](crate::Mailable::envelope)
/// returns.
///
/// ```
/// use smeltery_mail::Envelope;
///
/// let e = Envelope::new()
///     .to("ada@example.com")
///     .cc("Bob <bob@example.com>")
///     .reply_to("support@example.com")
///     .subject("Welcome");
/// assert_eq!(e.subject_text(), "Welcome");
/// assert_eq!(e.recipients().len(), 2);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) from: Option<Address>,
    #[serde(default)]
    pub(crate) to: Vec<Address>,
    #[serde(default)]
    pub(crate) cc: Vec<Address>,
    #[serde(default)]
    pub(crate) bcc: Vec<Address>,
    #[serde(default)]
    pub(crate) reply_to: Vec<Address>,
    #[serde(default)]
    pub(crate) subject: String,
}

impl Envelope {
    /// An empty envelope.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a recipient.
    #[must_use]
    pub fn to(mut self, address: impl Into<Address>) -> Self {
        self.to.push(address.into());
        self
    }

    /// Add a copy recipient.
    #[must_use]
    pub fn cc(mut self, address: impl Into<Address>) -> Self {
        self.cc.push(address.into());
        self
    }

    /// Add a blind copy recipient (not in the headers).
    #[must_use]
    pub fn bcc(mut self, address: impl Into<Address>) -> Self {
        self.bcc.push(address.into());
        self
    }

    /// Add a reply-to address.
    #[must_use]
    pub fn reply_to(mut self, address: impl Into<Address>) -> Self {
        self.reply_to.push(address.into());
        self
    }

    /// Send from `address` instead of `MAIL_FROM_ADDRESS` / `MAIL_FROM_NAME`.
    #[must_use]
    pub fn from(mut self, address: impl Into<Address>) -> Self {
        self.from = Some(address.into());
        self
    }

    /// The subject.
    #[must_use]
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = subject.into();
        self
    }

    /// The subject text.
    pub fn subject_text(&self) -> &str {
        &self.subject
    }

    /// The sender set with [`Envelope::from`].
    pub fn from_address(&self) -> Option<&Address> {
        self.from.as_ref()
    }

    /// The `to` recipients.
    pub fn to_addresses(&self) -> &[Address] {
        &self.to
    }

    /// The `cc` recipients.
    pub fn cc_addresses(&self) -> &[Address] {
        &self.cc
    }

    /// The `bcc` recipients.
    pub fn bcc_addresses(&self) -> &[Address] {
        &self.bcc
    }

    /// The reply-to addresses.
    pub fn reply_to_addresses(&self) -> &[Address] {
        &self.reply_to
    }

    /// Every recipient: `to`, `cc` and `bcc`.
    pub fn recipients(&self) -> Vec<&Address> {
        self.to.iter().chain(&self.cc).chain(&self.bcc).collect()
    }

    /// Whether `email` is among the recipients (case-insensitive).
    pub fn has_recipient(&self, email: &str) -> bool {
        self.recipients()
            .iter()
            .any(|a| a.email().eq_ignore_ascii_case(email))
    }
}
