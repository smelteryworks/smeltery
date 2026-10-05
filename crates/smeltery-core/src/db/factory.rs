//! Factories: model rows with made-up data, for seeders and tests, and [`Fake`], the small
//! deterministic generator they use.
//!
//! ```no_run
//! # extern crate smeltery_core as smeltery;
//! use smeltery::db::factory::{Factory, Fake};
//! use smeltery::db::prelude::*;
//! # mod app { pub mod models { pub mod post {
//! # use smeltery_core::db::prelude::*;
//! # #[sea_orm::model]
//! # #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
//! # #[sea_orm(table_name = "posts")]
//! # pub struct Model {
//! #     #[sea_orm(primary_key)]
//! #     pub id: i64,
//! #     pub title: String,
//! #     pub body: String,
//! # }
//! # impl ActiveModelBehavior for ActiveModel {}
//! # } } }
//!
//! pub struct PostFactory;
//!
//! impl Factory for PostFactory {
//!     type Entity = crate::app::models::post::Entity;
//!
//!     fn definition(&self, fake: &mut Fake) -> crate::app::models::post::ActiveModel {
//!         crate::app::models::post::ActiveModel {
//!             title: Set(fake.sentence(4)),
//!             body: Set(fake.paragraph()),
//!             ..Default::default()
//!         }
//!     }
//! }
//!
//! # async fn demo(db: Db) -> smeltery::Result<()> {
//! let post = PostFactory.create(&db).await?;
//! let posts = PostFactory.count(3).create(&db).await?;
//! # Ok(())
//! # }
//! # fn main() {}
//! ```

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use sea_orm::EntityTrait;

use crate::db::{Db, Record};
use crate::error::Result;

/// The SeaORM `ActiveModel` a factory builds.
pub type FactoryModel<F> = <<F as Factory>::Entity as EntityTrait>::ActiveModel;

/// The model row a factory creates.
pub type FactoryRow<F> = <<F as Factory>::Entity as EntityTrait>::Model;

/// Builds model rows with made-up data.
///
/// [`create`](Factory::create) inserts through [`Record::create`], so timestamps are set.
/// Every `create` call uses a new [`Fake`] with the next seed of a process-wide sequence:
/// rows differ from each other, and a test that runs alone gets the same data every time.
pub trait Factory: Sized + Send + Sync {
    /// The SeaORM entity of the model.
    type Entity: EntityTrait;

    /// The column values of one row (leave the primary key and timestamps unset).
    fn definition(&self, fake: &mut Fake) -> FactoryModel<Self>;

    /// The values of one row without saving it.
    fn make(&self, fake: &mut Fake) -> FactoryModel<Self> {
        self.definition(fake)
    }

    /// Insert one row.
    ///
    /// # Errors
    /// The insert fails.
    fn create(&self, db: &Db) -> impl Future<Output = Result<FactoryRow<Self>>> + Send
    where
        FactoryRow<Self>: Record<Entity = Self::Entity>,
        FactoryModel<Self>: Send,
    {
        self.create_with(db, |_| {})
    }

    /// Insert one row after `edit` changes its values (to set a foreign key, say).
    ///
    /// # Errors
    /// The insert fails.
    fn create_with<E>(
        &self,
        db: &Db,
        edit: E,
    ) -> impl Future<Output = Result<FactoryRow<Self>>> + Send
    where
        E: FnOnce(&mut FactoryModel<Self>) + Send,
        FactoryRow<Self>: Record<Entity = Self::Entity>,
        FactoryModel<Self>: Send,
    {
        let mut values = self.definition(&mut Fake::next());
        edit(&mut values);
        <FactoryRow<Self> as Record>::create(db, values)
    }

    /// `n` rows at once: `PostFactory.count(3).create(&db)`.
    fn count(self, n: usize) -> Many<Self> {
        Many { factory: self, n }
    }
}

/// Several rows from one factory, from [`Factory::count`].
#[derive(Debug)]
pub struct Many<F> {
    factory: F,
    n: usize,
}

impl<F: Factory> Many<F> {
    /// The values of every row, without saving them.
    pub fn make(&self, fake: &mut Fake) -> Vec<FactoryModel<F>> {
        (0..self.n).map(|_| self.factory.make(fake)).collect()
    }

    /// Insert every row, in order.
    ///
    /// # Errors
    /// An insert fails (the rows before it stay).
    pub async fn create(&self, db: &Db) -> Result<Vec<FactoryRow<F>>>
    where
        FactoryRow<F>: Record<Entity = F::Entity>,
        FactoryModel<F>: Send,
    {
        let mut rows = Vec::with_capacity(self.n);
        for _ in 0..self.n {
            rows.push(self.factory.create(db).await?);
        }
        Ok(rows)
    }
}

/// The seed sequence [`Fake::next`] draws from.
static NEXT_SEED: AtomicU64 = AtomicU64::new(1);

const WORDS: &[&str] = &[
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "ad",
    "minim",
    "veniam",
    "quis",
    "nostrud",
    "exercitation",
    "ullamco",
    "laboris",
    "nisi",
    "aliquip",
    "ex",
    "ea",
    "commodo",
    "consequat",
    "duis",
    "aute",
    "irure",
    "in",
    "reprehenderit",
    "voluptate",
    "velit",
    "esse",
    "cillum",
    "fugiat",
    "nulla",
    "pariatur",
    "excepteur",
    "sint",
    "occaecat",
    "cupidatat",
    "non",
    "proident",
    "sunt",
    "culpa",
    "qui",
    "officia",
    "deserunt",
    "mollit",
    "anim",
    "id",
    "est",
    "laborum",
];

const FIRST_NAMES: &[&str] = &[
    "Ada",
    "Alan",
    "Grace",
    "Linus",
    "Barbara",
    "Dennis",
    "Margaret",
    "Ken",
    "Radia",
    "Edsger",
    "Frances",
    "Donald",
    "Sophie",
    "John",
    "Hedy",
    "Tim",
    "Katherine",
    "Niklaus",
    "Joan",
    "Bjarne",
];

const LAST_NAMES: &[&str] = &[
    "Lovelace",
    "Turing",
    "Hopper",
    "Torvalds",
    "Liskov",
    "Ritchie",
    "Hamilton",
    "Thompson",
    "Perlman",
    "Dijkstra",
    "Allen",
    "Knuth",
    "Wilson",
    "Backus",
    "Lamarr",
    "Berners",
    "Johnson",
    "Wirth",
    "Clarke",
    "Stroustrup",
];

/// A small, deterministic generator of made-up data: the same seed gives the same values.
///
/// ```
/// use smeltery_core::db::factory::Fake;
///
/// let mut a = Fake::seeded(7);
/// let mut b = Fake::seeded(7);
/// assert_eq!(a.name(), b.name());
/// assert_eq!(a.sentence(5), b.sentence(5));
/// let n = a.int(1..=6);
/// assert!((1..=6).contains(&n));
/// ```
#[derive(Clone, Debug)]
pub struct Fake {
    state: u64,
    seed: u64,
    emails: u64,
}

impl Default for Fake {
    fn default() -> Self {
        Self::seeded(0)
    }
}

impl Fake {
    /// A generator with seed 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// A generator with this seed.
    pub fn seeded(seed: u64) -> Self {
        Self {
            state: seed ^ 0x5DEE_CE66_D1CE_4E5B,
            seed,
            emails: 0,
        }
    }

    /// A generator with the next seed of a process-wide sequence (what factories use).
    pub fn next() -> Self {
        Self::seeded(NEXT_SEED.fetch_add(1, Ordering::Relaxed))
    }

    /// The next raw 64-bit value (SplitMix64).
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (`n` > 0).
    fn below(&mut self, n: usize) -> usize {
        let n = n.max(1) as u64;
        usize::try_from(self.next_u64() % n).unwrap_or(0)
    }

    fn pick(&mut self, list: &[&'static str]) -> &'static str {
        let i = self.below(list.len());
        list.get(i).copied().unwrap_or("lorem")
    }

    /// One lowercase word.
    pub fn word(&mut self) -> String {
        self.pick(WORDS).to_owned()
    }

    /// `n` words separated by spaces.
    pub fn words(&mut self, n: usize) -> String {
        (0..n)
            .map(|_| self.pick(WORDS))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A sentence of `n` words (at least one): capitalised, ending with a period.
    pub fn sentence(&mut self, n: usize) -> String {
        let words = self.words(n.max(1));
        let mut chars = words.chars();
        let first = chars.next().map(|c| c.to_ascii_uppercase());
        let mut out: String = first.into_iter().chain(chars).collect();
        out.push('.');
        out
    }

    /// Three to six sentences.
    pub fn paragraph(&mut self) -> String {
        let n = 3 + self.below(4);
        (0..n)
            .map(|_| {
                let len = 4 + self.below(8);
                self.sentence(len)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A first name.
    pub fn first_name(&mut self) -> String {
        self.pick(FIRST_NAMES).to_owned()
    }

    /// A last name.
    pub fn last_name(&mut self) -> String {
        self.pick(LAST_NAMES).to_owned()
    }

    /// `First Last`.
    pub fn name(&mut self) -> String {
        format!("{} {}", self.first_name(), self.last_name())
    }

    /// An address at `example.com` (may repeat).
    pub fn email(&mut self) -> String {
        format!(
            "{}.{}@example.com",
            self.first_name().to_lowercase(),
            self.last_name().to_lowercase()
        )
    }

    /// An address at `example.com` that this generator never returns twice, and that
    /// generators with different seeds do not share.
    pub fn unique_email(&mut self) -> String {
        self.emails += 1;
        format!(
            "{}{}.{}@example.com",
            self.first_name().to_lowercase(),
            self.seed,
            self.emails
        )
    }

    /// An integer in the range.
    pub fn int(&mut self, range: std::ops::RangeInclusive<i64>) -> i64 {
        let (lo, hi) = (*range.start(), *range.end());
        if hi <= lo {
            return lo;
        }
        let span = hi.abs_diff(lo).saturating_add(1);
        let offset = if span == 0 {
            self.next_u64()
        } else {
            self.next_u64() % span
        };
        lo.wrapping_add_unsigned(offset)
    }

    /// `true` or `false`.
    pub fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    /// A made-up UUID (version 4 layout) as text, from the same deterministic sequence as every other value
    /// here: fine for seed data, never for identifiers or tokens that must be unguessable.
    pub fn uuid(&mut self) -> String {
        let a = self.next_u64();
        let b = self.next_u64();
        format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            a >> 32,
            (a >> 16) & 0xFFFF,
            a & 0x0FFF,
            ((b >> 48) & 0x3FFF) | 0x8000,
            b & 0xFFFF_FFFF_FFFF
        )
    }

    /// A date between 2000-01-01 and 2029-12-31.
    pub fn date(&mut self) -> sea_orm::prelude::Date {
        // 730_120 is 2000-01-01 counted in days from 0001-01-01 (day 1).
        let days = i32::try_from(730_120 + self.int(0..=10_956)).unwrap_or(730_120);
        sea_orm::prelude::Date::from_num_days_from_ce_opt(days).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_values() {
        let mut a = Fake::seeded(42);
        let mut b = Fake::seeded(42);
        for _ in 0..20 {
            assert_eq!(a.paragraph(), b.paragraph());
            assert_eq!(a.unique_email(), b.unique_email());
            assert_eq!(a.uuid(), b.uuid());
            assert_eq!(a.date(), b.date());
            assert_eq!(a.bool(), b.bool());
        }
        assert_ne!(Fake::seeded(1).words(8), Fake::seeded(2).words(8));
    }

    #[test]
    fn values_have_the_promised_shape() {
        let mut f = Fake::new();
        let s = f.sentence(4);
        assert_eq!(s.split(' ').count(), 4);
        assert!(s.ends_with('.') && s.chars().next().unwrap().is_uppercase());
        assert_eq!(f.words(3).split(' ').count(), 3);
        assert!(f.email().ends_with("@example.com"));
        let emails: std::collections::HashSet<_> = (0..100).map(|_| f.unique_email()).collect();
        assert_eq!(emails.len(), 100);
        for _ in 0..1000 {
            assert!((-3..=3).contains(&f.int(-3..=3)));
        }
        assert_eq!(f.int(5..=5), 5);
        let _ = f.int(i64::MIN..=i64::MAX);
        let uuid = f.uuid();
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.as_bytes()[14], b'4');
        let date = f.date().to_string();
        assert!(
            date.as_str() >= "2000-01-01" && date.as_str() <= "2029-12-31",
            "{date}"
        );
        assert_eq!(f.name().split(' ').count(), 2);
        let mut a = Fake::next();
        let mut b = Fake::next();
        assert_ne!(a.unique_email(), b.unique_email());
    }
}
