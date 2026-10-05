//! Database models, one module per table.

pub mod event;
pub mod user;
// smeltery:mods

pub use event::Model as Event;
pub use user::Model as User;
// smeltery:models
