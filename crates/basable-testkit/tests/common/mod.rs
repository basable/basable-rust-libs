//! Shared by the database-backed suites: the fixture migrations (the
//! scaffolder's templates rendered for two nanoservices, `orders` with one
//! processing-object type and `inventory` with one plain table) and the two
//! nanoservice markers.

// Each suite is its own binary and uses a subset of this module.
#![allow(dead_code)]

use std::path::PathBuf;

use basable_db::{Nanoservice, Stateful};
use basable_testkit::{TestDb, runfile};

pub struct Orders;
impl Nanoservice for Orders {
    const NAME: &'static str = "orders";
}
impl Stateful for Orders {}

pub struct Inventory;
impl Nanoservice for Inventory {
    const NAME: &'static str = "inventory";
}
impl Stateful for Inventory {}

pub fn fixtures() -> PathBuf {
    runfile("crates/basable-testkit/tests/fixtures/migrations")
}

/// The fixture database, or `None` without `TEST_DATABASE_URL`.
pub async fn fixture_db() -> Option<TestDb> {
    TestDb::from_env_with_migrations(&fixtures()).await
}

/// The SQLSTATE an error carries, for assertions.
pub fn sqlstate(err: &sqlx::Error) -> String {
    basable_db::sqlstate::of(err).unwrap_or("<none>").to_owned()
}
