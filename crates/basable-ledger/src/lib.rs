//! A declarative double-entry ledger: journals derived from the states
//! business objects are in, rather than written per event.
//!
//! The user owns three things. The **object**, their own type behind the
//! [`LedgerObject`] reader (identity, version, change time, one accounting
//! state). The **accounting** of that type, [`Accounting`]: which state
//! changes are allowed, the [`Desired`] positions that exist while an object
//! is in each state, and the balance checks a booking must pass. And the
//! **batches**, [`BatchAccount`]: the only three things the framework knows
//! about a batch are its key, its ledger and its type, debit, credit or
//! memo. There is no batch table: the key is the user's, from a table of
//! their own or encoded from what names the batch, every line carries key,
//! ledger and type, and a batch is closed when a booking of the user's (a
//! return filed, a payout made) brings its balance to zero.
//!
//! The engine owns the rest. A call ([`Request`]) carries any mix of
//! registered types across processes. Per process it reads every journal,
//! folds the snapshot diffs into the current objects and sums the lines into
//! positions, checks each submitted object's version and state change,
//! evaluates its desired state, diffs it against the object's booked
//! positions, checks that left equals right per ledger and unit, and appends
//! one journal at the process's next sequence. The primary key on (process,
//! sequence) is the compare-and-set; a lost race is retried inside
//! [`Ledger::book`]. A bump of an [`Accounting::VERSION`] restates every
//! process whose recorded version is behind, by appending journals: once at
//! the start of the deployment that carries it ([`Ledger::restate_behind`]),
//! and for any process left behind, at its next booking.
//!
//! Persistence is the [`Store`] trait: [`InMemoryStore`] here, Postgres in
//! `basable-ledger-postgres`. The scenario DSL, the property suites and the
//! store conformance suite are `basable-ledger-testkit`'s.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod accounting;
mod balances;
mod desired;
mod engine;
mod erased;
mod error;
mod journal;
pub mod jsonpatch;
mod ledger;
mod memory;
mod object;
mod store;
mod types;

pub use accounting::{Accounting, BatchAccount, Unit};
pub use balances::{Balances, ObjectBalances};
pub use desired::{Desired, Position, balance};
pub use error::{LedgerError, Rejection, StoreError};
pub use journal::{Journal, Line, SnapshotDiff};
pub use ledger::{
    Builder, DEFAULT_MAX_ATTEMPTS, Ledger, Outcome, Request, Restated, RestatementPreview,
    RestatementReport,
};
pub use memory::InMemoryStore;
pub use object::LedgerObject;
pub use store::{Commit, CommitOutcome, ProcessRead, Store};
pub use types::{
    Batch, BatchKey, BatchType, LedgerKey, ObjectId, ProcessId, Timestamp, UnitKey, amount_string,
};
