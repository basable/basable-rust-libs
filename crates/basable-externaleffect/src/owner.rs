//! Claim authority, as the one slice of it a dispatch needs: the local
//! ownership deadline.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use basable_core::Deadline;

/// The slice of claim authority a dispatch needs: the local ownership
/// proof. A processing-object claim implements it; nothing else about the
/// claim is visible here, so this crate can never write status or complete
/// an attempt.
pub trait Owner: Send + Sync {
    /// The proof's horizon, `None` once the claim is fenced.
    fn ownership_deadline(&self) -> Option<Deadline>;
}

/// The explicit [`Owner`] for a call site that holds no claim — the
/// request path, where a handler performs the effect synchronously and the
/// caller's retry is the recovery. Passing it is a deliberate, greppable
/// statement that this dispatch runs without claim authority.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unfenced;

impl Owner for Unfenced {
    /// Never fences: the far-future deadline also keeps a dispatch bound by
    /// `call_timeout` alone.
    fn ownership_deadline(&self) -> Option<Deadline> {
        Some(Deadline::after(Duration::from_secs(24 * 60 * 60)))
    }
}

/// The claim's ownership deadline passed before dispatch. Nothing was
/// sent; a plain retry is the whole story. A zombie whose lease expired
/// must not send a remote call, and the local deadline is the only proof
/// available (admission clause 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OwnershipLost;

impl fmt::Display for OwnershipLost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ownership deadline passed before dispatch")
    }
}

impl Error for OwnershipLost {}

/// The claim self-fence, checked immediately before every remote
/// observation or send: the live deadline, or [`OwnershipLost`] once it has
/// passed on either clock.
pub fn check_ownership(proof: &dyn Owner) -> Result<Deadline, OwnershipLost> {
    match proof.ownership_deadline() {
        Some(d) if d.is_live() => Ok(d),
        _ => Err(OwnershipLost),
    }
}
