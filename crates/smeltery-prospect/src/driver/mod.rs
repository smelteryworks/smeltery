//! The drivers: `database` (the database's own full-text search) and `memory` (an in-process engine for tests).

pub(crate) mod database;
pub(crate) mod memory;
