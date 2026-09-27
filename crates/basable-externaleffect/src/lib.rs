//! Typed building blocks for the external-API effect calls a
//! processing-object reconciler makes, expressing the six-clause effect
//! admission contract as validated adapter fields rather than review
//! comments. One [`Adapter`] per external call names, in code, what makes a
//! retry of that call safe; [`Call::new`] refuses an adapter that provides
//! no evidence; and the `basable-effecttest` crate proves the evidence it
//! does provide against the component's own simulator. A port of the
//! basable monorepo's `golang/controller/lib/externaleffect`.
//!
//! It is deliberately NOT a runtime: no tasks, no loop, no control-flow
//! inversion. Reconcilers keep their explicit phase matches and
//! resolve-vs-send branching; this crate supplies one validated handle
//! ([`Call`]) per external call, and small helpers the reconciler invokes in
//! sequence. It owns no tables, executes no SQL, and does not depend on
//! `basable-processingobject` — claim authority arrives through the
//! [`Owner`] trait (which a claim implements), and the declared-slot marker
//! is a plain value type the component stores in its own status columns and
//! writes through the claim's `write_status`.
//!
//! Clause → field map (the admission contract, made mechanical):
//!
//! ```text
//! 1 identity/idempotency key    → Adapter.key (+ Declared.slot_identity)
//! 2 replay / authoritative look → Adapter.strategy (the sealed sum)
//! 3 late-call ordering          → Adapter.late_call (a named, probed choice)
//! 5 irreversible effects        → Adapter.irreversible (restricts strategies)
//! 6 bounded call + deadline     → Adapter.call_timeout + dispatch's fence
//! ```
//!
//! Clause 4 (orphan sweeping) is deliberately out of scope: sweepers are
//! component-owned schedules over component-owned inventory.
//!
//! # The strategy sum
//!
//! Every external effect answers one question — *a retry cannot know
//! whether a previous attempt landed; what makes acting again safe?* — and
//! the closed [`Strategy`] enum names the four answers that exist, ordered
//! by how much help the receiver gives:
//!
//! | Strategy | Pre-send requirement | Re-entry resolution |
//! |---|---|---|
//! | `Idempotent` | nothing — the receiver converges/dedupes | re-send is the resolution |
//! | `LookBeforeAct` | `lookup` returned verified absence | look again; found ⇒ adopt, never re-send |
//! | `KeyedReplay` | durable key committed (caller's row) | same-key replay within `window` — `dispatch` refuses an intent at or past it; once the provider id is persisted, `resolve_keyed` forever |
//! | `Declared` | slot written through the claim first | the slot resolver, or `Hold` forever when no receiver postcondition exists |
//!
//! The claim an adapter makes is readable from its literal — `strategy:
//! Strategy::Declared { .. }` in a diff IS the admission argument.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod adapter;
mod call;
mod owner;
mod slot;
mod verdict;

pub use adapter::{
    Adapter, BoxFuture, DeclaredResolution, IntentAgeFn, InvalidAdapter, KeyFn, LateCall, LookupFn,
    ProviderIdFn, ResolveFn, ResolveKeyedFn, SendFn, SlotIdentity, Strategy, intent_age_fn, key_fn,
    lookup_fn, provider_id_fn, resolve_by_lookup, resolve_fn, resolve_keyed_fn, send_fn,
};
pub use call::{Call, DispatchError, METRICS_TARGET, ResolveError};
pub use owner::{Owner, OwnershipLost, Unfenced, check_ownership};
pub use slot::{AttemptState, EffectSlot, Resolution, UnknownAttemptState};
pub use verdict::{
    Cancelled, Classifier, DefinitiveError, TimedOut, TransportError, Verdict, classify_transport,
    definitive, is_definitive, is_transport,
};
