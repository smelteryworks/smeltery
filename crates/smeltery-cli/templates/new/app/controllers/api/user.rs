//! `GET /api/user`: the user an API token belongs to.

use serde::Serialize;
use smeltery::db::prelude::DateTimeUtc;
use smeltery::prelude::*;

use crate::app::models::User;

/// What the API shows of a user: never the password hash, tokens or two-factor secrets.
#[derive(Debug, Serialize)]
pub struct UserJson {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub email_verified_at: Option<DateTimeUtc>,
}

/// The signed-in user (a token's user; a session's, on a first-party SPA in Hallmark's SPA mode).
pub async fn show(who: Authenticated, app: App) -> Result<Json<UserJson>> {
    let user: User = who.user(&app).await?.ok_or_else(Error::unauthorized)?;
    Ok(Json(UserJson {
        id: user.id,
        name: user.name,
        email: user.email,
        email_verified_at: user.email_verified_at,
    }))
}
