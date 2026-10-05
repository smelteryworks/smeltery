//! Database models, one module per table.

pub mod post;
pub mod user;
// smeltery:mods

pub use post::Model as Post;
pub use user::Model as User;
// smeltery:models
