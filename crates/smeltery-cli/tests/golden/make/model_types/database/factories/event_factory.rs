//! Fake `Event` records for tests and seeders.

use smeltery::db::factory::{Factory, Fake};
use smeltery::db::prelude::*;

use crate::app::models::event;

/// Builds `Event` records with fake values.
pub struct EventFactory;

impl Factory for EventFactory {
    type Entity = event::Entity;

    fn definition(&self, fake: &mut Fake) -> event::ActiveModel {
        event::ActiveModel {
            name: Set(fake.name()),
            notes: Set(Some(fake.paragraph())),
            seats: Set(fake.int(1..=100) as i32),
            views: Set(Some(fake.int(1..=1000))),
            open: Set(fake.bool()),
            price: Set(Some(fake.int(0..=10000) as f64 / 100.0)),
            day: Set(fake.date()),
            starts_at: Set(Some(DateTimeUtc::default())),
            meta: Set(smeltery::json!({})),
            code: Set(Uuid::parse_str(&fake.uuid()).unwrap_or_default()),
            user_id: Set(1),
            ..Default::default()
        }
    }
}
