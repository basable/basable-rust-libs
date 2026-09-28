//! The framework's error: one enum, matched on by kind. The Go original
//! returned wrapped sentinels a caller branched on with `errors.Is`; here
//! every kind is a variant, and the statement that failed is named in the
//! `Sql` variants' `op` so a log line still says where.

use std::fmt;

use basable_core::BoxError;

use crate::model::{NamespacedName, Ref};

/// Why a store call failed.
#[derive(Debug)]
pub enum Error {
    /// No envelope row exists for the reference.
    NotFound(Ref),
    /// An intent write was refused because the object's deletion has been
    /// requested. Deletion is one-way (invariant 6).
    Deleting(Ref),
    /// A create found the `(namespace, name)` pair held by another live
    /// object of the type. The framework classifies this one itself because
    /// the envelope owns that key; whether to adopt the winner or report a
    /// conflict belongs to the caller.
    NameTaken {
        /// The object that asked for the name.
        r: Ref,
        /// The identity in question.
        name: NamespacedName,
    },
    /// Corruption the framework's own invariants forbid — an envelope whose
    /// typed rows are missing. Always a bug or manual schema surgery, never
    /// a data condition.
    Invariant(String),
    /// Claim authority was lost: the token no longer matches, the lease
    /// expired, or the local ownership proof lapsed. Terminal for the
    /// attempt, never for the object.
    Fenced,
    /// A programming error in a declaration or an API call: an invalid type
    /// name, a declaration the database's migrations do not know, a
    /// reference used with the wrong store.
    InvalidConfig(String),
    /// The caller's `update_spec` closure refused the mutation; nothing was
    /// written.
    Mutate(BoxError),
    /// A statement failed. `op` names it.
    Sql {
        /// What the framework was doing.
        op: String,
        /// The database's answer.
        source: sqlx::Error,
    },
    /// A COMMIT's acknowledgement never arrived: the transaction may or may
    /// not have landed. Every store write is safe to retry after this
    /// (a create adopts by id; the others re-apply against the committed
    /// row).
    CommitUnknown {
        /// What the framework was committing.
        op: String,
        /// The connection's error.
        source: sqlx::Error,
    },
}

impl Error {
    pub(crate) fn sql(op: impl Into<String>, source: sqlx::Error) -> Error {
        Error::Sql {
            op: op.into(),
            source,
        }
    }

    pub(crate) fn commit(op: impl Into<String>, source: sqlx::Error) -> Error {
        Error::CommitUnknown {
            op: op.into(),
            source,
        }
    }

    pub(crate) fn invalid(msg: impl Into<String>) -> Error {
        Error::InvalidConfig(msg.into())
    }

    /// Whether this is [`Error::NotFound`].
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::NotFound(_))
    }

    /// Whether this is [`Error::CommitUnknown`].
    pub fn is_commit_unknown(&self) -> bool {
        matches!(self, Error::CommitUnknown { .. })
    }

    /// The database error underneath, if any.
    pub fn sqlx(&self) -> Option<&sqlx::Error> {
        match self {
            Error::Sql { source, .. } | Error::CommitUnknown { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound(r) => write!(f, "processing object {r} not found"),
            Error::Deleting(r) => write!(f, "processing object {r} is deleting"),
            Error::NameTaken { r, name } => {
                write!(
                    f,
                    "create {r}: identity {name}: processing object name taken"
                )
            }
            Error::Invariant(msg) => write!(f, "processing object invariant violated: {msg}"),
            Error::Fenced => f.write_str("processing object claim fenced"),
            Error::InvalidConfig(msg) => {
                write!(f, "invalid processing object configuration: {msg}")
            }
            Error::Mutate(e) => write!(f, "update spec: {e}"),
            Error::Sql { op, source } => write!(f, "{op}: {source}"),
            Error::CommitUnknown { op, source } => {
                write!(f, "{op}: commit outcome unknown — safe to retry: {source}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sql { source, .. } | Error::CommitUnknown { source, .. } => Some(source),
            Error::Mutate(e) => Some(&**e),
            _ => None,
        }
    }
}
