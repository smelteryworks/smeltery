//! Database seeders, run by `smeltery db:seed` in the order they are added below.

pub mod database_seeder;
pub mod post_seeder;
// smeltery:mods

use smeltery::db::seed::Seeders;

/// Register every seeder, in the order they run.
pub fn register(s: &mut Seeders) {
    s.add(database_seeder::DatabaseSeeder);
    s.add(post_seeder::PostSeeder);
    // smeltery:seeders
}
