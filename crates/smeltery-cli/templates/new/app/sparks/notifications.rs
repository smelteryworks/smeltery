//! The `notifications` Spark, shown on the dashboard: the signed-in user's private channel `private-users.<id>`,
//! live, without JavaScript. Its view is `resources/views/sparks/notifications.mold.html`.

use std::sync::LazyLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smeltery::anvil::Anvil;
use smeltery::cache::RateLimiter;
use smeltery::prelude::*;

use crate::app::events::user_notified::UserNotified;

/// At most ten pings a minute per user: every ping is a broadcast (and a PubSub message between processes), and a
/// Spark action can be called in a loop. Counted in the app's cache, so every process shares the count.
static PINGS: LazyLock<RateLimiter> =
    LazyLock::new(|| RateLimiter::new("notifications.ping", 10, Duration::from_secs(60)));

/// The messages sent to the signed-in user (`UserNotified`, `app/events/user_notified.rs`) since the page opened.
#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(name = "notifications", stream)]
pub struct Notifications {
    /// The signed-in user, set on the first render. The channel rules (`routes/channels.rs`) check it again for every
    /// event, so another id would receive nothing.
    pub user_id: i64,
    /// At most five, the newest first.
    pub messages: Vec<String>,
}

/// The event's data.
#[derive(Deserialize)]
pub struct Notified {
    pub message: String,
}

#[actions]
impl Notifications {
    /// Runs on the first render.
    pub async fn mount(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        self.user_id = ctx.user_id().unwrap_or_default();
        Ok(())
    }

    /// `wire:click="ping"`: sends this user a `UserNotified`, which comes back through the listener below (and
    /// reaches every other tab the user has open). Past the limit of [`PINGS`] it sends nothing.
    pub async fn ping(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        let Some(user_id) = ctx.user_id() else {
            return Ok(());
        };
        let hit = PINGS.hit(ctx.app(), &format!("user:{user_id}")).await?;
        if !hit.allowed() {
            return Ok(());
        }
        let anvil =
            Anvil::of(ctx.app()).ok_or_else(|| Error::internal("Anvil is not installed"))?;
        let message = format!(
            "A message for you, sent at {} UTC.",
            smeltery::db::prelude::ChronoUtc::now().format("%H:%M:%S")
        );
        anvil.send(&UserNotified { user_id, message }).await?;
        Ok(())
    }

    /// Runs when `UserNotified` is broadcast on this user's private channel.
    #[on("anvil:private-users.{user_id}", "App\\Events\\UserNotified")]
    pub async fn notified(&mut self, event: Notified) -> Result<()> {
        self.messages.insert(0, event.message);
        self.messages.truncate(5);
        Ok(())
    }
}
