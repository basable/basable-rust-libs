//! The declared-effect vocabulary, one shape for every component's
//! declare-before-I/O marker. The component still owns everything durable:
//! the effect columns, their all-or-nothing CHECK, the adapter mapping, and
//! the write itself (the slot rides typed status through the claim's
//! `write_status`). This crate only names the in-memory value.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};

/// One declared remote effect: its operation, when it was declared, and —
/// once the call returned ambiguously — why it is unresolved. Only
/// pending/unknown live here; success and definitive failure are consumed
/// by the completion that observes them and never stored. One outstanding
/// effect per object: the slot's identity is the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectSlot {
    /// The declaring adapter's operation.
    pub operation: String,
    /// The receiver-facing identity for `SlotIdentity::Keyed` effects — the
    /// receiver holds a receipt under exactly this key, which is what makes
    /// a resolver's exact re-read possible. Row-scoped effects send no key
    /// to any receiver (the row's own durable state is the identity) and
    /// leave it empty; their status tables need no column for it.
    pub key: String,
    /// The DATABASE clock at declaration (the component's database `now()`,
    /// never the process clock): grace clocks run from it, and expiring one
    /// early abandons a paid box. The provenance is the caller's obligation
    /// — this crate executes no SQL and cannot verify it.
    pub declared_at: DateTime<Utc>,
    /// The ambiguity reason, empty while merely pending. A resolver pass
    /// that adds no evidence leaves it in place.
    pub detail: String,
}

/// A resolver's verdict about a possibly-sent effect, read from the
/// receiver: landed (`Succeeded`), rejected before acting (`Failed`), still
/// undecidable (`Unknown`), or proven safe to (re)send (`Superseded`) —
/// either the send verifiably never landed, or re-sending the same identity
/// is provably harmless. `Superseded` is the ONE verdict on which a caller
/// may dispatch again. This is the stored vocabulary; [`Resolution`] is the
/// typed verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttemptState {
    /// The effect landed.
    Succeeded,
    /// The receiver rejected it before acting.
    Failed,
    /// Still undecidable.
    Unknown,
    /// Proven safe to (re)send.
    Superseded,
}

impl AttemptState {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptState::Succeeded => "succeeded",
            AttemptState::Failed => "failed",
            AttemptState::Unknown => "unknown",
            AttemptState::Superseded => "superseded",
        }
    }
}

impl fmt::Display for AttemptState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stored attempt state that names none of the four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAttemptState(pub String);

impl fmt::Display for UnknownAttemptState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown attempt state {:?}", self.0)
    }
}

impl std::error::Error for UnknownAttemptState {}

impl FromStr for AttemptState {
    type Err = UnknownAttemptState;

    fn from_str(s: &str) -> Result<AttemptState, UnknownAttemptState> {
        match s {
            "succeeded" => Ok(AttemptState::Succeeded),
            "failed" => Ok(AttemptState::Failed),
            "unknown" => Ok(AttemptState::Unknown),
            "superseded" => Ok(AttemptState::Superseded),
            other => Err(UnknownAttemptState(other.to_owned())),
        }
    }
}

/// A `Declared` resolver's typed verdict. The result exists only for a
/// landed effect, and must then identify the LANDED effect —
/// receipt-sourced, never re-derived from current intent, so a resolver can
/// never credit work the receiver did not do. The other three carry the
/// resolver's reasoning — what the caller stores in the slot's `detail` or
/// reports in its retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution<R> {
    /// The effect landed; this is its receipt.
    Succeeded(R),
    /// The receiver rejected it before acting.
    Failed {
        /// The reasoning.
        detail: String,
    },
    /// Still undecidable.
    Unknown {
        /// The reasoning.
        detail: String,
    },
    /// Proven safe to (re)send.
    Superseded {
        /// The reasoning.
        detail: String,
    },
}

impl<R> Resolution<R> {
    /// The stored state of this verdict.
    pub fn state(&self) -> AttemptState {
        match self {
            Resolution::Succeeded(_) => AttemptState::Succeeded,
            Resolution::Failed { .. } => AttemptState::Failed,
            Resolution::Unknown { .. } => AttemptState::Unknown,
            Resolution::Superseded { .. } => AttemptState::Superseded,
        }
    }

    /// The reasoning, empty for a landed effect.
    pub fn detail(&self) -> &str {
        match self {
            Resolution::Succeeded(_) => "",
            Resolution::Failed { detail }
            | Resolution::Unknown { detail }
            | Resolution::Superseded { detail } => detail,
        }
    }

    /// The receipt, for a landed effect.
    pub fn result(&self) -> Option<&R> {
        match self {
            Resolution::Succeeded(r) => Some(r),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_states_round_trip_their_spelling() {
        for s in [
            AttemptState::Succeeded,
            AttemptState::Failed,
            AttemptState::Unknown,
            AttemptState::Superseded,
        ] {
            assert_eq!(s.as_str().parse::<AttemptState>(), Ok(s));
        }
        assert!("settled".parse::<AttemptState>().is_err());
    }

    #[test]
    fn a_resolution_names_its_state_and_detail() {
        let r: Resolution<u8> = Resolution::Superseded {
            detail: "absent".into(),
        };
        assert_eq!(r.state(), AttemptState::Superseded);
        assert_eq!(r.detail(), "absent");
        assert!(r.result().is_none());
        assert_eq!(Resolution::Succeeded(7).result(), Some(&7));
    }
}
