//! The basable messenger code generator, as a library. `routing.yaml` in,
//! two crates and a topology page out:
//!
//! 1. [`Spec::parse`]: the YAML as a line-aware tree ([`yaml`]), the JSON
//!    Schema check ([`SCHEMA_JSON`]), then the coded semantic rules
//!    ([`Code`]); every diagnostic carries its line.
//! 2. [`analyze`]: the message table (handlers and senders in declaration
//!    order, the one response kind every declaration agrees on) and the
//!    cycle analysis that boxes a feedback set of routes (`W_ROUTE_CYCLE`).
//! 3. [`emit`]: the `interfaces` and `messenger` crates as `prettyplease`
//!    output; [`docs::render`]: the Markdown topology.
//!
//! The semantics are the basable platform's (`golang/tools/codegen/
//! messenger-gen-v2` in the monorepo): a typed response is a strict 1:1
//! request with exactly one handler; `response: error` is a sequential,
//! fail-fast fan-out; no response is a void fan-out to 0..N handlers, in
//! declaration order. What Rust adds: static dispatch through generated
//! traits instead of interfaces, so a nanoservice sending a message it did
//! not declare does not compile, and the boxing of route cycles that
//! static dispatch makes necessary.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod diagnostic;
pub mod docs;
pub mod emit;
pub mod graph;
pub mod names;
pub mod spec;
pub mod yaml;

pub use diagnostic::{Code, Diagnostic, Diagnostics};
pub use emit::{Crate, EmitError, HEADER, emit};
pub use graph::{Analysis, analyze};
pub use spec::{Decl, Kind, Messenger, Nanoservice, RESPONSE_ERROR, SCHEMA_JSON, Spec};
