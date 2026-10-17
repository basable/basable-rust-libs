//! The Postgres store of `basable-ledger`: the journal, batch and version
//! tables in the owning nanoservice's schema, the compare-and-set commit on
//! (process, sequence), batch creation with epochs, the batch sum a claim
//! reads, the batch lifecycle behind the horizon wait, and the optional
//! projector. The engine is `basable-ledger`'s; this crate is the `Store`
//! it runs on, over a `NanoPool`'s `PgPool`.
//!
//! The tables come as dbmate migrations with `nano_ledger` as the schema
//! and role placeholder, applied with the replacement to the owning
//! nanoservice's name: [`migrations`] for the ledger, and
//! [`projector_migrations`] only when the implementer runs the
//! [`Projector`].
//!
//! # A trigger instead of the projector, and its warning
//!
//! A user who wants `batch_balance` kept in the write transaction can add a
//! trigger instead of running the projector: an `AFTER INSERT ON journal
//! FOR EACH STATEMENT` trigger with `REFERENCING NEW TABLE AS inserted` and
//! one set-based `INSERT INTO batch_balance ... SELECT ... FROM inserted,
//! jsonb_array_elements(inserted.lines) ... ON CONFLICT DO UPDATE SET
//! amount = batch_balance.amount + excluded.amount`. Always consistent, no
//! projector to run. **Warning: this puts a balance back into the write
//! path. Every booking that touches a shared batch (this month's revenue, a
//! bank clearing batch) updates the same `batch_balance` row and waits for
//! the previous commit on it, so bookings serialize on the hot batches.
//! Invisible at a tenant's volume; a bottleneck at thousands of
//! transactions per second, which is exactly what the batch model exists to
//! avoid. Beyond that, feed ClickHouse through change data capture on the
//! journal table.**

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod lifecycle;
mod projector;
mod store;

use std::path::Path;

use basable_db::migrate::{MigrateError, Migration};

pub use lifecycle::{BatchLifecycle, Claim, LifecycleError};
pub use projector::{Applied, BalanceRow, Projector, ProjectorError};
pub use store::PgStore;

/// The file name of the ledger's migration, as dbmate versions it.
pub const MIGRATION_FILE: &str = "20261017000001_ledger_tables.sql";

/// The ledger's migration, dbmate format, `nano_ledger` as the placeholder
/// for the owning nanoservice's schema and role.
pub const MIGRATION: &str = include_str!("../migrations/20261017000001_ledger_tables.sql");

/// The file name of the optional projector's migration.
pub const PROJECTOR_MIGRATION_FILE: &str = "20261017000002_ledger_projector.sql";

/// The optional projector's migration: `batch_balance`, `projected_process`
/// and `projector_watermark`. Apply it only when running the projector.
pub const PROJECTOR_MIGRATION: &str =
    include_str!("../migrations/20261017000002_ledger_projector.sql");

/// The placeholder the migrations name the schema and role by.
pub const SCHEMA_PLACEHOLDER: &str = "nano_ledger";

/// The ledger's migrations, parsed, ready for `basable_db::migrate::apply`
/// with the replacement `(SCHEMA_PLACEHOLDER, "nano_<name>")`.
pub fn migrations() -> Result<Vec<Migration>, MigrateError> {
    Ok(vec![basable_db::migrate::parse(
        Path::new(MIGRATION_FILE),
        MIGRATION,
    )?])
}

/// The projector's migrations, parsed the same way.
pub fn projector_migrations() -> Result<Vec<Migration>, MigrateError> {
    Ok(vec![basable_db::migrate::parse(
        Path::new(PROJECTOR_MIGRATION_FILE),
        PROJECTOR_MIGRATION,
    )?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_migrations_parse() {
        let m = migrations().expect("parses");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].version, "20261017000001");
        assert!(m[0].up.contains("CREATE TABLE journal"));
        assert!(m[0].up.contains("CREATE TABLE batch_claim"));
        assert!(MIGRATION.contains(SCHEMA_PLACEHOLDER));
        let p = projector_migrations().expect("parses");
        assert_eq!(p[0].version, "20261017000002");
        assert!(p[0].up.contains("CREATE TABLE batch_balance"));
    }
}
