//! The dashboard: the first page after logging in.

use smeltery::alloy::{self, Page};
use smeltery::auth::Auth;
use smeltery::db::prelude::*;

use crate::app::models::User;

/// The dashboard's "Recent activity" card.
#[derive(Debug, Serialize)]
pub struct Activity {
    /// The accounts in this app.
    pub accounts: u64,
    /// When the signed-in user registered (UTC, `YYYY-MM-DD HH:MM`).
    pub member_since: Option<String>,
}

/// `GET /dashboard` (behind the `auth` and `verified` middleware), `resources/js/pages/dashboard.tsx`.
/// The user's name comes from the shared props (`app/providers/alloy.rs`); `activity` is deferred: the page shows
/// first, and the client asks for it right after.
pub async fn index(auth: Auth, db: Db) -> smeltery::Result<Page> {
    let member_since = auth
        .user::<User>()
        .await?
        .and_then(|u| u.created_at)
        .map(|at| at.format("%Y-%m-%d %H:%M").to_string());
    let page = alloy::render("dashboard").defer("activity", move || async move {
        Ok(Activity {
            accounts: User::count(&db).await?,
            member_since,
        })
    });
    Ok(page)
}
