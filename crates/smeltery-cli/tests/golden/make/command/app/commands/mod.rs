//! Console commands of this app, run with `smeltery <name>`.

pub mod send_report;
// smeltery:mods

use smeltery::console::Commands;

/// Register every command.
pub fn register(c: &mut Commands) {
    let _ = &c; // keeps `c` used while no command is registered
    c.add(send_report::SendReport);
    // smeltery:commands
}
