//! Fake `Photo` records for tests and seeders.

use smeltery::db::factory::{Factory, Fake};
use smeltery::db::prelude::*;

use crate::app::models::photo;

/// Builds `Photo` records with fake values.
pub struct PhotoFactory;

impl Factory for PhotoFactory {
    type Entity = photo::Entity;

    fn definition(&self, fake: &mut Fake) -> photo::ActiveModel {
        photo::ActiveModel {
            title: Set(fake.sentence(4)),
            image: Set("files/example.txt".to_owned()),
            scan: Set(Some("files/example.txt".to_owned())),
            ..Default::default()
        }
    }
}
