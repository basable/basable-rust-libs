//! The crate's errors: a registry rejection at boot, a binder's refusal,
//! and everything a load or a repository call can fail with.

use std::error::Error;
use std::fmt;

use basable_core::BoxError;
use basable_core::names::InvalidName;

/// Why the config-type registry could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// Two types share an id.
    DuplicateId {
        /// The id.
        id: i16,
        /// The two names.
        names: (String, String),
    },
    /// Two types share a name.
    DuplicateName {
        /// The name.
        name: String,
        /// The two ids.
        ids: (i16, i16),
    },
    /// Two types share a public-id prefix.
    DuplicatePrefix {
        /// The prefix.
        prefix: String,
        /// The two names.
        names: (String, String),
    },
    /// A type's name is not a proto message name.
    InvalidTypeName {
        /// The name.
        name: String,
    },
    /// A type's prefix is not a valid public-id prefix.
    InvalidPrefix {
        /// The type.
        name: String,
        /// The rule it breaks.
        cause: InvalidName,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::DuplicateId { id, names } => write!(
                f,
                "config types {:?} and {:?} share id {id}",
                names.0, names.1
            ),
            RegistryError::DuplicateName { name, ids } => write!(
                f,
                "config type {name:?} is registered twice (ids {} and {})",
                ids.0, ids.1
            ),
            RegistryError::DuplicatePrefix { prefix, names } => write!(
                f,
                "config types {:?} and {:?} share prefix {prefix:?}",
                names.0, names.1
            ),
            RegistryError::InvalidTypeName { name } => write!(
                f,
                "config type name {name:?} must be a proto message name (PascalCase)"
            ),
            RegistryError::InvalidPrefix { name, cause } => {
                write!(f, "config type {name:?} prefix: {cause}")
            }
        }
    }
}

impl Error for RegistryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RegistryError::InvalidPrefix { cause, .. } => Some(cause),
            _ => None,
        }
    }
}

/// Why a binder did not do what it was asked.
#[derive(Debug)]
pub enum BinderError {
    /// The row is still named by another object; a prune retries once the
    /// referrers are gone, a repository delete reports it.
    StillReferenced(String),
    /// The message is not writable as given (a reference that is not a
    /// UUID, a timestamp that does not parse): the caller's input.
    Invalid(String),
    /// A statement failed.
    Sql(sqlx::Error),
    /// Anything else.
    Other(BoxError),
}

impl fmt::Display for BinderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinderError::StillReferenced(what) => write!(f, "object is still referenced: {what}"),
            BinderError::Invalid(what) => write!(f, "invalid: {what}"),
            BinderError::Sql(e) => fmt::Display::fmt(e, f),
            BinderError::Other(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl Error for BinderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            BinderError::Sql(e) => Some(e),
            BinderError::Other(e) => Some(&**e),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for BinderError {
    fn from(e: sqlx::Error) -> BinderError {
        BinderError::Sql(e)
    }
}

impl From<serde_json::Error> for BinderError {
    fn from(e: serde_json::Error) -> BinderError {
        BinderError::Invalid(format!("decode message: {e}"))
    }
}

impl From<uuid::Error> for BinderError {
    fn from(e: uuid::Error) -> BinderError {
        BinderError::Invalid(format!("expected a UUID: {e}"))
    }
}

impl From<BoxError> for BinderError {
    fn from(e: BoxError) -> BinderError {
        BinderError::Other(e)
    }
}

/// Why a load or a repository call failed.
#[derive(Debug)]
pub enum ConfigError {
    /// A seed file could not be read or parsed, or an item in it is
    /// malformed.
    Seed {
        /// The file, by base name.
        file: String,
        /// What is wrong.
        reason: String,
    },
    /// An environment name outside the closed vocabulary.
    Environment(String),
    /// A reference that does not parse, or points at an object the file set
    /// does not declare.
    Reference {
        /// The referring item.
        item: String,
        /// What is wrong.
        reason: String,
    },
    /// A cycle of references, as a path.
    Cycle(String),
    /// A type name no registered type carries.
    UnknownType(String),
    /// An item's body does not decode as its type's message.
    Decode {
        /// The item.
        item: String,
        /// The decoder's error.
        source: BoxError,
    },
    /// A binder failed or refused.
    Binder {
        /// The item.
        item: String,
        /// The binder's error.
        source: BinderError,
    },
    /// A statement failed.
    Sql {
        /// What was being done.
        op: String,
        /// The database's error.
        source: sqlx::Error,
    },
}

impl ConfigError {
    pub(crate) fn sql(op: impl Into<String>, source: sqlx::Error) -> ConfigError {
        ConfigError::Sql {
            op: op.into(),
            source,
        }
    }

    /// Whether this is a binder's [`BinderError::StillReferenced`].
    pub fn is_still_referenced(&self) -> bool {
        matches!(
            self,
            ConfigError::Binder {
                source: BinderError::StillReferenced(_),
                ..
            }
        )
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Seed { file, reason } => write!(f, "config file {file}: {reason}"),
            ConfigError::Environment(e) => {
                write!(f, "invalid environment {e:?} (valid: dev, prod, test)")
            }
            ConfigError::Reference { item, reason } => write!(f, "{item}: {reason}"),
            ConfigError::Cycle(path) => write!(f, "circular reference: {path}"),
            ConfigError::UnknownType(name) => write!(f, "unknown config type {name:?}"),
            ConfigError::Decode { item, source } => write!(f, "{item}: decode: {source}"),
            ConfigError::Binder { item, source } => write!(f, "{item}: {source}"),
            ConfigError::Sql { op, source } => write!(f, "{op}: {source}"),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConfigError::Decode { source, .. } => Some(&**source),
            ConfigError::Binder { source, .. } => Some(source),
            ConfigError::Sql { source, .. } => Some(source),
            _ => None,
        }
    }
}
