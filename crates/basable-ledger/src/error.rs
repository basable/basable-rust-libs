//! The errors: a rejection names why an object cannot book; a store error
//! is the backend's; a ledger error is what a call fails with.

use std::fmt;

use basable_core::BoxError;

use crate::types::{ObjectId, ProcessId};

/// Why an object was rejected. Rejections are decided by the engine or the
/// user's `validate_balances`; a rejected object parks with its reason,
/// together with the rest of its process's part of the call.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rejection {
    message: String,
}

impl Rejection {
    /// A rejection with its reason.
    pub fn new(message: impl Into<String>) -> Self {
        Rejection {
            message: message.into(),
        }
    }

    /// The reason.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Rejection {}

/// A store failed.
#[derive(Debug)]
pub enum StoreError {
    /// The backend's own error.
    Backend(BoxError),
    /// A stored row the engine cannot read: a journal's JSON, an unknown
    /// batch type, one batch key under two ledgers or types.
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Backend(e) => write!(f, "store: {e}"),
            StoreError::Corrupt(m) => write!(f, "store: corrupt: {m}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Backend(e) => Some(e.as_ref()),
            StoreError::Corrupt(_) => None,
        }
    }
}

/// A call to the ledger failed as a whole. Per-object refusals are
/// [`Outcome`](crate::Outcome)s, not errors.
#[derive(Debug)]
pub enum LedgerError {
    /// The store failed.
    Store(StoreError),
    /// An object of a type the ledger was not built with.
    UnknownType(String),
    /// Two objects in one call share an id but are different objects.
    DuplicateObject(ProcessId, ObjectId),
    /// Another writer kept winning the process's sequence.
    Contention {
        /// The process.
        process: ProcessId,
        /// How many times the commit was attempted.
        attempts: u32,
    },
    /// A restatement or a commit could not be planned for a process: a
    /// current object's desired state no longer balances, or names a booked
    /// batch under another ledger or type. Bookings report this per
    /// object as an [`Outcome`](crate::Outcome); a restatement has no object
    /// to report on.
    Rejected {
        /// The process.
        process: ProcessId,
        /// The reason.
        rejection: Rejection,
    },
    /// A stored snapshot no longer decodes into its type: the type changed
    /// without an upgrade of the stored diffs.
    Undecodable {
        /// The process.
        process: ProcessId,
        /// The object.
        object: ObjectId,
        /// serde's reason.
        reason: String,
    },
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LedgerError::Store(e) => write!(f, "{e}"),
            LedgerError::UnknownType(t) => write!(f, "object type {t:?} is not registered"),
            LedgerError::DuplicateObject(p, o) => {
                write!(
                    f,
                    "object {} of process {} appears twice in one call",
                    o.0, p.0
                )
            }
            LedgerError::Contention { process, attempts } => write!(
                f,
                "process {} kept changing under the call: {attempts} attempts",
                process.0
            ),
            LedgerError::Rejected { process, rejection } => {
                write!(f, "process {}: {rejection}", process.0)
            }
            LedgerError::Undecodable {
                process,
                object,
                reason,
            } => write!(
                f,
                "object {} of process {}: stored snapshot does not decode: {reason}",
                object.0, process.0
            ),
        }
    }
}

impl std::error::Error for LedgerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LedgerError::Store(e) => Some(e),
            _ => None,
        }
    }
}

impl From<StoreError> for LedgerError {
    fn from(e: StoreError) -> Self {
        LedgerError::Store(e)
    }
}
