//! The [`CurrentToken`] handler argument.

use http::request::Parts;
use sea_orm::prelude::DateTimeUtc;
use smeltery_core::auth::{Authenticated, Credential};
use smeltery_core::{App, Error, Result};

use crate::guard::GUARD;
use crate::tokens::{AccessToken, Tokens};

/// The token the guard accepted for this request (a request extension).
#[derive(Clone, Debug)]
pub(crate) struct TokenMeta(pub(crate) AccessToken);

/// The token that authenticated this request, as a handler argument: on a route with `auth:hallmark` (or any
/// API route, where the stateless guards run for it). 401 when the request was not authenticated by a Hallmark
/// token (a session, no credential); `Option<CurrentToken>` gives `None` instead.
///
/// ```
/// use smeltery::hallmark::CurrentToken;
/// use smeltery::http::StatusCode;
///
/// /// `DELETE /api/tokens/current`: sign this device out.
/// async fn destroy(token: CurrentToken) -> smeltery::Result<StatusCode> {
///     token.revoke().await?;
///     Ok(StatusCode::NO_CONTENT)
/// }
/// # let _ = destroy;
/// ```
#[derive(Clone)]
pub struct CurrentToken {
    app: App,
    token: AccessToken,
}

impl std::fmt::Debug for CurrentToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurrentToken")
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl CurrentToken {
    /// The token's id.
    pub fn id(&self) -> i64 {
        self.token.id
    }

    /// Its user's id.
    pub fn user_id(&self) -> i64 {
        self.token.user_id
    }

    /// Its name.
    pub fn name(&self) -> &str {
        &self.token.name
    }

    /// Its abilities.
    pub fn abilities(&self) -> &[String] {
        &self.token.abilities
    }

    /// Whether it grants `ability` (listed by exact name, or `*`).
    pub fn can(&self, ability: &str) -> bool {
        self.token.can(ability)
    }

    /// When it stops working (`None` = never).
    pub fn expires_at(&self) -> Option<DateTimeUtc> {
        self.token.expires_at
    }

    /// The stored token.
    pub fn token(&self) -> &AccessToken {
        &self.token
    }

    /// Delete this token (the device signs out) and publish its revocation; `true` when it was still there.
    ///
    /// # Errors
    /// A query fails.
    pub async fn revoke(&self) -> Result<bool> {
        Tokens::of(&self.app)?
            .revoke(self.token.user_id, self.token.id)
            .await
    }
}

async fn find(parts: &mut Parts, app: &App) -> Result<Option<CurrentToken>> {
    let Some(principal) =
        <Authenticated as axum::extract::OptionalFromRequestParts<App>>::from_request_parts(
            parts, app,
        )
        .await?
    else {
        return Ok(None);
    };
    let Credential::Token { id, .. } = &principal.credential else {
        return Ok(None);
    };
    if principal.guard != GUARD {
        return Ok(None);
    }
    Ok(parts
        .extensions
        .get::<TokenMeta>()
        .filter(|meta| meta.0.id == *id && meta.0.user_id == principal.user_id)
        .map(|meta| CurrentToken {
            app: app.clone(),
            token: meta.0.clone(),
        }))
}

impl axum::extract::FromRequestParts<App> for CurrentToken {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        find(parts, app).await?.ok_or_else(Error::unauthorized)
    }
}

impl axum::extract::OptionalFromRequestParts<App> for CurrentToken {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut Parts,
        app: &App,
    ) -> std::result::Result<Option<Self>, Self::Rejection> {
        find(parts, app).await
    }
}
