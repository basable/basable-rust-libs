//! The test harness of a basable tenant, the port of the monorepo's
//! `golang/test/testdb.go` and the processing-object testkit's
//! `commitfault.go`.
//!
//! A test asks for a [`TestDb`]: a fresh database on the Postgres
//! `TEST_DATABASE_URL` names, owned by an `app` login the harness creates
//! (CNPG creates it in production), with the tenant's dbmate migrations
//! applied as that login exactly as the migration Job applies them. From it
//! the test takes the [`MigratorPool`] (the `app` login itself) or a
//! [`NanoPool`] for one nanoservice, which is the pool the production
//! binary would hold: `SET ROLE nano_<name>`, `search_path` pinned. Without
//! `TEST_DATABASE_URL` the harness answers `None` and the test returns,
//! which is how the unit-test run stays green on a machine without Postgres
//! while the CI's Postgres service makes every suite run.
//!
//! [`CommitFaultProxy`] sits between a pool and Postgres and drops exactly
//! one COMMIT acknowledgement, in either of the two ways the wire cannot
//! tell apart: the commit landed, or it did not.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod commitfault;
mod testdb;

pub use commitfault::{CommitFault, CommitFaultProxy};
pub use testdb::{DATABASE_URL_VAR, MIGRATIONS_DIR_VAR, TestDb, TestDbError, runfile};
