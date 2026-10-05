//! The main seeder: development data.

use smeltery::db::prelude::*;
use smeltery::db::seed::Seeder;

/// Runs on `smeltery db:seed`. Add development data here, for example with the factories in `database/factories/`:
/// `UserFactory.count(3).create(db).await?;`.
pub struct DatabaseSeeder;

impl Seeder for DatabaseSeeder {
    async fn run(&self, _db: &Db) -> smeltery::Result<()> {
        Ok(())
    }
}
