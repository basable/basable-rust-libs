//! The per-adapter audit: each component's own test file builds a
//! [`Harness`] over its simulator or fake for ONE
//! [`basable_externaleffect::Call`] and runs [`run`] (or the [`audit!`]
//! macro), and the suite mechanically proves the admission evidence the
//! adapter declares — identity determinism, the ownership fence, the
//! classifier floor, and the strategy's safety property
//! (send-twice-lands-once, look-then-adopt, same-key replay,
//! resolve-never-resends). There is deliberately no registry to enumerate:
//! the component's test file calling `run` per adapter IS the audit index.
//! A port of the monorepo's `lib/externaleffect/effecttest`.
//!
//! [`AckLossProxy`] injects real ack loss into an HTTP provider client: the
//! request lands, the answer never arrives.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod harness;
mod probes;
mod proxy;

pub use harness::{FixedOwner, Harness, expired_owner, live_owner};
pub use probes::{ProbeOutcome, ProbeReport, run, try_run};
pub use proxy::{AckLossProxy, RequestHead};

/// Declares the audit of one adapter as a test: `audit!(name, factory)`,
/// where `factory` is an async closure (or `fn`) that builds a fresh
/// [`Harness`] each time it is called. Expands to a `#[tokio::test]`, so
/// the calling crate depends on `tokio` with the `macros` feature.
#[macro_export]
macro_rules! audit {
    ($name:ident, $factory:expr) => {
        #[::tokio::test]
        async fn $name() {
            $crate::run($factory).await;
        }
    };
}
