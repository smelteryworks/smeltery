//! Broadcasting channels (Anvil): the public channels clients may subscribe to, and who may join each private
//! channel. `bootstrap/app.rs` installs them with `.anvil(routes::channels::channels)`.

use smeltery::anvil::{ChannelCtx, Channels};

/// Declares the app's channels. A public channel's events are public: anyone with the app key may subscribe.
pub fn channels(c: &mut Channels) {
    c.public("announcements");
    // `private-users.{id}`: only the signed-in user with that id may join (guests are refused before this runs).
    c.private("users.{user}", |ctx: ChannelCtx| async move {
        let user: i64 = ctx.param("user")?;
        Ok(ctx.user_id() == Some(user))
    });
    // smeltery:channels
}
