//! The dashboard: the first page after logging in.

use smeltery::auth::Auth;
use smeltery::db::prelude::*;

use crate::app::models::{Page, User, page};

/// `resources/views/dashboard.mold.html`.
#[derive(smeltery::Mold)]
#[mold("dashboard")]
pub struct DashboardPage {
    /// The logged-in user's name.
    pub name: String,
    /// The pages the scraper stored last, newest first.
    pub pages: Vec<Page>,
    /// How many pages the scraper stored.
    pub page_count: u64,
}

/// `GET /dashboard` (behind the `auth` middleware).
pub async fn index(auth: Auth, db: Db) -> smeltery::Result<DashboardPage> {
    let name = auth
        .user::<User>()
        .await?
        .map(|u| u.name)
        .unwrap_or_default();
    let pages = Page::query()
        .order_by_desc(page::Column::UpdatedAt)
        .limit(10)
        .all(db.conn())
        .await?;
    Ok(DashboardPage {
        name,
        pages,
        page_count: Page::count(&db).await?,
    })
}
