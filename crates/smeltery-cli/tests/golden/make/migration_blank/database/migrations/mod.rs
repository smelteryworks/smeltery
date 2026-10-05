//! Database migrations, run in the order they are added below.

pub mod m2026_10_01_090000_create_users_table;
pub mod m2026_10_01_090001_create_password_reset_tokens_table;
pub mod m2026_10_01_090002_create_sessions_table;
pub mod m2026_10_01_090003_create_watchfire_tables;
pub mod m2026_10_01_090004_create_cache_tables;
pub mod m2026_10_01_090005_create_pubsub_messages_table;
pub mod m2026_10_01_090006_add_two_factor_columns_to_users_table;
pub mod m2026_10_03_120000_backfill_slugs;
// smeltery:mods

use smeltery::db::migration::Migrator;

/// Register every migration, oldest first.
pub fn register(m: &mut Migrator) {
    m.add(m2026_10_01_090000_create_users_table::CreateUsersTable);
    m.add(m2026_10_01_090001_create_password_reset_tokens_table::CreatePasswordResetTokensTable);
    m.add(m2026_10_01_090002_create_sessions_table::CreateSessionsTable);
    m.add(m2026_10_01_090003_create_watchfire_tables::CreateWatchfireTables);
    m.add(m2026_10_01_090004_create_cache_tables::CreateCacheTables);
    m.add(m2026_10_01_090005_create_pubsub_messages_table::CreatePubsubMessagesTable);
    m.add(
        m2026_10_01_090006_add_two_factor_columns_to_users_table::AddTwoFactorColumnsToUsersTable,
    );
    m.add(m2026_10_03_120000_backfill_slugs::BackfillSlugs);
    // smeltery:migrations
}
