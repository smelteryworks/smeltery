//! Broadcasting channels (Anvil): the public channels clients may subscribe to. `bootstrap/app.rs` installs them
//! with `.anvil(routes::channels::channels)`.

use smeltery::anvil::Channels;

/// Declares the app's channels. A public channel's events are public: anyone with the app key may subscribe.
pub fn channels(c: &mut Channels) {
    c.public("announcements");
    // smeltery:channels
}
