//! Sparks: live components (state in Rust, view in `resources/views/sparks/`, no JavaScript to write).

pub mod counter;
pub mod live_counter;
pub mod post_image;
// smeltery:mods

use smeltery::sparks::Sparks;

/// Register every Spark.
pub fn register(s: &mut Sparks) {
    s.add::<counter::Counter>();
    s.add::<post_image::PostImage>();
    s.add::<live_counter::LiveCounter>();
    // smeltery:sparks
}
