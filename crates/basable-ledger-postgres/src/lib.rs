//! The Postgres store of `basable-ledger`: the journal and version tables in
//! the owning nanoservice's schema, the compare-and-set commit on (process,
//! sequence), the batch sum a claim reads, and the restatement worklist. The
//! engine is `basable-ledger`'s; this crate is the `Store` it runs on, over
//! a `NanoPool`'s `PgPool`.
//!
//! The tables come as one dbmate migration with `nano_ledger` as the schema
//! and role placeholder, applied with the replacement to the owning
//! nanoservice's name: [`migrations`].
//!
//! # No batch table in the framework
//!
//! Each kind of batch has a table of the user's, one row per batch, reached
//! only through [`PgBatch`], and one sequence allocates `batch_key` over all
//! of them. A row lands in the commit's transaction with the first journal
//! that books on the batch, so a lost race or a refusal leaves none. A
//! booking that names no new batch stays one statement. A line carries the
//! key alone. A batch's balance is the sum of its lines over every process
//! ([`basable_ledger::Store::batch_sum`], over the expression index on the
//! lines' batch keys), and a batch is closed when a booking of the user's
//! brings that sum to zero: a VAT return filed for what the month's batch
//! held, a payout for what the clearing batch held. A line landing in the
//! batch afterwards is a remainder the next claim takes. Nothing is marked.

#[cfg(test)]
mod db_tests;
mod store;

use std::path::Path;

use basable_db::migrate::{MigrateError, Migration};

pub use store::{PgBatch, PgStore, stored_batch};

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
        assert!(
            m[0].up
                .contains("CREATE TABLE process_ledger_object_version")
        );
        assert!(!m[0].up.contains("CREATE TABLE batch"), "no batch table");
        assert!(MIGRATION.contains(SCHEMA_PLACEHOLDER));
    }
}
