//! Validation ("Assay"): `#[derive(Validate)]` rules on form structs, the [`Valid`] extractor and
//! [`ValidationErrors`].
//!
//! ```
//! use smeltery::prelude::*;
//! # use serde::Deserialize;
//!
//! #[derive(Deserialize, Validate)]
//! pub struct RegisterForm {
//!     #[validate(required, max = 255)]
//!     pub name: String,
//!     #[validate(required, email, unique(table = "users", column = "email"))]
//!     pub email: String,
//!     #[validate(required, min = 8, confirmed)]
//!     pub password: String,
//!     pub password_confirmation: String,
//! }
//!
//! async fn store(Valid(form): Valid<RegisterForm>) -> Redirect {
//!     Redirect::to("/")
//! }
//! # fn main() {}
//! ```
//!
//! A failed validation answers 422 JSON to JSON requests; on web routes, other requests are
//! redirected back with the errors and the old input flashed to the session, where `@error`
//! and `old()` read them.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;

use axum::extract::FromRequest;
use axum::response::{IntoResponse, Response};
use http::{StatusCode, header};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::db::Db;
use crate::error::{Error, Result};

pub mod rules;

/// The raw input of a request, field â†’ text (JSON scalars as text, arrays and objects as JSON).
pub type Input = BTreeMap<String, String>;

/// Validation messages by field: `{"email": ["The email field is required."]}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ValidationErrors(BTreeMap<String, Vec<String>>);

impl ValidationErrors {
    /// No errors.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a message for `field`.
    pub fn add(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.0.entry(field.into()).or_default().push(message.into());
    }

    /// The first message for `field`.
    pub fn first(&self, field: &str) -> Option<&str> {
        self.0
            .get(field)
            .and_then(|m| m.first())
            .map(String::as_str)
    }

    /// Every message for `field`.
    pub fn get(&self, field: &str) -> &[String] {
        self.0.get(field).map_or(&[], Vec::as_slice)
    }

    /// Whether `field` has a message.
    pub fn has(&self, field: &str) -> bool {
        !self.get(field).is_empty()
    }

    /// Whether there is no message at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The number of fields with messages.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Every field with its messages, by field name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    /// `Ok(())` when empty, else `Err(self)`.
    ///
    /// # Errors
    /// There is at least one message.
    pub fn into_result(self) -> std::result::Result<(), Self> {
        if self.is_empty() { Ok(()) } else { Err(self) }
    }

    /// Every field of `other` with `other`'s messages in place of this one's.
    fn replace_from(&mut self, other: Self) {
        for (field, messages) in other.0 {
            self.0.insert(field, messages);
        }
    }

    pub(crate) fn into_map(self) -> BTreeMap<String, Vec<String>> {
        self.0
    }
}

/// A failed validation, carried by [`Error::Validation`]: the status (422, or 429 for login
/// throttling), the messages and the input to flash back as old input.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Invalid {
    /// 422, or 429 for too many login attempts.
    pub status: StatusCode,
    /// The summary for JSON clients.
    pub message: String,
    /// The messages by field.
    pub errors: ValidationErrors,
    /// The submitted input (without fields that look like secrets), flashed as old input.
    pub old: Input,
    /// Seconds until the client may try again: sent as `Retry-After` on the JSON answer (a 429).
    pub retry_after: Option<u64>,
}

impl Invalid {
    /// A 422 with these errors and old input.
    pub fn new(errors: ValidationErrors, old: Input) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: "The given data was invalid.".to_owned(),
            errors,
            old,
            retry_after: None,
        }
    }

    /// A 429 with `message` on `field` (and as the summary) and `Retry-After: retry_after` (at least 1): a throttled
    /// attempt.
    ///
    /// ```
    /// use smeltery_core::validation::Invalid;
    ///
    /// let invalid = Invalid::too_many("code", "Too many attempts. Please try again in 60 seconds.", 60);
    /// assert_eq!(invalid.status.as_u16(), 429);
    /// assert_eq!(invalid.retry_after, Some(60));
    /// ```
    pub fn too_many(field: &str, message: impl Into<String>, retry_after: u64) -> Self {
        let message = message.into();
        let mut errors = ValidationErrors::new();
        errors.add(field, message.clone());
        let mut invalid = Self::new(errors, Input::new());
        invalid.status = StatusCode::TOO_MANY_REQUESTS;
        invalid.message = message;
        // `Retry-After: 0` would invite an immediate retry into the same refusal.
        invalid.retry_after = Some(retry_after.max(1));
        invalid
    }

    pub(crate) fn into_response(self) -> Response {
        let body = serde_json::json!({ "message": self.message, "errors": self.errors });
        let mut response = (self.status, axum::Json(body)).into_response();
        if let Some(seconds) = self.retry_after {
            response
                .headers_mut()
                .insert(http::header::RETRY_AFTER, http::HeaderValue::from(seconds));
        }
        response
            .extensions_mut()
            .insert(InvalidMarker(Box::new(self)));
        response
    }
}

/// The failed validation in the response extensions, for the session middleware.
#[derive(Clone, Debug)]
pub(crate) struct InvalidMarker(pub(crate) Box<Invalid>);

/// What a [`Validate`] implementation sees besides the struct: the raw input and the
/// database for the `unique` / `exists` rules.
pub struct ValidationContext<'a> {
    input: &'a Input,
    db: Option<Db>,
    route_key: Option<String>,
    failure: Mutex<Option<Error>>,
}

impl std::fmt::Debug for ValidationContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidationContext")
            .field("fields", &self.input.keys().collect::<Vec<_>>())
            .field("db", &self.db.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> ValidationContext<'a> {
    /// A context over `input`, with the database for the DB rules when there is one.
    pub fn new(input: &'a Input, db: Option<Db>) -> Self {
        Self {
            input,
            db,
            route_key: None,
            failure: Mutex::new(None),
        }
    }

    /// The route's last path parameter (what a bare `except_id` excludes).
    pub fn with_route_key(mut self, key: Option<String>) -> Self {
        self.route_key = key;
        self
    }

    /// The raw value of `field`.
    pub fn input(&self, field: &str) -> Option<&str> {
        self.input.get(field).map(String::as_str)
    }

    /// The database, if the app has one.
    pub fn db(&self) -> Option<&Db> {
        self.db.as_ref()
    }

    /// The route's last path parameter, if any.
    pub fn route_key(&self) -> Option<&str> {
        self.route_key.as_deref()
    }

    /// Record a failure that is not a validation message (a database error): [`Valid`]
    /// answers it as a 500 instead of the messages.
    pub fn fail(&self, error: Error) {
        let mut slot = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(error);
        }
    }

    /// The failure recorded with [`fail`](Self::fail), if any.
    pub fn take_failure(&self) -> Option<Error> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// Rules over a deserialized struct; `#[derive(Validate)]` implements it.
pub trait Validate: Sync {
    /// Check every rule; `Err` holds every message.
    fn validate(
        &self,
        ctx: &ValidationContext<'_>,
    ) -> impl Future<Output = std::result::Result<(), ValidationErrors>> + Send;

    /// The rules that can be checked on raw text (`required`, `email`, `numeric` â€¦), used
    /// when the input does not even deserialize, so every message shows at once.
    #[doc(hidden)]
    fn validate_input(_input: &Input) -> ValidationErrors {
        ValidationErrors::new()
    }

    /// Field-specific messages for input that does not deserialize (`integer` â†’ "must be an
    /// integer"), `None` for the generic one.
    #[doc(hidden)]
    fn type_message(_field: &str) -> Option<String> {
        None
    }
}

/// A validated request body: `Valid(form): Valid<RegisterForm>`.
///
/// It reads a URL-encoded form, a `multipart/form-data` form (file fields are
/// [`UploadedFile`](crate::http::UploadedFile)s) or JSON when the content type is JSON, drops
/// fields that are empty strings (so `""` becomes absent and an `Option` field `None`), deserializes
/// and runs the rules. On failure the request gets [`Error::Validation`]: 422 JSON for JSON
/// requests, a redirect back with errors and old input on web routes.
#[derive(Clone, Debug)]
pub struct Valid<T>(pub T);

impl<T> std::ops::Deref for Valid<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> FromRequest<App> for Valid<T>
where
    T: DeserializeOwned + Validate + Send,
{
    type Rejection = Error;

    async fn from_request(
        req: axum::extract::Request,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let json = content_type.starts_with("application/json") || content_type.contains("+json");
        let (mut parts, body) = req.into_parts();
        let route_key = axum::extract::RawPathParams::from_request_parts(&mut parts, app)
            .await
            .ok()
            .and_then(|p| p.iter().last().map(|(_, v)| v.to_owned()));
        let mut file_errors = ValidationErrors::new();
        let (input, old, parsed) = if let Some(boundary) = crate::multipart::boundary(&content_type)
        {
            let form = crate::upload::read(app, body, boundary).await?;
            file_errors = form.file_errors;
            parse_multipart::<T>(app, form.text, form.files)?
        } else {
            let bytes = axum::body::to_bytes(body, app.settings().body_limit)
                .await
                .map_err(|_| Error::http(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large"))?;
            let (input, parsed) = if json {
                parse_json::<T>(&bytes)?
            } else {
                parse_form::<T>(&bytes)
            };
            let old = old_input(app, &input);
            (input, old, parsed)
        };
        let value = match parsed {
            Ok(value) => value,
            Err(failure) => {
                let mut errors = T::validate_input(&input);
                if let Some(field) = failure.field
                    && !errors.has(&field)
                {
                    let message = if failure.missing {
                        rules::msg_required(&rules::label(&field))
                    } else {
                        T::type_message(&field).unwrap_or_else(|| {
                            format!("The {} field is invalid.", rules::label(&field))
                        })
                    };
                    errors.add(field, message);
                }
                errors.replace_from(file_errors);
                if errors.is_empty() {
                    errors.add("_body", "The request body is invalid.");
                }
                return Err(Error::Validation(Box::new(Invalid::new(errors, old))));
            }
        };
        let ctx = ValidationContext::new(&input, app.db().ok()).with_route_key(route_key);
        let result = value.validate(&ctx).await;
        if let Some(error) = ctx.take_failure() {
            return Err(error);
        }
        let mut errors = result.err().unwrap_or_default();
        errors.replace_from(file_errors);
        if errors.is_empty() {
            Ok(Self(value))
        } else {
            Err(Error::Validation(Box::new(Invalid::new(errors, old))))
        }
    }
}

use axum::extract::FromRequestParts as _;

/// Why the input did not deserialize, and on which field when known.
struct DeFailure {
    field: Option<String>,
    missing: bool,
}

fn de_failure<E: std::fmt::Display>(error: &serde_path_to_error::Error<E>) -> DeFailure {
    let message = error.inner().to_string();
    if let Some(rest) = message.strip_prefix("missing field `")
        && let Some(field) = rest.split('`').next()
    {
        return DeFailure {
            field: Some(field.to_owned()),
            missing: true,
        };
    }
    let path = error.path().to_string();
    let field = path
        .split(['.', '['])
        .next()
        .filter(|f| !f.is_empty() && *f != "?");
    DeFailure {
        field: field.map(str::to_owned),
        missing: false,
    }
}

/// The form pairs (empty values dropped) and the deserialized value.
fn parse_form<T: DeserializeOwned>(bytes: &[u8]) -> (Input, std::result::Result<T, DeFailure>) {
    let pairs: Vec<(String, String)> = form_urlencoded::parse(bytes).into_owned().collect();
    let input: Input = pairs
        .iter()
        .filter(|(k, _)| k != "_token" && k != "_method")
        .cloned()
        .collect();
    let kept: Vec<(String, String)> = pairs.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    let encoded = serde_urlencoded::to_string(&kept).unwrap_or_default();
    let de = serde_urlencoded::Deserializer::new(form_urlencoded::parse(encoded.as_bytes()));
    let parsed = serde_path_to_error::deserialize(de).map_err(|e| de_failure(&e));
    (input, parsed)
}

/// A multipart form's text fields and files: the input (a file field holds its file name), the
/// old input to flash (text fields only) and the deserialized value. A file field deserializes
/// into an [`UploadedFile`](crate::http::UploadedFile).
#[allow(clippy::type_complexity)]
fn parse_multipart<T: DeserializeOwned>(
    app: &App,
    text: Vec<(String, String)>,
    files: Vec<(String, crate::upload::UploadedFile)>,
) -> Result<(Input, Input, std::result::Result<T, DeFailure>)> {
    let text: Vec<(String, String)> = text
        .into_iter()
        .filter(|(k, _)| k != "_token" && k != "_method")
        .collect();
    let mut input: Input = text.iter().cloned().collect();
    let old = old_input(app, &input);
    let mut kept: Vec<(String, String)> = text.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    let mut claimable = std::collections::HashMap::new();
    for (field, file) in files {
        let key = crate::upload::file_key()?;
        input.insert(field.clone(), file.name().to_owned());
        kept.push((field, key.clone()));
        claimable.insert(key, file);
    }
    let encoded = serde_urlencoded::to_string(&kept).unwrap_or_default();
    let parsed = crate::upload::with_files(claimable, || {
        let de = serde_urlencoded::Deserializer::new(form_urlencoded::parse(encoded.as_bytes()));
        serde_path_to_error::deserialize(de).map_err(|e| de_failure(&e))
    });
    Ok((input, old, parsed))
}

/// The JSON object's fields as text (empty strings dropped) and the deserialized value.
#[allow(clippy::type_complexity)]
fn parse_json<T: DeserializeOwned>(
    bytes: &[u8],
) -> Result<(Input, std::result::Result<T, DeFailure>)> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| Error::bad_request("The request body is not valid JSON."))?;
    let mut input = Input::new();
    let value = match value {
        serde_json::Value::Object(map) => {
            let mut kept = serde_json::Map::new();
            for (k, v) in map {
                let text = match &v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                };
                input.insert(k.clone(), text);
                if !matches!(&v, serde_json::Value::String(s) if s.is_empty()) {
                    kept.insert(k, v);
                }
            }
            serde_json::Value::Object(kept)
        }
        other => other,
    };
    let parsed = serde_path_to_error::deserialize(value).map_err(|e| de_failure(&e));
    Ok((input, parsed))
}

/// Field names holding one of these, compared in lowercase with everything but letters and
/// digits left out (`private_key`, `privateKey` and `private-key` all hold `privatekey`), are
/// never flashed as old input.
const SECRET_NAME_PARTS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "passcode",
    "secret",
    "token",
    "apikey",
    "privatekey",
    "accesskey",
    "cardnumber",
    "creditcard",
    "securitycode",
    "twofactor",
    "recoverycode",
];

/// Field names with one of these as a whole word are never flashed as old input. Words are
/// split at anything but letters and digits and where a lowercase letter or digit meets an
/// uppercase one (`mfaCode` is `mfa` and `code`), and compared ignoring case.
const SECRET_NAME_WORDS: &[&str] = &[
    "key", "pin", "otp", "totp", "mfa", "2fa", "tfa", "auth", "card", "cvv", "cvc", "cvv2", "iban",
    "ssn", "answer", "recovery",
];

/// The longest value flashed as old input; a longer one is left out.
pub(crate) const MAX_OLD_VALUE: usize = 16 * 1024;
/// The most fields flashed as old input.
pub(crate) const MAX_OLD_FIELDS: usize = 100;
/// The most bytes (names and values) flashed as old input together.
pub(crate) const MAX_OLD_TOTAL: usize = 64 * 1024;

/// Whether a field name looks like it holds a secret (see [`SECRET_NAME_PARTS`] and
/// [`SECRET_NAME_WORDS`]).
pub(crate) fn is_secret_field(name: &str) -> bool {
    let joined: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    SECRET_NAME_PARTS.iter().any(|part| joined.contains(part))
        || name_words(name).any(|word| SECRET_NAME_WORDS.contains(&word.as_str()))
}

/// The words of a field name, in lowercase: split at anything but ASCII letters and digits,
/// and before an uppercase letter that follows a lowercase letter or a digit (camelCase).
fn name_words(name: &str) -> impl Iterator<Item = String> + '_ {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .flat_map(|part| {
            let mut words = Vec::new();
            let mut word = String::new();
            let mut previous: Option<char> = None;
            for c in part.chars() {
                let boundary = c.is_ascii_uppercase()
                    && previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit());
                if boundary && !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                word.push(c.to_ascii_lowercase());
                previous = Some(c);
            }
            if !word.is_empty() {
                words.push(word);
            }
            words
        })
}

/// The input to flash back: every field except those that look like secrets (passwords,
/// tokens, keys, card numbers â€¦) and those the app named with
/// [`AppBuilder::dont_flash`](crate::AppBuilder::dont_flash); at most [`MAX_OLD_FIELDS`]
/// fields of at most [`MAX_OLD_VALUE`] bytes each and [`MAX_OLD_TOTAL`] bytes in all, so a
/// stored session stays small.
pub(crate) fn old_input(app: &App, input: &Input) -> Input {
    let extra = app.dont_flash();
    let mut out = Input::new();
    let mut total = 0usize;
    for (k, v) in input {
        if is_secret_field(k) || extra.iter().any(|name| name == k) || v.len() > MAX_OLD_VALUE {
            continue;
        }
        let size = k.len() + v.len();
        if out.len() >= MAX_OLD_FIELDS || total + size > MAX_OLD_TOTAL {
            break;
        }
        total += size;
        out.insert(k.clone(), v.clone());
    }
    out
}

pub(crate) fn old_from_form(app: &App, bytes: &[u8]) -> Input {
    let input: Input = form_urlencoded::parse(bytes)
        .into_owned()
        .filter(|(k, _)| k != "_token" && k != "_method")
        .collect();
    old_input(app, &input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Form {
        name: String,
        age: Option<i64>,
    }

    impl Validate for Form {
        async fn validate(
            &self,
            _ctx: &ValidationContext<'_>,
        ) -> std::result::Result<(), ValidationErrors> {
            Ok(())
        }
    }

    #[test]
    fn empty_strings_become_absent() {
        let (input, parsed) = parse_form::<Form>(b"name=Ann&age=&_token=x");
        let form = parsed.ok().unwrap();
        assert_eq!((form.name.as_str(), form.age), ("Ann", None));
        assert_eq!(input.get("age").map(String::as_str), Some(""));
        assert!(!input.contains_key("_token"));
    }

    #[test]
    fn deserialize_failures_name_the_field() {
        let (_, parsed) = parse_form::<Form>(b"name=&age=3");
        let failure = parsed.err().unwrap();
        assert_eq!(
            (failure.field.as_deref(), failure.missing),
            (Some("name"), true)
        );
        let (_, parsed) = parse_form::<Form>(b"name=a&age=x");
        let failure = parsed.err().unwrap();
        assert_eq!(
            (failure.field.as_deref(), failure.missing),
            (Some("age"), false)
        );
        let (input, parsed) = parse_json::<Form>(br#"{"name": "", "age": "no"}"#).unwrap();
        assert_eq!(input.get("name").map(String::as_str), Some(""));
        assert!(parsed.is_err());
        assert!(parse_json::<Form>(b"{").is_err());
    }

    #[test]
    fn errors_serialize_as_a_map() {
        let mut e = ValidationErrors::new();
        e.add("email", "a");
        e.add("email", "b");
        assert_eq!(e.first("email"), Some("a"));
        assert_eq!(e.get("email").len(), 2);
        assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"email":["a","b"]}"#);
        assert!(e.clone().into_result().is_err());
        assert!(ValidationErrors::new().into_result().is_ok());
    }

    async fn app_without_flash(names: &[&str]) -> App {
        crate::AppBuilder::new(crate::config::Settings::from_env())
            .dont_flash(names)
            .build()
            .await
            .unwrap()
            .app
    }

    #[tokio::test]
    async fn secrets_are_never_flashed_as_old_input() {
        let app = app_without_flash(&["nickname"]).await;
        let secret = [
            "password",
            "password_confirmation",
            "current_password",
            "newPassword",
            "token",
            "reset_token",
            "client_secret",
            "api_key",
            "api-key",
            "apiKey",
            "key",
            "pin",
            "otp",
            "card",
            "card_number",
            "cvv",
            "iban",
            "ssn",
            // S1-12 re-check: camelCase and more secret names.
            "privateKey",
            "private-key",
            "passcode",
            "mfa_code",
            "mfaCode",
            "otpCode",
            "totp",
            "2fa_code",
            "twoFactorCode",
            "recovery_codes",
            "recoveryCodes",
            "security_answer",
            "securityAnswer",
            "auth",
            "authCode",
            "creditCard",
            "cardNumber",
            "cvv2",
            "securityCode",
            "accessKey",
            "userPIN",
        ];
        let kept = [
            "name",
            "email",
            "keyword",
            "monkey",
            "spinach",
            "title",
            "discount_code",
            "className",
            "author",
            "lessons",
            "pinned",
            "keyboardLayout",
            "cardinal",
        ];
        let mut input = Input::new();
        for name in secret.iter().chain(&kept).chain(&["nickname"]) {
            input.insert((*name).to_owned(), "v".to_owned());
        }
        let old = old_input(&app, &input);
        let mut names: Vec<&str> = old.keys().map(String::as_str).collect();
        names.sort_unstable();
        let mut expected = kept.to_vec();
        expected.sort_unstable();
        assert_eq!(names, expected, "`nickname` is named by `dont_flash`");
    }

    #[tokio::test]
    async fn old_input_is_bounded() {
        let app = app_without_flash(&[]).await;
        let mut input = Input::new();
        input.insert("big".to_owned(), "x".repeat(MAX_OLD_VALUE + 1));
        input.insert("fits".to_owned(), "x".repeat(MAX_OLD_VALUE));
        let old = old_input(&app, &input);
        assert!(!old.contains_key("big") && old.contains_key("fits"));

        let many: Input = (0..1000)
            .map(|i| (format!("f{i:04}"), "v".to_owned()))
            .collect();
        assert_eq!(old_input(&app, &many).len(), MAX_OLD_FIELDS);

        let heavy: Input = (0..50)
            .map(|i| (format!("f{i:02}"), "x".repeat(MAX_OLD_VALUE - 10)))
            .collect();
        let old = old_input(&app, &heavy);
        let total: usize = old.iter().map(|(k, v)| k.len() + v.len()).sum();
        assert!(total <= MAX_OLD_TOTAL && !old.is_empty(), "{total}");
    }
}
