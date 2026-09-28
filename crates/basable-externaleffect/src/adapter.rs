//! The [`Adapter`] — one external call's complete admission evidence,
//! filled in by the component and validated fail-fast by [`Call::new`] —
//! and the closed [`Strategy`] sum that carries the clause-2 evidence. The
//! sum is closed deliberately: a new safety shape is a new admission
//! argument, made in this crate where its validation rules and effecttest
//! probes live, never improvised at a call site.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use basable_core::names::validate_type_name;
use basable_core::{BoxError, Ctx};

use crate::call::Call;
use crate::slot::{EffectSlot, Resolution};
use crate::verdict::Classifier;

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The ONE dispatch path for a call: the provider invocation, constructed
/// here and nowhere else, so dispatch's fences are the only road to the
/// wire. The context carries the bounded deadline.
pub type SendFn<A, R> =
    Box<dyn Fn(Ctx, A) -> BoxFuture<'static, Result<R, BoxError>> + Send + Sync>;
/// Derives the deterministic effect identity from the dispatch args.
pub type KeyFn<A> = Box<dyn Fn(&A) -> String + Send + Sync>;
/// The look-before-act observation: `Some` is the receipt, `None` is
/// VERIFIED absence, an error is "could not prove either way".
pub type LookupFn<A, R> =
    Box<dyn Fn(Ctx, A) -> BoxFuture<'static, Result<Option<R>, BoxError>> + Send + Sync>;
/// How old the durable intent behind the args is, on the caller's clock.
pub type IntentAgeFn<A> = Box<dyn Fn(&A) -> Duration + Send + Sync>;
/// The receiver identity carried by a landed result.
pub type ProviderIdFn<R> = Box<dyn Fn(&R) -> String + Send + Sync>;
/// The authoritative read by persisted provider identity.
pub type ResolveKeyedFn<R> =
    Box<dyn Fn(Ctx, String) -> BoxFuture<'static, Result<R, BoxError>> + Send + Sync>;
/// The declared-slot resolver: settles a slot found on claim WITHOUT
/// sending the effect.
pub type ResolveFn<RA, R> = Box<
    dyn Fn(Ctx, RA, EffectSlot) -> BoxFuture<'static, Result<Resolution<R>, BoxError>>
        + Send
        + Sync,
>;

/// Boxes a send closure: `send_fn(|ctx, args| async move { … })`.
pub fn send_fn<A, R, F, Fut>(f: F) -> SendFn<A, R>
where
    F: Fn(Ctx, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<R, BoxError>> + Send + 'static,
{
    Box::new(move |ctx, args| Box::pin(f(ctx, args)))
}

/// Boxes a key derivation: `key_fn(|args| format!(…))`.
pub fn key_fn<A>(f: impl Fn(&A) -> String + Send + Sync + 'static) -> KeyFn<A> {
    Box::new(f)
}

/// Boxes a lookup closure.
pub fn lookup_fn<A, R, F, Fut>(f: F) -> LookupFn<A, R>
where
    F: Fn(Ctx, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<R>, BoxError>> + Send + 'static,
{
    Box::new(move |ctx, args| Box::pin(f(ctx, args)))
}

/// Boxes an intent-age derivation.
pub fn intent_age_fn<A>(f: impl Fn(&A) -> Duration + Send + Sync + 'static) -> IntentAgeFn<A> {
    Box::new(f)
}

/// Boxes a provider-id extraction.
pub fn provider_id_fn<R>(f: impl Fn(&R) -> String + Send + Sync + 'static) -> ProviderIdFn<R> {
    Box::new(f)
}

/// Boxes a resolve-by-provider-id closure.
pub fn resolve_keyed_fn<R, F, Fut>(f: F) -> ResolveKeyedFn<R>
where
    F: Fn(Ctx, String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<R, BoxError>> + Send + 'static,
{
    Box::new(move |ctx, id| Box::pin(f(ctx, id)))
}

/// Boxes a declared-slot resolver.
pub fn resolve_fn<RA, R, F, Fut>(f: F) -> ResolveFn<RA, R>
where
    F: Fn(Ctx, RA, EffectSlot) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Resolution<R>, BoxError>> + Send + 'static,
{
    Box::new(move |ctx, args, slot| Box::pin(f(ctx, args, slot)))
}

/// Adapts an exact-observation lookup into a `Declared` resolver: found is
/// the receipt — the effect landed and the found value identifies it
/// (`Succeeded`); verified absence proves the send never landed
/// (`Superseded` — clear the slot and declare fresh); a lookup error proves
/// nothing (returned as an error; the slot stays). Sound only for an
/// identity that is never reused, so a hit is always this row's own.
pub fn resolve_by_lookup<RA, R>(lookup: LookupFn<RA, R>) -> ResolveFn<RA, R>
where
    RA: Send + 'static,
    R: Send + 'static,
{
    Box::new(move |ctx, args, _slot| {
        let fut = lookup(ctx, args);
        Box::pin(async move {
            match fut.await? {
                Some(found) => Ok(Resolution::Succeeded(found)),
                None => Ok(Resolution::Superseded {
                    detail: "verifiably absent at the receiver".into(),
                }),
            }
        })
    })
}

/// The clause-3 evidence: why a dispatch from a stale attempt (an older
/// generation's zombie that passed its fences before stalling) cannot
/// corrupt newer state. Same-key idempotency alone does not order create
/// versus delete, so a choice here is mandatory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LateCall {
    /// The send derives everything from durable args and re-asserts rather
    /// than accumulates — a late duplicate is a no-op or a harmless
    /// re-assertion at the receiver (a server-side apply, an
    /// ensure-by-deterministic-name, a hardware reset). effecttest asserts
    /// that a stale intent dispatched after a newer one landed creates
    /// nothing new at the receiver.
    Convergent,
    /// The key embeds the intent scope (generation, member vector, charge
    /// id), so a late call lands under an old identity that current readers
    /// ignore. effecttest asserts a scope advance changes the key.
    KeyScoped,
    /// A durable, independently scheduled, component-owned compensator (a
    /// teardown sweep, an orphan settlement) collects a late call's residue.
    /// effecttest requires the harness's compensation probe — the component
    /// authors the assertion; the kit refuses to pass without it running.
    Compensated,
}

/// Where a `Declared` effect's clause-1 identity lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotIdentity {
    /// `Adapter::key` derives the receiver-facing key; `declare` stamps it
    /// into the slot, and the receiver holds a receipt under exactly it.
    Keyed,
    /// The object row's own durable state is the identity (a stored
    /// never-reused name; the one-slot-per-row rule itself). `declare`
    /// leaves the slot's key empty.
    RowScoped,
}

/// The closed clause-2 evidence: exactly one of the four shapes, ordered by
/// how much help the receiver gives a retry.
///
/// `A` is the DISPATCH payload; `RA` is the RESOLVE identity — what a later
/// attempt needs to go LOOK at a possibly-sent effect, which for a
/// `Declared` effect is often less than (or different from) what the send
/// carried. Most effects use one type for both and simply name it twice;
/// an adapter whose two inputs genuinely differ names both, and which
/// fields feed which path becomes a compile-time fact instead of a comment.
pub enum Strategy<A, RA, R> {
    /// The send itself is replay-safe by construction — it derives every
    /// receiver identity deterministically from durable args and converges
    /// or dedupes at the receiver (a server-side apply; an ensure that gets
    /// by name before creating). The evidence is behavioural, not
    /// structural, so effecttest proves it against the component's
    /// simulator: the same args sent twice land exactly one effect.
    Idempotent,
    /// An authoritative observation by the deterministic identity in the
    /// args gates the send — the send itself is NOT replay-safe (a second
    /// install against an unobserved existing release is the hazard the
    /// gate exists to prevent). A retry is safe only through the gate.
    LookBeforeAct {
        /// The observation.
        lookup: LookupFn<A, R>,
    },
    /// The provider replays the original result for the same idempotency
    /// key within `window`; beyond it, the authoritative per-effect read by
    /// the PERSISTED provider identity is the only safe path (a payment
    /// provider that prunes keys after a day will happily mint a second
    /// live payment for a "replay" past the window).
    KeyedReplay {
        /// The provider-documented safe replay horizon, and a RUNTIME gate:
        /// dispatch refuses a send whose intent age is at or past it, because
        /// the provider may have pruned the key and a "replay" would then
        /// mint a second live effect. Past the window the only admissible
        /// paths are `resolve_keyed` by the persisted provider id or the
        /// component's own settlement (a webhook that names the payment, a
        /// human).
        window: Duration,
        /// How long the durable intent behind the args — the row that minted
        /// the key — has existed, on the caller's clock (a reconciler
        /// measures its database `now()` against the row's `created_at`).
        intent_age: IntentAgeFn<A>,
        /// Extracts the receiver identity from a landed result. The caller
        /// must persist it in its own durable state before relying on it;
        /// once persisted, `resolve_keyed` is the only admissible path for
        /// that effect.
        provider_id: ProviderIdFn<R>,
        /// Reads the authoritative outcome by persisted provider identity.
        /// It must ERROR for an unknown id — an unknown outcome must never
        /// read as settled.
        resolve: ResolveKeyedFn<R>,
    },
    /// Declare-before-I/O on a component-owned slot, written through the
    /// claim's `write_status` by the same attempt that then dispatches. The
    /// receiver offers nothing a blind retry could lean on, so the durable
    /// marker is the only fence: a slot found on claim means "may have been
    /// sent", and the caller MUST route it through [`Call::resolve`], never
    /// dispatch.
    Declared {
        /// Where the identity lives.
        slot_identity: SlotIdentity,
        /// How a found slot is settled.
        resolution: DeclaredResolution<RA, R>,
    },
}

/// How a `Declared` slot found on claim is settled.
pub enum DeclaredResolution<RA, R> {
    /// Settle WITHOUT sending this effect: by exact lookup over a stored
    /// never-reused identity ([`resolve_by_lookup`]), by receiver receipt
    /// under the slot's key, by probing the effect's postcondition on a
    /// grace read from `declared_at`, or by component settlement that may
    /// read the component's own tables.
    Resolve(ResolveFn<RA, R>),
    /// No receiver postcondition exists (a disk flash over SSH has no
    /// receiver identity, and an error cannot prove the bytes never
    /// landed). Any ambiguity holds the slot forever; manual, audited
    /// resolution is the only exit. Requires `Adapter::classify` to be
    /// `None`: with no way to prove "never landed", calling any error
    /// definitive would be a lie, so every dispatch error is ambiguous —
    /// including a structured pre-flight rejection, which is the accepted
    /// cost.
    Hold,
}

impl<A, RA, R> Strategy<A, RA, R> {
    /// The strategy's name, for logs and panics.
    pub fn name(&self) -> &'static str {
        match self {
            Strategy::Idempotent => "Idempotent",
            Strategy::LookBeforeAct { .. } => "LookBeforeAct",
            Strategy::KeyedReplay { .. } => "KeyedReplay",
            Strategy::Declared { .. } => "Declared",
        }
    }

    /// Whether this is `Declared { resolution: Hold }`.
    pub fn holds(&self) -> bool {
        matches!(
            self,
            Strategy::Declared {
                resolution: DeclaredResolution::Hold,
                ..
            }
        )
    }
}

impl<A, RA, R> fmt::Debug for Strategy<A, RA, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Strategy::KeyedReplay { window, .. } => f
                .debug_struct("KeyedReplay")
                .field("window", window)
                .finish_non_exhaustive(),
            Strategy::Declared {
                slot_identity,
                resolution,
            } => f
                .debug_struct("Declared")
                .field("slot_identity", slot_identity)
                .field(
                    "resolution",
                    &match resolution {
                        DeclaredResolution::Resolve(_) => "Resolve",
                        DeclaredResolution::Hold => "Hold",
                    },
                )
                .finish(),
            other => f.write_str(other.name()),
        }
    }
}

/// ONE external call's complete admission evidence — a struct of fields the
/// component fills, validated fail-fast by [`Call::new`]. An adapter that
/// provides no evidence cannot be constructed; which evidence it provides
/// is readable from the literal itself. `A` feeds `key`/`send` (declare
/// and dispatch); `RA` feeds a `Declared` resolver — name one type twice
/// when they coincide.
pub struct Adapter<A, RA, R> {
    /// Names the effect: lowercase snake_case, at most 64 characters,
    /// unique within the component. Stamped into declared slots and error
    /// text.
    pub operation: String,
    /// Derives the deterministic effect identity from the args (clause 1).
    /// Equal args MUST yield equal keys, and a fresh key comes only from
    /// fresh durable intent — never a retry count. Required for
    /// `KeyedReplay`, `Declared { Keyed }` and `LateCall::KeyScoped`;
    /// optional elsewhere, where identity is proven behaviourally instead.
    pub key: Option<KeyFn<A>>,
    /// The ONE dispatch path for this call.
    pub send: SendFn<A, R>,
    /// Judges send errors — [`Classifier::transport`], refined per
    /// provider, for a reversible effect; [`Classifier::fail_closed`] over
    /// the provider's definitive proof for an irreversible one. Required,
    /// except under `Declared { Hold }`, where it must be `None`.
    pub classify: Option<Classifier>,
    /// Bounds one dispatch (clause 6). Dispatch additionally caps it at the
    /// remaining ownership deadline, so no call outlives the lease that
    /// fenced it.
    pub call_timeout: Duration,
    /// Marks clause-5 effects: charges, paid orders, destructive writes. It
    /// restricts the admissible strategies to those carrying durable intent
    /// (`KeyedReplay`, `Declared`) and makes effecttest's ack-loss probes
    /// mandatory rather than skippable.
    pub irreversible: bool,
    /// The clause-3 evidence.
    pub late_call: LateCall,
    /// The clause-2 evidence.
    pub strategy: Strategy<A, RA, R>,
}

/// A construction-time rejection: the first violation, named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidAdapter {
    /// The adapter's operation name as given.
    pub operation: String,
    /// The violation.
    pub reason: String,
}

impl fmt::Display for InvalidAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid adapter: effect {:?}: {}",
            self.operation, self.reason
        )
    }
}

impl Error for InvalidAdapter {}

impl<A, RA, R> Adapter<A, RA, R> {
    pub(crate) fn validated(self) -> Result<Adapter<A, RA, R>, InvalidAdapter> {
        let fail = |reason: String| InvalidAdapter {
            operation: self.operation.clone(),
            reason,
        };
        if let Err(e) = validate_type_name(&self.operation) {
            return Err(fail(format!("operation: {e}")));
        }
        if self.call_timeout.is_zero() {
            return Err(fail("call_timeout must be positive".into()));
        }
        if self.late_call == LateCall::KeyScoped && self.key.is_none() {
            return Err(fail(
                "LateCall::KeyScoped requires key — the key IS the intent scope".into(),
            ));
        }
        match &self.strategy {
            Strategy::Idempotent | Strategy::LookBeforeAct { .. } => {}
            Strategy::KeyedReplay { window, .. } => {
                if self.key.is_none() {
                    return Err(fail("KeyedReplay requires key".into()));
                }
                if window.is_zero() {
                    return Err(fail("KeyedReplay.window must be positive".into()));
                }
            }
            Strategy::Declared { slot_identity, .. } => {
                if *slot_identity == SlotIdentity::Keyed && self.key.is_none() {
                    return Err(fail("Declared { Keyed } requires key".into()));
                }
            }
        }
        match (self.strategy.holds(), self.classify.is_some()) {
            (true, true) => {
                return Err(fail(
                    "Declared { Hold } requires classify = None — every error is ambiguous by policy"
                        .into(),
                ));
            }
            (false, false) => return Err(fail("classify is required".into())),
            _ => {}
        }
        if self.irreversible
            && !matches!(
                self.strategy,
                Strategy::KeyedReplay { .. } | Strategy::Declared { .. }
            )
        {
            return Err(fail(
                "irreversible requires KeyedReplay or Declared — clause 5 needs durable intent or a provider guarantee"
                    .into(),
            ));
        }
        Ok(self)
    }
}

impl<A, RA, R> Call<A, RA, R> {
    /// Validates the adapter and returns the callable handle, or the first
    /// violation named.
    pub fn new(adapter: Adapter<A, RA, R>) -> Result<Call<A, RA, R>, InvalidAdapter> {
        adapter.validated().map(Call::from_validated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::{EffectSlot, Resolution};

    type Test = Adapter<String, String, String>;
    type Mutate = Box<dyn Fn(&mut Test)>;

    fn base() -> Test {
        Adapter {
            operation: "test_effect".into(),
            key: None,
            send: send_fn(|_ctx, s: String| async move { Ok(s) }),
            classify: Some(Classifier::transport()),
            call_timeout: Duration::from_secs(1),
            irreversible: false,
            late_call: LateCall::Convergent,
            strategy: Strategy::Idempotent,
        }
    }

    fn key() -> KeyFn<String> {
        key_fn(|s: &String| format!("key/{s}"))
    }

    fn look_before_act() -> Strategy<String, String, String> {
        Strategy::LookBeforeAct {
            lookup: lookup_fn(|_ctx, _s: String| async { Ok(None) }),
        }
    }

    fn keyed_replay(window: Duration) -> Strategy<String, String, String> {
        Strategy::KeyedReplay {
            window,
            intent_age: intent_age_fn(|_s: &String| Duration::ZERO),
            provider_id: provider_id_fn(|r: &String| r.clone()),
            resolve: resolve_keyed_fn(|_ctx, id: String| async move { Ok(id) }),
        }
    }

    fn declared(slot_identity: SlotIdentity) -> Strategy<String, String, String> {
        Strategy::Declared {
            slot_identity,
            resolution: DeclaredResolution::Resolve(resolve_fn(
                |_ctx, _s: String, _slot: EffectSlot| async {
                    Ok(Resolution::Unknown {
                        detail: String::new(),
                    })
                },
            )),
        }
    }

    fn hold() -> Strategy<String, String, String> {
        Strategy::Declared {
            slot_identity: SlotIdentity::RowScoped,
            resolution: DeclaredResolution::Hold,
        }
    }

    /// The validation matrix, minus every rule the enums made
    /// unrepresentable (a missing send, strategy, late call, lookup,
    /// intent age, provider id, resolver or slot identity; a resolver next
    /// to Hold).
    #[test]
    fn new_names_the_first_violation() {
        let cases: Vec<(&str, Mutate, &str)> = vec![
            (
                "empty operation",
                Box::new(|a| a.operation = String::new()),
                "operation",
            ),
            (
                "operation format",
                Box::new(|a| a.operation = "CreateServer".into()),
                "snake_case",
            ),
            (
                "operation length",
                Box::new(|a| a.operation = "a".repeat(65)),
                "64",
            ),
            (
                "zero timeout",
                Box::new(|a| a.call_timeout = Duration::ZERO),
                "call_timeout",
            ),
            (
                "key scoped without key",
                Box::new(|a| a.late_call = LateCall::KeyScoped),
                "KeyScoped requires key",
            ),
            (
                "no classifier",
                Box::new(|a| a.classify = None),
                "classify is required",
            ),
            (
                "keyed replay without key",
                Box::new(|a| a.strategy = keyed_replay(Duration::from_secs(3600))),
                "KeyedReplay requires key",
            ),
            (
                "keyed replay zero window",
                Box::new(|a| {
                    a.key = Some(key());
                    a.strategy = keyed_replay(Duration::ZERO);
                }),
                "window",
            ),
            (
                "declared keyed without key",
                Box::new(|a| a.strategy = declared(SlotIdentity::Keyed)),
                "Keyed } requires key",
            ),
            (
                "hold with classifier",
                Box::new(|a| a.strategy = hold()),
                "classify = None",
            ),
            (
                "irreversible idempotent",
                Box::new(|a| a.irreversible = true),
                "irreversible requires",
            ),
            (
                "irreversible look before act",
                Box::new(|a| {
                    a.irreversible = true;
                    a.strategy = look_before_act();
                }),
                "irreversible requires",
            ),
        ];
        for (name, mutate, want) in cases {
            let mut a = base();
            mutate(&mut a);
            let err = match Call::new(a) {
                Ok(_) => panic!("{name}: new accepted an invalid adapter"),
                Err(e) => e,
            };
            assert_eq!(err.operation.is_empty(), name == "empty operation");
            assert!(
                err.reason.contains(want),
                "{name}: {err} does not name the violation {want:?}"
            );
        }
    }

    #[test]
    fn new_accepts_each_strategy() {
        let cases: Vec<(&str, Mutate)> = vec![
            ("idempotent", Box::new(|_| {})),
            (
                "look before act",
                Box::new(|a| a.strategy = look_before_act()),
            ),
            (
                "keyed replay irreversible",
                Box::new(|a| {
                    a.key = Some(key());
                    a.strategy = keyed_replay(Duration::from_secs(3600));
                    a.irreversible = true;
                    a.late_call = LateCall::KeyScoped;
                }),
            ),
            (
                "declared row scoped",
                Box::new(|a| {
                    a.strategy = declared(SlotIdentity::RowScoped);
                    a.irreversible = true;
                    a.late_call = LateCall::Compensated;
                }),
            ),
            (
                "declared keyed",
                Box::new(|a| {
                    a.key = Some(key());
                    a.strategy = declared(SlotIdentity::Keyed);
                }),
            ),
            (
                "declared hold",
                Box::new(|a| {
                    a.classify = None;
                    a.strategy = hold();
                }),
            ),
        ];
        for (name, mutate) in cases {
            let mut a = base();
            mutate(&mut a);
            assert!(Call::new(a).is_ok(), "{name}: new rejected a valid adapter");
        }
    }

    #[tokio::test]
    async fn resolve_by_lookup_maps_the_three_answers() {
        let found = resolve_by_lookup(lookup_fn(|_ctx, _s: String| async {
            Ok(Some("found-value".to_owned()))
        }));
        let slot = EffectSlot {
            operation: "x".into(),
            key: String::new(),
            declared_at: chrono::Utc::now(),
            detail: String::new(),
        };
        assert_eq!(
            found(Ctx::background(), "x".into(), slot.clone())
                .await
                .unwrap(),
            Resolution::Succeeded("found-value".to_owned())
        );

        let absent = resolve_by_lookup(lookup_fn(|_ctx, _s: String| async { Ok(None::<String>) }));
        assert_eq!(
            absent(Ctx::background(), "x".into(), slot.clone())
                .await
                .unwrap()
                .state(),
            crate::AttemptState::Superseded
        );

        let failed = resolve_by_lookup(lookup_fn(|_ctx, _s: String| async {
            Err::<Option<String>, BoxError>("cannot prove either way".into())
        }));
        assert!(
            failed(Ctx::background(), "x".into(), slot).await.is_err(),
            "a lookup failure must return an error, never a verdict"
        );
    }
}
