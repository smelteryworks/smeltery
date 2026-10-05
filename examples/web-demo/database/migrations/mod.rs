//! Database migrations, run in the order they are added below.

pub mod m2026_10_04_004654_create_users_table;
pub mod m2026_10_04_004655_create_password_reset_tokens_table;
pub mod m2026_10_04_004656_create_sessions_table;
pub mod m2026_10_04_004657_create_watchfire_tables;
pub mod m2026_10_04_004700_create_posts_table;
pub mod m2026_10_04_004701_add_image_to_posts_table;
pub mod m2026_10_04_004702_create_metrics_table;
pub mod m2026_10_04_004703_create_pages_table;
// smeltery:mods

use smeltery::db::migration::Migrator;

/// Register every migration, oldest first.
pub fn register(m: &mut Migrator) {
    m.add(m2026_10_04_004654_create_users_table::CreateUsersTable);
    m.add(m2026_10_04_004655_create_password_reset_tokens_table::CreatePasswordResetTokensTable);
    m.add(m2026_10_04_004656_create_sessions_table::CreateSessionsTable);
    m.add(m2026_10_04_004657_create_watchfire_tables::CreateWatchfireTables);
    m.add(m2026_10_04_004700_create_posts_table::CreatePostsTable);
    m.add(m2026_10_04_004701_add_image_to_posts_table::AddImageToPostsTable);
    m.add(m2026_10_04_004702_create_metrics_table::CreateMetricsTable);
    m.add(m2026_10_04_004703_create_pages_table::CreatePagesTable);
    // smeltery:migrations
}
