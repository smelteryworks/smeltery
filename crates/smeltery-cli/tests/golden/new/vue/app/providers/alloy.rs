//! Alloy, the bridge to Vue: the HTML of a first visit and the props every page gets.

use smeltery::alloy::{Props, SharedCtx};

use crate::app::models::User;

/// The HTML of a first visit, `resources/views/app.mold.html`: it loads the Vite assets and embeds the page. Later
/// visits get the page as JSON.
#[derive(smeltery::Mold, Default)]
#[mold("app")]
pub struct Root {}

/// The signed-in user as pages see it: an allow-list of fields, never the model itself. Every prop is readable in
/// the browser.
#[derive(Debug, serde::Serialize)]
pub struct SharedUser {
    pub id: i64,
    pub name: String,
    pub email: String,
    /// Whether the e-mail address is verified.
    pub email_verified: bool,
    /// Whether logging in asks for a two-factor code (a confirmed enrolment).
    pub two_factor_enabled: bool,
}

impl From<User> for SharedUser {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            name: user.name,
            email: user.email,
            email_verified: user.email_verified_at.is_some(),
            two_factor_enabled: user.two_factor_confirmed_at.is_some(),
        }
    }
}

/// Props every page gets (`usePage().props` in the browser); a page prop with the same key wins. Never put secrets
/// here: everything is sent to the browser.
pub async fn shared(ctx: SharedCtx) -> smeltery::Result<Props> {
    let app = smeltery::json!({ "name": ctx.app().settings().name });
    let user = ctx.auth().user::<User>().await?.map(SharedUser::from);
    Ok(Props::new()
        .with("app", app)
        .with("auth", smeltery::json!({ "user": user })))
}
