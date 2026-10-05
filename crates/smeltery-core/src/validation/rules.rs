//! The rules `#[derive(Validate)]` calls, and their messages. Public so the generated code
//! can reach them; apps use the derive.

use sea_orm::sea_query::{Alias, Expr, ExprTrait, Func, Query};
use sea_orm::{ConnectionTrait, Value};

use super::ValidationContext;
use crate::error::{Error, Result};

/// A field's value as the rules see it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Subject<'a> {
    /// `None` (or a missing field).
    Absent,
    /// Text.
    Str(&'a str),
    /// A whole number.
    Int(i128),
    /// A floating-point number.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// A list with this many items.
    List(usize),
    /// An uploaded file of this many bytes (`min`, `max` and `between` count kilobytes).
    File(u64),
    /// An uploaded file with its name and content type: `min`, `max` and `between` count
    /// kilobytes, and `mimes` checks the name's extension.
    Upload {
        /// The size in bytes.
        size: u64,
        /// The file name (without directories).
        name: &'a str,
        /// The content type.
        mime: &'a str,
    },
}

impl Subject<'_> {
    /// Whether the value is missing.
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Missing, blank text or an empty list: what `required` rejects.
    pub fn is_blank(&self) -> bool {
        match self {
            Self::Absent => true,
            Self::Str(s) => s.trim().is_empty(),
            Self::List(n) => *n == 0,
            _ => false,
        }
    }

    fn db_value(&self) -> Option<Value> {
        match self {
            Self::Str(s) => Some(Value::from((*s).to_owned())),
            Self::Int(n) => i64::try_from(*n).ok().map(Value::from),
            Self::Float(f) => Some(Value::from(*f)),
            Self::Bool(b) => Some(Value::from(*b)),
            _ => None,
        }
    }

    fn text(&self) -> Option<String> {
        match self {
            Self::Str(s) => Some((*s).to_owned()),
            Self::Int(n) => Some(n.to_string()),
            Self::Float(f) => Some(f.to_string()),
            Self::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }
}

/// Types a validated field may have.
pub trait AsSubject {
    /// The value as the rules see it.
    fn as_subject(&self) -> Subject<'_>;
}

impl AsSubject for String {
    fn as_subject(&self) -> Subject<'_> {
        Subject::Str(self)
    }
}

impl AsSubject for &str {
    fn as_subject(&self) -> Subject<'_> {
        Subject::Str(self)
    }
}

impl AsSubject for bool {
    fn as_subject(&self) -> Subject<'_> {
        Subject::Bool(*self)
    }
}

macro_rules! int_subject {
    ($($t:ty),*) => {$(
        impl AsSubject for $t {
            fn as_subject(&self) -> Subject<'_> {
                Subject::Int(i128::from(*self))
            }
        }
    )*};
}
int_subject!(i8, i16, i32, i64, u8, u16, u32, u64);

impl AsSubject for f32 {
    fn as_subject(&self) -> Subject<'_> {
        Subject::Float(f64::from(*self))
    }
}

impl AsSubject for f64 {
    fn as_subject(&self) -> Subject<'_> {
        Subject::Float(*self)
    }
}

impl<T> AsSubject for Vec<T> {
    fn as_subject(&self) -> Subject<'_> {
        Subject::List(self.len())
    }
}

impl<T: AsSubject> AsSubject for Option<T> {
    fn as_subject(&self) -> Subject<'_> {
        self.as_ref().map_or(Subject::Absent, AsSubject::as_subject)
    }
}

/// The field name as messages show it: `password_confirmation` → `password confirmation`.
pub fn label(field: &str) -> String {
    field.replace('_', " ")
}

/// "The {field} field is required."
pub fn msg_required(label: &str) -> String {
    format!("The {label} field is required.")
}

/// `required`: `Some(message)` when the value is missing or blank.
pub fn required(s: &Subject<'_>, label: &str) -> Option<String> {
    s.is_blank().then(|| msg_required(label))
}

/// `email`.
pub fn email(s: &Subject<'_>, label: &str) -> Option<String> {
    let ok = match s {
        Subject::Str(v) => is_email(v),
        _ => false,
    };
    (!ok).then(|| format!("The {label} field must be a valid email address."))
}

fn is_email(v: &str) -> bool {
    let Some((local, domain)) = v.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && !v.chars().any(|c| c.is_whitespace() || c.is_control())
        && !local.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
}

/// `url`: an `http` or `https` URL with a host.
pub fn url(s: &Subject<'_>, label: &str) -> Option<String> {
    let ok = match s {
        Subject::Str(v) => ["http://", "https://"].iter().any(|scheme| {
            v.get(..scheme.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(scheme))
                && v.get(scheme.len()..).is_some_and(|rest| {
                    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
                    !host.is_empty() && !rest.chars().any(char::is_whitespace)
                })
        }),
        _ => false,
    };
    (!ok).then(|| format!("The {label} field must be a valid URL."))
}

/// What `min`/`max`/`between` measure: characters, the value, or items.
enum Size {
    Chars(f64),
    Number(f64),
    Items(f64),
    Kilobytes(f64),
}

#[allow(clippy::cast_precision_loss)]
fn size(s: &Subject<'_>) -> Option<Size> {
    match s {
        Subject::Str(v) => Some(Size::Chars(v.chars().count() as f64)),
        Subject::Int(n) => Some(Size::Number(*n as f64)),
        Subject::Float(f) => Some(Size::Number(*f)),
        Subject::List(n) => Some(Size::Items(*n as f64)),
        Subject::File(bytes) | Subject::Upload { size: bytes, .. } => {
            Some(Size::Kilobytes(*bytes as f64 / 1024.0))
        }
        _ => None,
    }
}

/// `min = n`.
pub fn min(s: &Subject<'_>, label: &str, n: f64) -> Option<String> {
    match size(s)? {
        Size::Chars(v) if v < n => Some(format!(
            "The {label} field must be at least {} characters.",
            num(n)
        )),
        Size::Number(v) if v < n => Some(format!("The {label} field must be at least {}.", num(n))),
        Size::Items(v) if v < n => Some(format!(
            "The {label} field must have at least {} items.",
            num(n)
        )),
        Size::Kilobytes(v) if v < n => Some(format!(
            "The {label} field must be at least {} kilobytes.",
            num(n)
        )),
        _ => None,
    }
}

/// `max = n`.
pub fn max(s: &Subject<'_>, label: &str, n: f64) -> Option<String> {
    match size(s)? {
        Size::Chars(v) if v > n => Some(format!(
            "The {label} field must not be greater than {} characters.",
            num(n)
        )),
        Size::Number(v) if v > n => Some(format!(
            "The {label} field must not be greater than {}.",
            num(n)
        )),
        Size::Items(v) if v > n => Some(format!(
            "The {label} field must not have more than {} items.",
            num(n)
        )),
        Size::Kilobytes(v) if v > n => Some(format!(
            "The {label} field must not be greater than {} kilobytes.",
            num(n)
        )),
        _ => None,
    }
}

/// `between(a, b)`.
pub fn between(s: &Subject<'_>, label: &str, a: f64, b: f64) -> Option<String> {
    let (a_s, b_s) = (num(a), num(b));
    match size(s)? {
        Size::Chars(v) if v < a || v > b => Some(format!(
            "The {label} field must be between {a_s} and {b_s} characters."
        )),
        Size::Number(v) if v < a || v > b => Some(format!(
            "The {label} field must be between {a_s} and {b_s}."
        )),
        Size::Items(v) if v < a || v > b => Some(format!(
            "The {label} field must have between {a_s} and {b_s} items."
        )),
        Size::Kilobytes(v) if v < a || v > b => Some(format!(
            "The {label} field must be between {a_s} and {b_s} kilobytes."
        )),
        _ => None,
    }
}

/// `mimes = "png,jpg"`: an uploaded file whose name ends in one of the extensions (`jpg` and
/// `jpeg` count as one). The upload's content was already checked against its type.
pub fn mimes(s: &Subject<'_>, label: &str, extensions: &[&str]) -> Option<String> {
    let ok = match s {
        Subject::Upload { name, .. } => crate::upload::extension(name).is_some_and(|ext| {
            extensions.iter().any(|allowed| {
                let allowed = allowed.trim().to_ascii_lowercase();
                allowed == ext || (is_jpeg(&allowed) && is_jpeg(&ext))
            })
        }),
        _ => false,
    };
    (!ok).then(|| {
        format!(
            "The {label} field must be a file of type: {}.",
            extensions.join(", ")
        )
    })
}

fn is_jpeg(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg")
}

/// `3.0` → `3`, `2.5` → `2.5`.
fn num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{n:.0}")
    } else {
        n.to_string()
    }
}

/// `numeric`: a number, or text that parses as one.
pub fn numeric(s: &Subject<'_>, label: &str) -> Option<String> {
    let ok = match s {
        Subject::Int(_) | Subject::Float(_) => true,
        Subject::Str(v) => v.trim().parse::<f64>().is_ok_and(f64::is_finite),
        _ => false,
    };
    (!ok).then(|| format!("The {label} field must be a number."))
}

/// `integer`: a whole number, or text that parses as one.
pub fn integer(s: &Subject<'_>, label: &str) -> Option<String> {
    let ok = match s {
        Subject::Int(_) => true,
        Subject::Float(f) => f.fract() == 0.0,
        Subject::Str(v) => v.trim().parse::<i128>().is_ok(),
        _ => false,
    };
    (!ok).then(|| format!("The {label} field must be an integer."))
}

fn chars_all(s: &Subject<'_>, f: impl Fn(char) -> bool) -> bool {
    match s {
        Subject::Str(v) => v.chars().all(f),
        Subject::Int(n) => n.to_string().chars().all(f),
        _ => false,
    }
}

/// `alpha`: letters only.
pub fn alpha(s: &Subject<'_>, label: &str) -> Option<String> {
    (!chars_all(s, char::is_alphabetic))
        .then(|| format!("The {label} field must only contain letters."))
}

/// `alpha_num`: letters and digits.
pub fn alpha_num(s: &Subject<'_>, label: &str) -> Option<String> {
    (!chars_all(s, char::is_alphanumeric))
        .then(|| format!("The {label} field must only contain letters and numbers."))
}

/// `alpha_dash`: letters, digits, `-` and `_`.
pub fn alpha_dash(s: &Subject<'_>, label: &str) -> Option<String> {
    (!chars_all(s, |c| c.is_alphanumeric() || c == '-' || c == '_')).then(|| {
        format!("The {label} field must only contain letters, numbers, dashes, and underscores.")
    })
}

/// `in_list("a", "b")`.
pub fn in_list(s: &Subject<'_>, label: &str, options: &[&str]) -> Option<String> {
    let ok = s.text().is_some_and(|v| options.contains(&v.as_str()));
    (!ok).then(|| format!("The selected {label} is invalid."))
}

/// `confirmed`: `other` is the value of `<field>_confirmation`.
pub fn confirmed(s: &Subject<'_>, label: &str, other: Option<String>) -> Option<String> {
    (s.text() != other || other.is_none())
        .then(|| format!("The {label} field confirmation does not match."))
}

/// `same = "other"`.
pub fn same(
    s: &Subject<'_>,
    label: &str,
    other_label: &str,
    other: Option<String>,
) -> Option<String> {
    (s.text() != other || other.is_none())
        .then(|| format!("The {label} field must match {other_label}."))
}

/// `accepted`: `yes`, `on`, `1`, `true` or `true`.
pub fn accepted(s: &Subject<'_>, label: &str) -> Option<String> {
    let ok = match s {
        Subject::Bool(b) => *b,
        Subject::Int(n) => *n == 1,
        Subject::Str(v) => matches!(v.to_ascii_lowercase().as_str(), "yes" | "on" | "1" | "true"),
        _ => false,
    };
    (!ok).then(|| format!("The {label} field must be accepted."))
}

/// The text form of a subject, for comparing with `<field>_confirmation` or `same`.
pub fn text_of(s: &Subject<'_>) -> Option<String> {
    s.text()
}

/// Rows in `table` where `column` equals the value, excluding the row whose `id` is `except`.
async fn count(
    ctx: &ValidationContext<'_>,
    s: &Subject<'_>,
    table: &str,
    column: &str,
    except: Option<&str>,
) -> Result<Option<i64>> {
    let db = ctx.db().ok_or_else(|| {
        Error::internal(format!(
            "the rule on `{table}.{column}` needs a database: set DATABASE_URL"
        ))
    })?;
    let Some(value) = s.db_value() else {
        return Ok(None);
    };
    let mut select = Query::select();
    select
        .expr_as(Func::count(Expr::cust("*")), Alias::new("n"))
        .from(Alias::new(table))
        .and_where(Expr::col(Alias::new(column)).eq(value));
    if let Some(id) = except {
        let id = id
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::from(id.to_owned()));
        select.and_where(Expr::col(Alias::new("id")).ne(id));
    }
    let stmt = db.conn().get_database_backend().build(&select);
    let row = db.conn().query_one_raw(stmt).await?;
    Ok(Some(match row {
        Some(row) => row.try_get::<i64>("", "n")?,
        None => 0,
    }))
}

/// `unique(table = "…", column = "…"[, except_id])`: no other row has this value.
/// DB errors are recorded on the context ([`ValidationContext::fail`]).
pub async fn unique(
    ctx: &ValidationContext<'_>,
    s: &Subject<'_>,
    label: &str,
    table: &str,
    column: &str,
    except: Option<&str>,
) -> Option<String> {
    match count(ctx, s, table, column, except).await {
        Ok(Some(n)) if n > 0 => Some(format!("The {label} has already been taken.")),
        Ok(_) => None,
        Err(e) => {
            ctx.fail(e);
            None
        }
    }
}

/// `exists(table = "…", column = "…")`: a row has this value.
pub async fn exists(
    ctx: &ValidationContext<'_>,
    s: &Subject<'_>,
    label: &str,
    table: &str,
    column: &str,
) -> Option<String> {
    match count(ctx, s, table, column, None).await {
        Ok(Some(0)) | Ok(None) => Some(format!("The selected {label} is invalid.")),
        Ok(Some(_)) => None,
        Err(e) => {
            ctx.fail(e);
            None
        }
    }
}

/// The text of a raw input field as a subject (for checks before deserializing).
pub fn raw<'a>(input: &'a super::Input, field: &str) -> Subject<'a> {
    match input.get(field) {
        Some(v) if !v.is_empty() => Subject::Str(v),
        _ => Subject::Absent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Subject<'_> {
        Subject::Str(v)
    }

    #[test]
    fn files_are_present_and_measured_in_kilobytes() {
        let file = Subject::File(3 * 1024);
        assert!(required(&file, "photo").is_none());
        assert_eq!(
            max(&file, "photo", 2.0).unwrap(),
            "The photo field must not be greater than 2 kilobytes."
        );
        assert!(max(&file, "photo", 3.0).is_none());
        assert_eq!(
            min(&file, "photo", 4.0).unwrap(),
            "The photo field must be at least 4 kilobytes."
        );
        assert_eq!(
            between(&file, "photo", 4.0, 8.0).unwrap(),
            "The photo field must be between 4 and 8 kilobytes."
        );
        assert!(email(&file, "photo").is_some(), "a file is not text");
    }

    #[test]
    fn every_rule_and_message() {
        assert_eq!(
            required(&Subject::Absent, "name").unwrap(),
            "The name field is required."
        );
        assert!(required(&s("  "), "name").is_some());
        assert!(required(&Subject::List(0), "tags").is_some());
        assert!(required(&s("x"), "name").is_none());
        assert!(required(&Subject::Bool(false), "x").is_none());

        assert_eq!(
            email(&s("nope"), "email").unwrap(),
            "The email field must be a valid email address."
        );
        for bad in ["a@b", "@b.c", "a b@c.d", "a@.c", "a@c.", "a@c..d"] {
            assert!(email(&s(bad), "e").is_some(), "{bad}");
        }
        assert!(email(&s("ada@example.com"), "e").is_none());

        assert_eq!(
            url(&s("ftp://x"), "site").unwrap(),
            "The site field must be a valid URL."
        );
        assert!(url(&s("https://"), "u").is_some());
        assert!(url(&s("https://example.com/a?b"), "u").is_none());
        assert!(url(&s("HTTP://x.y"), "u").is_none());

        assert_eq!(
            min(&s("short"), "password", 8.0).unwrap(),
            "The password field must be at least 8 characters."
        );
        assert_eq!(
            min(&Subject::Int(1), "age", 18.0).unwrap(),
            "The age field must be at least 18."
        );
        assert_eq!(
            min(&Subject::List(1), "tags", 2.0).unwrap(),
            "The tags field must have at least 2 items."
        );
        assert!(
            min(&s("ééééé"), "x", 5.0).is_none(),
            "characters, not bytes"
        );
        assert_eq!(
            max(&s(&"x".repeat(256)), "name", 255.0).unwrap(),
            "The name field must not be greater than 255 characters."
        );
        assert_eq!(
            max(&Subject::Float(2.5), "n", 2.0).unwrap(),
            "The n field must not be greater than 2."
        );
        assert_eq!(
            max(&Subject::List(4), "tags", 3.0).unwrap(),
            "The tags field must not have more than 3 items."
        );
        assert_eq!(
            between(&Subject::Int(130), "age", 1.0, 120.0).unwrap(),
            "The age field must be between 1 and 120."
        );
        assert_eq!(
            between(&s("a"), "code", 2.0, 4.0).unwrap(),
            "The code field must be between 2 and 4 characters."
        );
        assert_eq!(
            between(&Subject::List(0), "tags", 1.0, 2.0).unwrap(),
            "The tags field must have between 1 and 2 items."
        );
        assert!(between(&Subject::Int(5), "age", 1.0, 120.0).is_none());

        assert_eq!(
            numeric(&s("x"), "price").unwrap(),
            "The price field must be a number."
        );
        assert!(numeric(&s("2.5"), "p").is_none());
        assert!(numeric(&Subject::Int(2), "p").is_none());
        assert_eq!(
            integer(&s("2.5"), "age").unwrap(),
            "The age field must be an integer."
        );
        assert!(integer(&s("-3"), "age").is_none());

        assert_eq!(
            alpha(&s("ab1"), "x").unwrap(),
            "The x field must only contain letters."
        );
        assert!(alpha(&s("abé"), "x").is_none());
        assert_eq!(
            alpha_num(&s("a-1"), "x").unwrap(),
            "The x field must only contain letters and numbers."
        );
        assert!(alpha_num(&s("a1"), "x").is_none());
        assert_eq!(
            alpha_dash(&s("a 1"), "x").unwrap(),
            "The x field must only contain letters, numbers, dashes, and underscores."
        );
        assert!(alpha_dash(&s("a-1_b"), "x").is_none());

        assert_eq!(
            in_list(&s("c"), "role", &["a", "b"]).unwrap(),
            "The selected role is invalid."
        );
        assert!(in_list(&s("a"), "role", &["a", "b"]).is_none());
        assert!(in_list(&Subject::Int(2), "n", &["1", "2"]).is_none());

        assert_eq!(
            confirmed(&s("x"), "password", Some("y".into())).unwrap(),
            "The password field confirmation does not match."
        );
        assert!(confirmed(&s("x"), "password", None).is_some());
        assert!(confirmed(&s("x"), "password", Some("x".into())).is_none());
        assert_eq!(
            same(&s("x"), "email", "email again", Some("y".into())).unwrap(),
            "The email field must match email again."
        );
        assert!(same(&s("x"), "a", "b", Some("x".into())).is_none());

        assert_eq!(
            accepted(&s("no"), "terms").unwrap(),
            "The terms field must be accepted."
        );
        for ok in ["yes", "on", "1", "TRUE"] {
            assert!(accepted(&s(ok), "t").is_none(), "{ok}");
        }
        assert!(accepted(&Subject::Bool(true), "t").is_none());
        assert!(accepted(&Subject::Bool(false), "t").is_some());

        assert_eq!(label("password_confirmation"), "password confirmation");
        assert_eq!(Some(3).as_subject(), Subject::Int(3));
        assert_eq!(None::<String>.as_subject(), Subject::Absent);
        assert_eq!(vec![1, 2].as_subject(), Subject::List(2));
        assert_eq!(num(2.5), "2.5");
    }
}
