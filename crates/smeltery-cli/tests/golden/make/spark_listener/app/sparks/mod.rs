//! Sparks: live components (state in Rust, view in `resources/views/sparks/`, no JavaScript to write).

pub mod announcements;
pub mod counter;
pub mod notifications;
pub mod order_status;
// smeltery:mods

use smeltery::sparks::Sparks;

/// Register every Spark.
pub fn register(s: &mut Sparks) {
    s.add::<announcements::Announcements>();
    s.add::<counter::Counter>();
    s.add::<notifications::Notifications>();
    s.add::<order_status::OrderStatus>();
    // smeltery:sparks
}
