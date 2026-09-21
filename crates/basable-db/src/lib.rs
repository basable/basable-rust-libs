//! Postgres for basable nanoservices, the port of the monorepo's
//! `golang/lib/db` and the test harness's dbmate runner, shaped for one
//! database shared by several nanoservices (the Directive §2 made
//! mechanical, plan B3).
//!
//! - [`Nanoservice`] and [`Stateful`]: the markers a nanoservice crate
//!   implements on a unit type. A pool exists only for a `Stateful` one, so
//!   asking for a pool on a nanoservice that owns nothing is a compile error.
//! - [`NanoPool`]: the pool of one stateful nanoservice. Every connection is
//!   the `app` login switched to that nanoservice's role (`SET ROLE
//!   nano_<name>`) with `search_path` pinned to its schema, so a query
//!   against another nanoservice's tables is a Postgres permission error
//!   (SQLSTATE 42501), not a convention.
//! - [`MigratorPool`]: the `app` login itself, which owns every schema. It
//!   is a distinct type so it cannot be handed to nanoservice code; the
//!   testkit and local bootstrap are its only users.
//! - [`migrate`]: the dbmate-format runner (`-- migrate:up` / `--
//!   migrate:down`, one transaction per file, the `schema_migrations`
//!   ledger dbmate itself writes) and [`migrate::verify`], which a booting
//!   app uses to check the ledger holds every version it embeds. Production
//!   migrations run in the dbmate Job; the runner is for tests and local
//!   bootstrap.
//! - [`sqlstate`]: the SQLSTATE questions the frameworks ask of a
//!   `sqlx::Error` (a unique violation, a permission error).
//! - [`begin_snapshot`]: a `REPEATABLE READ READ ONLY` transaction.
//! - [`release_listen_conn`]: returns a connection that ran `LISTEN` to its
//!   pool with its subscriptions cleared, or discards it when that cannot be
//!   confirmed.
//!
//! Every error out of this crate is `sqlx::Error` or a typed enum of its
//! own ([`migrate::MigrateError`]); nothing is boxed and nothing is walked.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod listen;
pub mod migrate;
mod nanoservice;
mod pool;
mod snapshot;
pub mod sqlstate;

pub use listen::release_listen_conn;
pub use nanoservice::{Nanoservice, Stateful};
pub use pool::{MigratorPool, NanoPool, PoolConfig};
pub use snapshot::begin_snapshot;
