//! A declarative double-entry ledger: journals derived from the states
//! business objects are in, rather than written per event.
//!
//! The user owns three things. The **object**, their own type behind the
//! [`LedgerObject`] reader (identity, version, change time, one accounting
//! state). The **accounting** of that type, [`Accounting`]: which state
//! changes are allowed, the [`Desired`] positions that exist while an object
//! is in each state, and the balance checks a booking must pass. And the
//! **batches**, [`LedgerBatch`]: one value of the user's `Batch` enum is one
//! batch, and the framework knows of it only its primary key, its ledger and
//! its type, debit, credit or memo. There is no batch table in the framework:
//! each kind of batch has a table of the user's, one row per batch, and one
//! sequence allocates the key a stored line carries over all of them. A batch
//! is closed when a booking of the user's (a return filed, a payout made)
//! brings its balance to zero.
//!
//! The engine owns the rest. A call ([`Request`]) carries any mix of
//! registered types across processes. Per process it reads every journal,
//! folds the snapshot diffs into the current objects and sums the lines into
//! positions, finds the row of every batch the objects name, checks each
//! submitted object's version and state change, evaluates its desired state,
//! diffs it against the object's booked positions, checks that left equals
//! right per ledger and unit, creates the rows of the batches it uses first,
//! and appends one journal at the process's next sequence. The primary key on
//! (process, sequence) is the compare-and-set; a lost race is retried
//! inside [`Ledger::book`]. Every booking re-evaluates every object of its
//! process under the running rules, so a rules change is booked at the
//! process's next touch like any other difference; a bump of an
//! [`Accounting::VERSION`] also restates, once at the start of the
//! deployment that carries it ([`Ledger::restate_behind`]), every process
//! the store's version index still lists under the older number.
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

pub use accounting::{Accounting, LedgerBatch, Unit};
pub use balances::{Balances, ObjectBalances};
pub use desired::{Desired, Position, balance};
pub use error::{LedgerError, Rejection, StoreError};
pub use journal::{Journal, Line, SnapshotDiff, lines_from_json, lines_to_json};
pub use ledger::{
    Builder, DEFAULT_MAX_ATTEMPTS, Ledger, Outcome, Request, Restated, RestatementPreview,
    RestatementReport,
};
pub use memory::InMemoryStore;
pub use object::LedgerObject;
pub use store::{Commit, CommitOutcome, Store};
pub use types::{
    BatchKey, BatchType, LedgerKey, ObjectId, ProcessId, StoredBatch, Timestamp, UnitKey,
    amount_string,
};
