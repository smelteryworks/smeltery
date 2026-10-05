//! Database migrations, run in the order they are added below.

pub mod m2026_10_04_010026_create_users_table;
pub mod m2026_10_04_010027_create_password_reset_tokens_table;
pub mod m2026_10_04_010028_create_sessions_table;
pub mod m2026_10_04_010029_create_watchfire_tables;
// smeltery:mods

use smeltery::db::migration::Migrator;

/// Register every migration, oldest first.
pub fn register(m: &mut Migrator) {
    m.add(m2026_10_04_010026_create_users_table::CreateUsersTable);
    m.add(m2026_10_04_010027_create_password_reset_tokens_table::CreatePasswordResetTokensTable);
    m.add(m2026_10_04_010028_create_sessions_table::CreateSessionsTable);
    m.add(m2026_10_04_010029_create_watchfire_tables::CreateWatchfireTables);
    // smeltery:migrations
}
