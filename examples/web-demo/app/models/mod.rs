//! Database models, one module per table.

pub mod metric;
pub mod page;
pub mod post;
pub mod user;
// smeltery:mods

pub use metric::Model as Metric;
pub use page::Model as Page;
pub use post::Model as Post;
pub use user::Model as User;
// smeltery:models
