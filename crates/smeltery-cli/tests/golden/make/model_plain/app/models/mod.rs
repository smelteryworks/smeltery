//! Database models, one module per table.

pub mod post_comment;
pub mod user;
// smeltery:mods

pub use post_comment::Model as PostComment;
pub use user::Model as User;
// smeltery:models
