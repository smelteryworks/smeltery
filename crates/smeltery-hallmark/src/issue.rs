//! Issuing tokens for an email and password (mobile, desktop and command-line clients) and signing a token out.

use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use smeltery_core::auth::verify_credentials;
use smeltery_core::http::ClientInfo;
use smeltery_core::{App, Error, Result};

use crate::extract::CurrentToken;
use crate::tokens::{MAX_NAME_LEN, NewToken, Tokens};

/// The message on `email` when the address and password do not match (the same for unknown addresses).
pub const FAILED: &str = "These credentials do not match our records.";

/// The message on `code` when the user needs a second factor and gave none.
pub const CODE_REQUIRED: &str = "A two-factor code is required.";

/// The message on `code` when the second factor is wrong.
pub const CODE_INVALID: &str = "The two-factor code is invalid.";

/// Check `email` and `password` and, when they match, create a token named `device_name` with `abilities` for that
/// user. The check is core's [`verify_credentials`]: the login budgets (shared with the web login), a dummy hash for
/// unknown addresses, the hash gate. Then the app's [`LoginPolicy`](smeltery_core::auth::LoginPolicy)s
/// ([`App::check_login`]) decide, as at the web sign-in. When a second factor is registered
/// ([`AppBuilder::second_factor`](smeltery_core::AppBuilder::second_factor)) and the user needs one, `code` must be
/// a valid code, before any token exists.
///
/// ```no_run
/// use serde::Deserialize;
/// use smeltery::hallmark::issue_for_credentials;
/// use smeltery::http::ClientInfo;
/// use smeltery::prelude::*;
///
/// #[derive(Deserialize)]
/// struct TokenRequest {
///     email: String,
///     password: String,
///     device_name: String,
///     code: Option<String>,
/// }
///
/// /// `POST /api/tokens` (with `throttle:10,1`).
/// async fn store(app: App, client: ClientInfo, Json(form): Json<TokenRequest>) -> Result<Response> {
///     let issued = issue_for_credentials(
///         &app, &client, &form.email, &form.password, &form.device_name, form.code.as_deref(), &["*"],
///     )
///     .await?;
///     Ok(issued.created())
/// }
/// # let _ = store;
/// ```
///
/// # Errors
/// 422 on `device_name` (1 to 255 characters, no control characters), on `email` ([`FAILED`]: wrong address or
/// password) or on `code` ([`CODE_REQUIRED`], [`CODE_INVALID`]); a login policy's refusal (its status, its message
/// on `email`); 429 past the login budgets or the second factor's budget (with `Retry-After`); 503 when too many
/// password checks wait; 400 for invalid abilities; a database failure.
pub async fn issue_for_credentials(
    app: &App,
    client: &ClientInfo,
    email: &str,
    password: &str,
    device_name: &str,
    code: Option<&str>,
    abilities: &[&str],
) -> Result<NewToken> {
    let count = device_name.chars().count();
    if !(1..=MAX_NAME_LEN).contains(&count) || device_name.chars().any(char::is_control) {
        return Err(Error::validation(
            "device_name",
            format!("The device name must be 1 to {MAX_NAME_LEN} characters."),
        ));
    }
    let tokens = Tokens::of(app)?;
    let ip = client
        .ip()
        .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    let Some(user) = verify_credentials(app, &ip, email, password).await? else {
        return Err(Error::validation("email", FAILED));
    };
    // The app's login policies (a suspended account, for example) decide before the second factor and any token.
    app.check_login(&user).await?;
    if let Some(second) = app.second_factor()
        && second.required(app, &user).await?
    {
        let code = code.map(str::trim).filter(|c| !c.is_empty());
        let Some(code) = code else {
            return Err(Error::validation("code", CODE_REQUIRED));
        };
        second
            .verify(app, &user, code)
            .await?
            .into_result("code", CODE_INVALID)?;
    }
    // Bound to the row the policies and the second factor judged: when the user's credentials end (or the password
    // changes) while this request runs, the new token ends at its first use.
    tokens.create_for(&user, device_name, abilities, None).await
}

impl NewToken {
    /// The answer of an issuing endpoint: 201 with [`to_json`](Self::to_json) and `Cache-Control: no-store`.
    pub fn created(&self) -> Response {
        let mut response = (StatusCode::CREATED, axum::Json(self.to_json())).into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

/// A handler for `DELETE /api/tokens/current` (behind `auth:hallmark`): the token of the request is deleted (the
/// device signs out) and 204 is answered.
///
/// ```
/// fn routes(r: &mut smeltery::routing::Router) {
///     r.delete("/tokens/current", smeltery::hallmark::revoke_current)
///         .middleware("auth:hallmark");
/// }
/// # let _ = routes;
/// ```
///
/// # Errors
/// A query fails.
pub async fn revoke_current(token: CurrentToken) -> Result<StatusCode> {
    token.revoke().await?;
    Ok(StatusCode::NO_CONTENT)
}
