//! The `SettingsSeeder` seeder.

use smeltery::db::Db;
use smeltery::db::seed::Seeder;

/// Fills the database with data.
pub struct SettingsSeeder;

impl Seeder for SettingsSeeder {
    async fn run(&self, _db: &Db) -> smeltery::Result<()> {
        // Create records here, e.g. with a factory: `PostFactory.count(10).create(_db).await?;`.
        Ok(())
    }
}
