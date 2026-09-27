//! The declarative reconciliation framework: desired state lives in typed
//! spec rows under a generic envelope, and stateless multi-replica workers
//! drive the external world to match it, one exclusively claimed, fenced
//! attempt at a time. A port of the basable monorepo's
//! `golang/controller/lib/processingobject`; its contract is that package's
//! doc and the eight invariants below, and each module names the invariant
//! it implements.
//!
//! The pieces: the declaration ([`decl`]), the read model ([`Meta`],
//! [`Object`]), the reconciler's verdict ([`Outcome`]), [`TypedStore`]'s
//! create, mutate and read paths, the claimed attempt —
//! [`TypedStore::claim_batch`], [`Claim`] with its heartbeat,
//! `write_status` and `complete` — and the [`Worker`] that runs a type's
//! [`Reconciler`] over claimed attempts on one replica.
//!
//! # Invariants
//!
//! 1. The envelope owns identity, desired-state generation, scheduling,
//!    deletion intent, and claim authority. Typed spec/status tables carry
//!    domain columns only and hang off the envelope by composite foreign key
//!    (type key, id) with ON DELETE CASCADE.
//! 2. Every accepted spec mutation locks the envelope first and, in that one
//!    transaction: advances generation, stamps generation_changed_at,
//!    advances wake_seq, resets retry state, re-arms scheduling, and
//!    publishes a wake.
//! 3. Typed status is writable only under exact claim authority: the writing
//!    transaction locks the envelope and verifies the attempt's claim token,
//!    generation, and wake sequence before the adapter write. There is no
//!    unfenced status-write path.
//! 4. Claims are exclusive and leased: a per-attempt UUID token, a lease that
//!    heartbeats extend, and expiry that makes the row claimable by a
//!    successor. A holder self-fences on its local ownership proof
//!    ([`basable_core::Deadline`]) before every authoritative write.
//! 5. Completion is one fenced transaction: typed status and envelope
//!    scheduling commit together or not at all. An ambiguous commit is
//!    retried; if the retry cannot identify which outcome landed, the landed
//!    transaction is adopted and no post-completion callback runs.
//! 6. Deletion is one-way: `mark_deleted` stamps `deleted_at`, and only a
//!    reconcile pass that has confirmed external absence returns
//!    [`Outcome::Delete`], which removes the envelope. A teardown that stops
//!    making progress stays loud, surfaced by age, never by leaking the row.
//! 7. Remote I/O never runs inside a database transaction, while holding an
//!    envelope lock, or on a connection borrowed from a pool another
//!    transaction is using.
//! 8. Types are nanoservice-local and nanoservices are separated: the
//!    framework offers no cross-type write path, and across nanoservices the
//!    messenger is the only channel.
//!
//! # What the port makes mechanical
//!
//! The typed tables of a type live in its nanoservice's schema and the
//! envelope partition is owned by the nanoservice's role, so every framework
//! statement targets the PARTITION (`processing_object_<type>`) through the
//! nanoservice's [`basable_db::NanoPool`]; another nanoservice's rows are a
//! Postgres permission error. [`Outcome`] is an enum whose scheduling
//! modifiers exist only on `Converged`; the adapter is a trait, so a missing
//! callback is a compile error; the mutate closure of `update_spec` is
//! synchronous and sees the status by shared reference. `docs/porting-notes.md`
//! lists every deviation.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod claim;
mod complete;
pub mod decl;
mod error;
mod model;
mod outcome;
mod store;
mod store_mutate;
mod store_read;
mod tx;
mod worker;
mod writestatus;

pub use claim::{Claim, LEASE_SLACK, LeaseHandle};
pub use complete::Completion;
pub use decl::{Adapter, Backoff, ProcessingObjectType, WorkerConfig};
pub use error::Error;
pub use model::{
    Meta, NamespacedName, Object, Phase, Ref, Row, SCHEDULE_IMMEDIATE, SCHEDULE_PARKED,
    SCHEDULE_PARKED_SQL,
};
pub use outcome::{Outcome, Schedule};
pub use store::{CreateOptions, TypedStore, WAKE_CHANNEL};
pub use tx::Tx;
pub use worker::{AfterComplete, COMPLETION_TIMEOUT, NoAfterComplete, Reconciler, Worker};
