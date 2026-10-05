//! An example event on a private channel: a message for one user, on `private-users.<id>`.

use serde::Serialize;
use smeltery::anvil::BroadcastEvent;

/// Sent with `anvil.send(&UserNotified { user_id, message }).await?`. Only the user with that id may join the
/// channel (`routes/channels.rs`); clients receive it as `App\Events\UserNotified` with `{"user_id", "message"}`.
#[derive(Debug, Serialize, BroadcastEvent)]
#[broadcast(private = "users.{user_id}")]
pub struct UserNotified {
    pub user_id: i64,
    pub message: String,
}
