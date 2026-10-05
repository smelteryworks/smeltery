//! `POST /notify-me`: the dashboard's "Notify me" button sends the signed-in user an event on their private channel.

use smeltery::anvil::Anvil;
use smeltery::auth::Auth;
use smeltery::http::{Back, Redirect};

use crate::app::events::user_notified::UserNotified;

/// Broadcasts `UserNotified` to the signed-in user (every tab and device they have open), then goes back.
pub async fn notify_me(auth: Auth, anvil: Anvil, back: Back) -> smeltery::Result<Redirect> {
    if let Some(user_id) = auth.id() {
        let message = format!(
            "A message for you, sent at {} UTC.",
            smeltery::db::prelude::ChronoUtc::now().format("%H:%M:%S")
        );
        anvil.send(&UserNotified { user_id, message }).await?;
    }
    Ok(back.redirect())
}
