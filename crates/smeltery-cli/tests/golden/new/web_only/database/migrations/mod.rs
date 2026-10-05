//! Database migrations, run in the order they are added below.

pub mod m2026_10_03_120000_create_users_table;
pub mod m2026_10_03_120001_create_password_reset_tokens_table;
pub mod m2026_10_03_120002_create_sessions_table;
pub mod m2026_10_03_120004_create_cache_tables;
pub mod m2026_10_03_120006_add_two_factor_columns_to_users_table;
// smeltery:mods

use smeltery::db::migration::Migrator;

/// Register every migration, oldest first.
pub fn register(m: &mut Migrator) {
    m.add(m2026_10_03_120000_create_users_table::CreateUsersTable);
    m.add(m2026_10_03_120001_create_password_reset_tokens_table::CreatePasswordResetTokensTable);
    m.add(m2026_10_03_120002_create_sessions_table::CreateSessionsTable);
    m.add(m2026_10_03_120004_create_cache_tables::CreateCacheTables);
    m.add(
        m2026_10_03_120006_add_two_factor_columns_to_users_table::AddTwoFactorColumnsToUsersTable,
    );
    // smeltery:migrations
}
