//! An example event: an announcement every client on the public `announcements` channel receives.

use serde::Serialize;
use smeltery::anvil::BroadcastEvent;

/// Sent with `anvil.send(&AnnouncementPosted { … }).await?` (a handler takes `anvil: smeltery::anvil::Anvil`; jobs
/// and agents use `smeltery::anvil::Anvil::of(&app)`). Clients receive it as `App\Events\AnnouncementPosted` with
/// this struct's JSON as its data; `#[serde(skip)]` keeps a field out of it.
#[derive(Debug, Serialize, BroadcastEvent)]
#[broadcast(public = "announcements")]
pub struct AnnouncementPosted {
    pub message: String,
}
