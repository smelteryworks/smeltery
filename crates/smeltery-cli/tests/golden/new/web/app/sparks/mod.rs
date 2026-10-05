//! Sparks: live components (state in Rust, view in `resources/views/sparks/`, no JavaScript to write).

pub mod counter;
// smeltery:mods

use smeltery::sparks::Sparks;

/// Register every Spark.
pub fn register(s: &mut Sparks) {
    s.add::<counter::Counter>();
    // smeltery:sparks
}
