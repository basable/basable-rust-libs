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
//! # No batch table
//!
//! A batch key is the user's, from a batch table of their own or encoded
//! from what names the batch, and every line carries the batch's key, ledger
//! and type. A batch's balance is the sum of its lines over every process
//! ([`basable_ledger::Store::batch_sum`], over the expression index on the
//! lines' batch keys), and a batch is closed when a booking of the user's
//! brings that sum to zero: a VAT return filed for what the month's batch
//! held, a payout for what the clearing batch held. A line landing in the
//! batch afterwards is a remainder the next claim takes. Nothing is marked.
//!
//! # Balances are the user's projection
//!
//! The engine keeps no balance table: a position is the sum of lines, read
//! from the journals, and a claim sums its batch the same way. Whoever wants
//! a `batch_balance` to query keeps it themselves, from the journal table:
//! through change data capture into ClickHouse (a `SummingMergeTree` keyed
//! by batch key and unit, which drops a row whose summed columns are all
//! zero when it merges), or with a trigger. An `AFTER INSERT ON journal FOR
//! EACH STATEMENT` trigger with `REFERENCING NEW TABLE AS inserted` and one
//! set-based `INSERT INTO batch_balance ... SELECT ... FROM inserted,
//! jsonb_array_elements(inserted.lines) ... ON CONFLICT DO UPDATE SET amount
//! = batch_balance.amount + excluded.amount` is always consistent and needs
//! nothing to run; it should end by deleting the rows it brought to zero,
//! since a zero balance is a closed batch and the table should hold only
//! the open ones. **Warning: a trigger puts a balance back into the write
//! path. Every booking that touches a shared batch (this month's revenue, a
//! bank clearing batch) updates the same `batch_balance` row and waits for
//! the previous commit on it, so bookings serialize on the hot batches.
//! Invisible at a tenant's volume; a bottleneck at thousands of
//! transactions per second, which is exactly what the batch model exists to
//! avoid.** The check any such projection must pass is
//! `basable_ledger_testkit::projection`.

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
        assert!(
            m[0].up
                .contains("CREATE TABLE process_ledger_object_version")
        );
        assert!(!m[0].up.contains("CREATE TABLE batch"), "no batch table");
        assert!(MIGRATION.contains(SCHEMA_PLACEHOLDER));
    }
}
