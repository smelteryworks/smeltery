//! API tokens for mobile apps, desktop apps and other clients (Hallmark): `POST /api/tokens` issues one for an e-mail
//! address and a password, `DELETE /api/tokens/current` signs the presenting token out.

use serde::Deserialize;
use smeltery::hallmark::{CurrentToken, issue_for_credentials};
use smeltery::http::{ClientInfo, StatusCode};
use smeltery::prelude::*;

/// The body of `POST /api/tokens` (JSON or form fields).
#[derive(Debug, Deserialize, Validate)]
pub struct TokenRequest {
    /// Trimmed and lower-cased, as the login form reads it.
    #[validate(required, email, max = 255)]
    #[serde(deserialize_with = "smeltery::auth::deserialize_email")]
    pub email: String,
    #[validate(required)]
    pub password: String,
    /// A name for the token the user recognises, such as "Ada's phone".
    #[validate(required, max = 255)]
    pub device_name: String,
    /// A code from the authenticator app, for accounts with two-factor authentication on.
    pub code: Option<String>,
}

/// `POST /api/tokens`: checks the address and password with the login's budgets and two-factor rule, then answers
/// 201 `{"token":"smt_…","token_type":"Bearer","expires_at":…,"abilities":["*"]}` (`Cache-Control: no-store`). The
/// token is shown this once; only its hash is stored. Wrong credentials: 422 on `email`.
pub async fn store(
    app: App,
    client: ClientInfo,
    Valid(form): Valid<TokenRequest>,
) -> Result<Response> {
    let issued = issue_for_credentials(
        &app,
        &client,
        &form.email,
        &form.password,
        &form.device_name,
        form.code.as_deref(),
        &["*"],
    )
    .await?;
    Ok(issued.created())
}

/// `DELETE /api/tokens/current`: the token of this request is deleted (the device signs out); 204.
pub async fn destroy(token: CurrentToken) -> Result<StatusCode> {
    token.revoke().await?;
    Ok(StatusCode::NO_CONTENT)
}
