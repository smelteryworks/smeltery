//! The `announcements` Spark, shown on the home page: the public `announcements` channel, live, without JavaScript.
//! Its view is `resources/views/sparks/announcements.mold.html`.

use serde::{Deserialize, Serialize};
use smeltery::prelude::*;

/// The newest announcements (`AnnouncementPosted`, `app/events/announcement_posted.rs`) since the page opened.
#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(name = "announcements", stream)]
pub struct Announcements {
    /// At most five, the newest first.
    pub messages: Vec<String>,
}

/// The event's data.
#[derive(Deserialize)]
pub struct Posted {
    pub message: String,
}

#[actions]
impl Announcements {
    /// Runs when `AnnouncementPosted` is broadcast on `announcements`, by any process of the app.
    #[on("anvil:announcements", "App\\Events\\AnnouncementPosted")]
    pub async fn posted(&mut self, event: Posted) -> Result<()> {
        self.messages.insert(0, event.message);
        self.messages.truncate(5);
        Ok(())
    }
}
