//! Fake `Post` records for tests and seeders.

use smeltery::db::factory::{Factory, Fake};
use smeltery::db::prelude::*;

use crate::app::models::post;

/// Builds `Post` records with fake values.
pub struct PostFactory;

impl Factory for PostFactory {
    type Entity = post::Entity;

    fn definition(&self, fake: &mut Fake) -> post::ActiveModel {
        post::ActiveModel {
            title: Set(fake.sentence(4)),
            body: Set(Some(fake.paragraph())),
            views: Set(fake.int(1..=100) as i32),
            rating: Set(Some(fake.int(0..=10000) as f64 / 100.0)),
            published: Set(fake.bool()),
            ..Default::default()
        }
    }
}
