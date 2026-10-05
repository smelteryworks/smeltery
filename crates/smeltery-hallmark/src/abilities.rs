//! Abilities: what a token may do. Names, bounds, the stored form and the `abilities:` / `ability:` middleware.

use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use http::StatusCode;
use smeltery_core::auth::Principal;
use smeltery_core::middleware::{BoxedMiddleware, Next, Request};
use smeltery_core::{Error, Result};

/// The most abilities one token holds.
pub const MAX_ABILITIES: usize = 64;

/// The longest ability name, in bytes.
pub const MAX_ABILITY_LEN: usize = 100;

/// The longest stored abilities text read back: every bounded list fits (64 names of 100 bytes, quoted, with
/// commas); a longer column is corrupt and never parsed.
const MAX_STORED_LEN: usize = MAX_ABILITIES * (MAX_ABILITY_LEN + 3) + 2;

/// Whether `ability` is a valid ability name: exactly `*`, or 1 to 100 of `A-Z a-z 0-9 : . _ -`.
pub fn valid_ability(ability: &str) -> bool {
    ability == "*"
        || (!ability.is_empty()
            && ability.len() <= MAX_ABILITY_LEN
            && ability
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-')))
}

/// The abilities of a new token, checked: at most 64 valid names (duplicates dropped, order kept).
pub(crate) fn checked(abilities: &[&str]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for ability in abilities {
        if !valid_ability(ability) {
            return Err(Error::bad_request(format!(
                "invalid token ability {:?}: use `*` or 1 to {MAX_ABILITY_LEN} of A-Z a-z 0-9 : . _ -",
                shorten(ability)
            )));
        }
        if !out.iter().any(|a| a == ability) {
            out.push((*ability).to_owned());
        }
    }
    if out.len() > MAX_ABILITIES {
        return Err(Error::bad_request(format!(
            "a token holds at most {MAX_ABILITIES} abilities"
        )));
    }
    Ok(out)
}

/// The first 40 characters of `text` (error messages never repeat a long value).
fn shorten(text: &str) -> String {
    text.chars().take(40).collect()
}

/// The stored abilities column read back with the same bounds, `None` when it is corrupt (too long, not a JSON
/// array of strings, too many, an invalid name). The length is checked before anything is parsed.
pub(crate) fn parse_stored(text: &str) -> Option<Vec<String>> {
    if text.len() > MAX_STORED_LEN {
        return None;
    }
    let list: Vec<String> = serde_json::from_str(text).ok()?;
    (list.len() <= MAX_ABILITIES && list.iter().all(|a| valid_ability(a))).then_some(list)
}

/// Which of the listed abilities a route needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    /// `abilities:a,b`: every one.
    All,
    /// `ability:a,b`: at least one.
    Any,
}

/// The middleware for `abilities:<list>` / `ability:<list>` on one route; an empty list or an invalid name fails
/// the build.
pub(crate) fn family(need: Need, prefix: &str, args: &str) -> Result<BoxedMiddleware> {
    let list: Vec<String> = args.split(',').map(str::trim).map(str::to_owned).collect();
    if list.iter().any(|a| !valid_ability(a)) || list.is_empty() {
        return Err(Error::internal(format!(
            "invalid middleware `{prefix}:{args}`: list abilities separated by commas, each `*` or 1 to \
             {MAX_ABILITY_LEN} of A-Z a-z 0-9 : . _ -, e.g. `{prefix}:orders:read,orders:write`"
        )));
    }
    let list: Arc<[String]> = list.into();
    Ok(BoxedMiddleware::new(move |req: Request, next: Next| {
        let list = Arc::clone(&list);
        async move { check(need, &list, req, next).await }
    }))
}

async fn check(need: Need, list: &[String], req: Request, next: Next) -> Response {
    let Some(principal) = req.extensions().get::<Principal>() else {
        return unauthenticated();
    };
    let allowed = match need {
        Need::All => list.iter().all(|a| principal.can(a)),
        Need::Any => list.iter().any(|a| principal.can(a)),
    };
    if allowed {
        next.run(req).await
    } else {
        forbidden()
    }
}

/// Core's bearer 401 (`{"error":"Unauthenticated."}`, `WWW-Authenticate: Bearer`, `no-store`).
pub(crate) fn unauthenticated() -> Response {
    smeltery_core::auth::unauthenticated_bearer()
}

/// 403 `{"error":"Forbidden."}`.
fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({ "error": "Forbidden." })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn ability_names_are_bounded() {
        for good in ["*", "orders:read", "a", "A.b_c-d:e", &"x".repeat(100)] {
            assert!(valid_ability(good), "{good}");
        }
        for bad in [
            "",
            "orders read",
            "orders:*x\n",
            "ä",
            "a,b",
            "**",
            &"x".repeat(101),
        ] {
            assert!(!valid_ability(bad), "{bad}");
        }
        assert_eq!(
            checked(&["a", "b", "a"]).unwrap(),
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert!(checked(&[]).unwrap().is_empty());
        let many: Vec<String> = (0..65).map(|i| format!("a{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(checked(&many).is_err());
        assert!(checked(&many[..64]).is_ok());
        let err = checked(&["bad name"]).unwrap_err().to_string();
        assert!(err.contains("bad name"), "{err}");
    }

    #[test]
    fn stored_abilities_fail_closed() {
        assert_eq!(parse_stored(r#"["a","*"]"#).unwrap(), vec!["a", "*"]);
        assert_eq!(parse_stored("[]").unwrap(), Vec::<String>::new());
        for corrupt in [
            "",
            "{",
            r#"{"a":1}"#,
            r#"["a b"]"#,
            r#"[1]"#,
            "null",
            r#""*""#,
        ] {
            assert!(parse_stored(corrupt).is_none(), "{corrupt}");
        }
        let huge = serde_json::to_string(&vec!["a"; 10_000]).unwrap();
        assert!(parse_stored(&huge).is_none());
        let sixty_five = serde_json::to_string(&vec!["a"; 65]).unwrap();
        assert!(parse_stored(&sixty_five).is_none());
        let biggest: Vec<String> = (0..64).map(|i| format!("{i:0>100}")).collect();
        assert!(parse_stored(&serde_json::to_string(&biggest).unwrap()).is_some());
    }

    #[test]
    fn family_arguments_are_checked() {
        assert!(family(Need::All, "abilities", "a,b").is_ok());
        assert!(family(Need::Any, "ability", " a , * ").is_ok());
        for bad in ["", "a,", ",a", "a b", "a,,b"] {
            assert!(family(Need::All, "abilities", bad).is_err(), "{bad:?}");
        }
    }
}
