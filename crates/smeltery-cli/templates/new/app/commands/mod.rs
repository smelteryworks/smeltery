//! Console commands of this app, run with `smeltery <name>`.

// smeltery:mods

use smeltery::console::Commands;

/// Register every command.
pub fn register(c: &mut Commands) {
    let _ = &c; // keeps `c` used while no command is registered
    // smeltery:commands
}
