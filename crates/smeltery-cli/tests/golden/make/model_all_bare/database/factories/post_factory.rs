//! Fake `Post` records for tests and seeders.

use smeltery::db::factory::{Factory, Fake};

use crate::app::models::post;

/// Builds `Post` records with fake values.
pub struct PostFactory;

impl Factory for PostFactory {
    type Entity = post::Entity;

    fn definition(&self, _fake: &mut Fake) -> post::ActiveModel {
        post::ActiveModel {
            ..Default::default()
        }
    }
}
