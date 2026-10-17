//! The Postgres store of `basable-ledger`: the journal, batch and version
//! tables in the owning nanoservice's schema, the compare-and-set commit on
//! (process, sequence), batch creation with epochs, and the batch sum a
//! claim reads. The engine is `basable-ledger`'s; this crate is the
//! `Store` it runs on, over a `NanoPool`'s `PgPool`.
//!
//! The tables come as one dbmate migration ([`MIGRATION`], [`migrations`])
//! with `nano_ledger` as the schema and role placeholder, applied with the
//! replacement to the owning nanoservice's name.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod store;

use std::path::Path;

use basable_db::migrate::{MigrateError, Migration};

pub use store::PgStore;

/// The file name of the ledger's migration, as dbmate versions it.
pub const MIGRATION_FILE: &str = "20261017000001_ledger_tables.sql";

/// The ledger's migration, dbmate format, `nano_ledger` as the placeholder
/// for the owning nanoservice's schema and role.
pub const MIGRATION: &str = include_str!("../migrations/20261017000001_ledger_tables.sql");

/// The placeholder the migration names the schema and role by.
pub const SCHEMA_PLACEHOLDER: &str = "nano_ledger";

/// The ledger's migrations, parsed, ready for `basable_db::migrate::apply`
/// with the replacement `(SCHEMA_PLACEHOLDER, "nano_<name>")`.
pub fn migrations() -> Result<Vec<Migration>, MigrateError> {
    Ok(vec![basable_db::migrate::parse(
        Path::new(MIGRATION_FILE),
        MIGRATION,
    )?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_migration_parses() {
        let m = migrations().expect("parses");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].version, "20261017000001");
        assert!(m[0].up.contains("CREATE TABLE journal"));
        assert!(MIGRATION.contains(SCHEMA_PLACEHOLDER));
    }
}
