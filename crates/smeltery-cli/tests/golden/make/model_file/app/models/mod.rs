//! Database models, one module per table.

pub mod photo;
pub mod user;
// smeltery:mods

pub use photo::Model as Photo;
pub use user::Model as User;
// smeltery:models
