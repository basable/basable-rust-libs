//! What a component supplies to audit ONE call against its own simulator
//! or fake, and the owner doubles the probes dispatch under.

use std::any::{Any, TypeId};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use basable_core::Deadline;
use basable_externaleffect::{BoxFuture, Call, Owner};

type Probe<T> = Box<dyn Fn() -> BoxFuture<'static, T> + Send + Sync>;
type Derive<A, B> = Box<dyn Fn(A) -> B + Send + Sync>;

/// The audit's inputs for one adapter. [`run`](crate::run) rebuilds it per
/// probe through the factory, so state never leaks between probes. Build it
/// with [`Harness::new`] and the optional setters.
pub struct Harness<A, RA, R> {
    /// The SAME validated handle production wires — never a test-only twin;
    /// auditing a twin audits nothing.
    pub call: Arc<Call<A, RA, R>>,
    pub(crate) args: Box<dyn Fn() -> A + Send + Sync>,
    pub(crate) advance_intent: Option<Derive<A, A>>,
    pub(crate) landed_count: Probe<usize>,
    pub(crate) past_window: Option<Derive<A, A>>,
    pub(crate) inject_ambiguity: Option<Probe<()>>,
    pub(crate) inject_definitive: Probe<()>,
    pub(crate) compensation_probe: Option<Probe<Result<(), String>>>,
    pub(crate) resolve_args: Option<Derive<A, RA>>,
}

impl<A, RA, R> Harness<A, RA, R>
where
    A: 'static,
    RA: 'static,
{
    /// The required inputs.
    ///
    /// `args` returns the canonical args for one committed intent against
    /// the harness's fixture; two calls within one harness must represent
    /// the SAME intent — the suite derives keys from both to probe
    /// determinism. `landed_count` is the audit's ground truth: how many
    /// real side effects of this operation the RECEIVER now holds — the
    /// simulator, or a fake that models the receiver's own store. It must
    /// observe that store, never the component's own bookkeeping.
    /// `inject_definitive` arms exactly one structured rejection that lands
    /// nothing (a simulator fault).
    pub fn new<LF, LFut, DF, DFut>(
        call: Arc<Call<A, RA, R>>,
        args: impl Fn() -> A + Send + Sync + 'static,
        landed_count: LF,
        inject_definitive: DF,
    ) -> Harness<A, RA, R>
    where
        LF: Fn() -> LFut + Send + Sync + 'static,
        LFut: Future<Output = usize> + Send + 'static,
        DF: Fn() -> DFut + Send + Sync + 'static,
        DFut: Future<Output = ()> + Send + 'static,
    {
        let resolve_args: Option<Derive<A, RA>> = if TypeId::of::<A>() == TypeId::of::<RA>() {
            Some(Box::new(|args: A| {
                *(Box::new(args) as Box<dyn Any>)
                    .downcast::<RA>()
                    .expect("A and RA are one type")
            }))
        } else {
            None
        };
        Harness {
            call,
            args: Box::new(args),
            advance_intent: None,
            landed_count: Box::new(move || Box::pin(landed_count())),
            past_window: None,
            inject_ambiguity: None,
            inject_definitive: Box::new(move || Box::pin(inject_definitive())),
            compensation_probe: None,
            resolve_args,
        }
    }

    /// Args for a strictly newer intent scope (next generation, rotated
    /// member vector, next charge; for a `Convergent` effect a newer
    /// payload against the same receiver identity — a rotated credential,
    /// a different schematic). Required iff the adapter declares
    /// `LateCall::KeyScoped` or `LateCall::Convergent`. A `Convergent`
    /// effect whose args carry no intent scope at all (a hardware reset, a
    /// detach: the only possible late call is a duplicate) returns `old`
    /// unchanged.
    pub fn advance_intent(mut self, f: impl Fn(A) -> A + Send + Sync + 'static) -> Self {
        self.advance_intent = Some(Box::new(f));
        self
    }

    /// The SAME intent as a later attempt would see it once the provider's
    /// replay window has elapsed — the key unchanged, the intent's age
    /// advanced past the window. Required iff the strategy is
    /// `KeyedReplay`.
    pub fn past_window(mut self, f: impl Fn(A) -> A + Send + Sync + 'static) -> Self {
        self.past_window = Some(Box::new(f));
        self
    }

    /// Arms exactly one ack loss: the next send's request reaches the
    /// simulator and LANDS, but the caller sees a transport error. Arm an
    /// [`AckLossProxy`](crate::AckLossProxy) in front of an HTTP provider,
    /// or implement it directly on an in-process fake. Optional for
    /// reversible adapters (the ack-loss probes then skip loudly, naming
    /// the missing evidence); REQUIRED for irreversible ones.
    pub fn inject_ambiguity<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.inject_ambiguity = Some(Box::new(move || Box::pin(f())));
        self
    }

    /// Asserts, against residue the suite just produced (a landed but
    /// unconsumed effect), that the component's compensation collects it;
    /// `Err` is the failure. Two honest shapes: run the compensator itself
    /// where it needs no database — the production adapter's own resolver
    /// or the production sweep adapter dispatched over the residue — or
    /// else pin the exact mechanism the compensator acts through (the
    /// identity the residue is discoverable and deletable by, and by
    /// nothing else), with the compensator's trigger logic left to the
    /// integration suites. A pinned mechanism must be one the fake
    /// ENFORCES, otherwise the probe is satisfiable by calling the fake's
    /// own delete and proves nothing. Required iff `LateCall::Compensated`.
    pub fn compensation_probe<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.compensation_probe = Some(Box::new(move || Box::pin(f())));
        self
    }

    /// Maps one intent's dispatch args to the resolve identity a later
    /// attempt would hold — what the declared probes pass to
    /// `Call::resolve`. Required for a `Declared` adapter whose `A` and
    /// `RA` differ; when they are one type it defaults to the identity.
    pub fn resolve_args(mut self, f: impl Fn(A) -> RA + Send + Sync + 'static) -> Self {
        self.resolve_args = Some(Box::new(f));
        self
    }
}

/// An [`Owner`] test double with a fixed deadline.
#[derive(Debug, Clone, Copy)]
pub struct FixedOwner(pub Deadline);

impl Owner for FixedOwner {
    fn ownership_deadline(&self) -> Option<Deadline> {
        Some(self.0)
    }
}

/// An owner whose proof runs another hour.
pub fn live_owner() -> FixedOwner {
    FixedOwner(Deadline::after(Duration::from_secs(60 * 60)))
}

/// An owner whose deadline has already passed — the clause-6 fence probe.
pub fn expired_owner() -> FixedOwner {
    FixedOwner(Deadline::after(Duration::ZERO))
}
