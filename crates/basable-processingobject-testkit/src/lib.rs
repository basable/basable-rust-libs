//! The conformance substrate for `basable-processingobject`, the port of
//! the monorepo's `lib/processingobject/testkit`: a reserved test-only
//! processing-object type with its nanoservice, migrations and adapter
//! ([`conformance`]), a provider simulator ([`WidgetSim`]), the
//! [`Harness`] the conformance suites build scenarios from, and the running
//! side: [`ExampleReconciler`] with its hooks, [`Gate`], [`Replica`]. The conformance
//! TESTS live under `tests/` of this crate and port
//! `golang/test/processingobject` phase by phase.
//!
//! The conformance type is a nanoservice like any other: `nano_conformance`
//! role and schema, the partition it owns, typed tables that reference the
//! partition. Its migrations are applied on top of the tenant fixture's
//! framework migration by [`apply_schema`], as the migrator.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod conformance;
mod harness;
mod runtime;
mod widgetsim;

pub use conformance::{
    Conformance, ConformanceAdapter, PUBLIC_ID_PREFIX, Spec, Status, TYPE_KEY, TYPE_NAME,
    apply_schema, conformance_type, identity_name,
};
pub use harness::{
    ConformanceClaim, ConformanceStore, DriveError, DriveFn, EnvelopeSnapshot, Harness,
    fast_config, reconciled,
};
pub use runtime::{ExampleReconciler, Gate, Hook, HookFuture, Replica, hook, run_worker};
pub use widgetsim::{Order, WidgetClient, WidgetSim, canonical_key};
