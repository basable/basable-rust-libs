//! The library face of the generator binary. A tenant's `crates/messenger`
//! names this crate as a dev-dependency so that the lock carries it and
//! crate_universe builds the binary (`gen_binaries` in MODULE.bazel) — cargo
//! drops a dependency on a crate without a library target, so this target
//! exists for the lock, not for linking. The generator itself is
//! [`basable_messenger_codegen`], re-exported here for a build that wants
//! to drive it in-process.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use basable_messenger_codegen as codegen;
