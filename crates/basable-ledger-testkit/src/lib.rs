//! The test harness of `basable-ledger`: the invoice and payment fixture a
//! suite books ([`fixture`]), the scenario DSL over the in-memory store
//! ([`scenario`]), the generator-driven properties ([`walk`]: every state
//! balances, path independence, replay books nothing) and the conformance
//! suite every `Store` adapter runs ([`conformance`]).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod conformance;
pub mod fixture;
pub mod scenario;
pub mod walk;

pub use scenario::Scenario;
