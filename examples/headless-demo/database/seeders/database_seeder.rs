//! The main seeder: demo data for development.

use smeltery::auth::hash_password;
use smeltery::db::prelude::*;
use smeltery::db::seed::Seeder;

use crate::app::models::{User, user};

/// Creates a demo user (`demo@example.com`, password `password`) when the `users` table is empty.
pub struct DatabaseSeeder;

impl Seeder for DatabaseSeeder {
    async fn run(&self, db: &Db) -> smeltery::Result<()> {
        if User::count(db).await? == 0 {
            User::create(
                db,
                user::ActiveModel {
                    name: Set("Demo User".to_owned()),
                    email: Set("demo@example.com".to_owned()),
                    password: Set(hash_password("password").await?),
                    ..Default::default()
                },
            )
            .await?;
        }
        Ok(())
    }
}
