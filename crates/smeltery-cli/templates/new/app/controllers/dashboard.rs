//! The dashboard: the first page after logging in.

use smeltery::auth::Auth;

use crate::app::models::User;

/// `resources/views/dashboard.mold.html`.
#[derive(smeltery::Mold)]
#[mold("dashboard")]
pub struct DashboardPage {
    /// The logged-in user's name.
    pub name: String,
}

/// `GET /dashboard` (behind the `auth` middleware).
pub async fn index(auth: Auth) -> smeltery::Result<DashboardPage> {
    let name = auth
        .user::<User>()
        .await?
        .map(|u| u.name)
        .unwrap_or_default();
    Ok(DashboardPage { name })
}
