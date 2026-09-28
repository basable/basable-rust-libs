//! `basable-core` is the leaf of the basable framework crates: the handful of
//! types every other crate shares and nothing else. It has no dependencies.
//!
//! - [`names`]: the registry naming rules (a processing-object type name is a
//!   SQL identifier fragment; a public-id prefix is lowercase letters).
//! - [`labels`]: the label charset that makes a validated selector safe to
//!   render as a SQL literal.
//! - [`Deadline`]: a horizon held against BOTH the monotonic and the wall
//!   clock, the local ownership proof of a claim.
//! - [`BoxError`]: any error boxed with its source chain, for the call whose
//!   caller only displays or walks it.
//! - [`AppError`] and [`Code`]: the application error with the sixteen
//!   Connect codes, the boundary's error vocabulary.
//! - [`Ctx`]: the request context handed to every handler and reconciler.
//!
//! The Go originals in the basable monorepo are the specification
//! (`golang/controller/lib/processingobject/type.go`, `claim.go`,
//! `golang/controller/lib/messages/apperror`); semantics are identical
//! except where the type system makes an invariant mechanical, which
//! `docs/porting-notes.md` lists.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod ctx;
mod deadline;
mod error;
pub mod labels;
pub mod names;

pub use ctx::Ctx;
pub use deadline::Deadline;
pub use error::{AppError, BoxError, Code, PAYMENT_REQUIRED_MESSAGE};
