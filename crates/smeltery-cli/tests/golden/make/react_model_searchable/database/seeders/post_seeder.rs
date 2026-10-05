//! The `PostSeeder` seeder.

use smeltery::db::Db;
use smeltery::db::factory::Factory;
use smeltery::db::seed::Seeder;

use crate::database::factories::post_factory::PostFactory;

/// Creates 10 `Post` records with `PostFactory`.
pub struct PostSeeder;

impl Seeder for PostSeeder {
    async fn run(&self, db: &Db) -> smeltery::Result<()> {
        PostFactory.count(10).create(db).await?;
        Ok(())
    }
}
