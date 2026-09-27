//! The callable surface of a validated adapter: small helpers the
//! reconciler invokes in sequence, each fencing exactly one step. The
//! reconciler keeps the control flow — which step runs, in which phase, and
//! what its verdict means for status, remain the component's explicit code.
//! Strategy-mismatched calls panic: an adapter wired to the wrong helper is
//! a construction bug, not data.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use basable_core::{BoxError, Ctx, Deadline};
use chrono::{DateTime, Utc};

use crate::adapter::{Adapter, BoxFuture, DeclaredResolution, LateCall, SlotIdentity, Strategy};
use crate::owner::{Owner, OwnershipLost, check_ownership};
use crate::slot::{EffectSlot, Resolution};
use crate::verdict::{Cancelled, TimedOut, Verdict};

/// The tracing target the verdict events go to: one event per dispatch,
/// resolution and lookup, with `operation` and the verdict as fields, so a
/// provider that starts answering ambiguously shows up as a rising series
/// before anyone reads a log.
pub const METRICS_TARGET: &str = "basable_externaleffect::metrics";

/// A validated adapter — the only handle that can dispatch or resolve.
/// [`Call::new`] is the only constructor.
pub struct Call<A, RA, R> {
    a: Adapter<A, RA, R>,
}

/// Why a dispatch did not return a result.
#[derive(Debug)]
pub enum DispatchError {
    /// Nothing was sent; a plain retry is the whole story.
    OwnershipLost,
    /// `KeyedReplay` only: nothing was sent; the intent is at or past the
    /// provider's replay window, so the caller resolves by its persisted
    /// provider id or holds — never re-sends.
    ReplayWindowElapsed {
        /// The effect.
        operation: String,
        /// The intent's age.
        age: Duration,
        /// The provider's replay window.
        window: Duration,
    },
    /// The effect MAY have landed — keep the marker / replay the same key;
    /// NEVER re-send blind. `Display` is the provider's text verbatim.
    Ambiguous(BoxError),
    /// The receiver's definitive answer, verbatim; nothing landed, consume
    /// the failure.
    Definitive(BoxError),
}

impl DispatchError {
    /// The "keep the marker / replay the same key" branch of every dispatch
    /// match.
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, DispatchError::Ambiguous(_))
    }

    /// The provider's error, for an ambiguous or definitive verdict.
    pub fn provider_error(&self) -> Option<&BoxError> {
        match self {
            DispatchError::Ambiguous(e) | DispatchError::Definitive(e) => Some(e),
            _ => None,
        }
    }
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispatchError::OwnershipLost => fmt::Display::fmt(&OwnershipLost, f),
            DispatchError::ReplayWindowElapsed {
                operation,
                age,
                window,
            } => write!(
                f,
                "replay window elapsed before dispatch: effect {operation:?}: intent is {age:?} old, replay window is {window:?}"
            ),
            DispatchError::Ambiguous(e) | DispatchError::Definitive(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl Error for DispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            DispatchError::Ambiguous(e) | DispatchError::Definitive(e) => Some(&**e),
            _ => None,
        }
    }
}

impl From<OwnershipLost> for DispatchError {
    fn from(_: OwnershipLost) -> DispatchError {
        DispatchError::OwnershipLost
    }
}

/// Why an observation (a lookup, a slot resolution, a resolve by provider
/// id) did not answer.
#[derive(Debug)]
pub enum ResolveError {
    /// Nothing was observed; a plain retry is the whole story.
    OwnershipLost,
    /// The observation failed: "could not prove either way" — fail closed,
    /// never send. `Display` is the provider's text verbatim.
    Failed(BoxError),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::OwnershipLost => fmt::Display::fmt(&OwnershipLost, f),
            ResolveError::Failed(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl Error for ResolveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ResolveError::Failed(e) => Some(&**e),
            ResolveError::OwnershipLost => None,
        }
    }
}

impl From<OwnershipLost> for ResolveError {
    fn from(_: OwnershipLost) -> ResolveError {
        ResolveError::OwnershipLost
    }
}

impl<A, RA, R> Call<A, RA, R> {
    pub(crate) fn from_validated(a: Adapter<A, RA, R>) -> Call<A, RA, R> {
        Call { a }
    }

    /// Mints the slot for one dispatch: the operation stamped, the key
    /// derived and stamped for `SlotIdentity::Keyed`, `declared_at = now`.
    /// `now` MUST be the database clock (the component's database `now()`).
    /// The reconciler then places the slot on its WORKING status and writes
    /// it through the claim's `write_status` itself — the write, its error
    /// handling (`Fenced` ⇒ never send; any other error ⇒ never send,
    /// retry), and the one-working-status-value rule stay exactly where the
    /// processing-object contract puts them. `Declared`-only; panics
    /// otherwise.
    pub fn declare(&self, args: &A, now: DateTime<Utc>) -> EffectSlot {
        let slot_identity = match &self.a.strategy {
            Strategy::Declared { slot_identity, .. } => *slot_identity,
            other => self.wrong_strategy("declare", other),
        };
        let key = match slot_identity {
            SlotIdentity::Keyed => self.key_for(args),
            SlotIdentity::RowScoped => String::new(),
        };
        EffectSlot {
            operation: self.a.operation.clone(),
            key,
            declared_at: now,
            detail: String::new(),
        }
    }

    /// The fenced send: [`check_ownership`], a context bounded by the
    /// smaller of `call_timeout` and the remaining ownership deadline, the
    /// send, then classification. For `Declared { Hold }` adapters every
    /// send error returns ambiguous.
    pub async fn dispatch(
        &self,
        ctx: &Ctx,
        proof: &dyn Owner,
        args: A,
    ) -> Result<R, DispatchError> {
        let deadline = check_ownership(proof)?;
        if let Strategy::KeyedReplay {
            window, intent_age, ..
        } = &self.a.strategy
        {
            let age = intent_age(&args);
            if age >= *window {
                return Err(DispatchError::ReplayWindowElapsed {
                    operation: self.a.operation.clone(),
                    age,
                    window: *window,
                });
            }
        }
        let op = self.a.operation.as_str();
        match self
            .bounded(ctx, deadline, |c| (self.a.send)(c, args))
            .await
        {
            Ok(res) => {
                tracing::debug!(target: METRICS_TARGET, operation = op, verdict = "sent", "effect attempt");
                Ok(res)
            }
            Err(err) => {
                let ambiguous = match &self.a.classify {
                    None => true,
                    Some(c) => c.judge(&err) == Verdict::Ambiguous,
                };
                if ambiguous {
                    tracing::debug!(target: METRICS_TARGET, operation = op, verdict = "ambiguous", "effect attempt");
                    Err(DispatchError::Ambiguous(err))
                } else {
                    tracing::debug!(target: METRICS_TARGET, operation = op, verdict = "failed", "effect attempt");
                    Err(DispatchError::Definitive(err))
                }
            }
        }
    }

    /// Settles a `Declared` slot found on claim — it NEVER sends this
    /// effect. A `Hold` adapter answers `Unknown` with zero I/O (the slot's
    /// detail carried through); otherwise the strategy's resolver runs
    /// under the ownership fence and the bounded context. `Declared`-only;
    /// panics otherwise.
    pub async fn resolve(
        &self,
        ctx: &Ctx,
        proof: &dyn Owner,
        args: RA,
        slot: &EffectSlot,
    ) -> Result<Resolution<R>, ResolveError> {
        let resolver = match &self.a.strategy {
            Strategy::Declared {
                resolution: DeclaredResolution::Hold,
                ..
            } => {
                return Ok(Resolution::Unknown {
                    detail: slot.detail.clone(),
                });
            }
            Strategy::Declared {
                resolution: DeclaredResolution::Resolve(r),
                ..
            } => r,
            other => self.wrong_strategy("resolve", other),
        };
        let deadline = check_ownership(proof)?;
        let slot = slot.clone();
        let res = self
            .bounded(ctx, deadline, |c| resolver(c, args, slot))
            .await;
        let state = match &res {
            Ok(r) => r.state().as_str(),
            Err(_) => "error",
        };
        tracing::debug!(target: METRICS_TARGET, operation = self.a.operation.as_str(), state, "effect resolution");
        res.map_err(ResolveError::Failed)
    }

    /// Runs the `LookBeforeAct` observation under the ownership fence:
    /// `Some` — adopt it; `None` — verified absence, dispatch is permitted;
    /// an error — fail closed, never send. `LookBeforeAct`-only; panics
    /// otherwise.
    pub async fn lookup(
        &self,
        ctx: &Ctx,
        proof: &dyn Owner,
        args: A,
    ) -> Result<Option<R>, ResolveError> {
        let lookup = match &self.a.strategy {
            Strategy::LookBeforeAct { lookup } => lookup,
            other => self.wrong_strategy("lookup", other),
        };
        let deadline = check_ownership(proof)?;
        let res = self.bounded(ctx, deadline, |c| lookup(c, args)).await;
        let outcome = match &res {
            Ok(Some(_)) => "found",
            Ok(None) => "absent",
            Err(_) => "error",
        };
        tracing::debug!(target: METRICS_TARGET, operation = self.a.operation.as_str(), outcome, "effect lookup");
        res.map_err(ResolveError::Failed)
    }

    /// Reads the authoritative outcome by the PERSISTED provider identity,
    /// under the ownership fence — the resolve-don't-resend path an effect
    /// must take forever once its provider id is durable.
    /// `KeyedReplay`-only; panics otherwise.
    pub async fn resolve_keyed(
        &self,
        ctx: &Ctx,
        proof: &dyn Owner,
        provider_id: String,
    ) -> Result<R, ResolveError> {
        let resolve = match &self.a.strategy {
            Strategy::KeyedReplay { resolve, .. } => resolve,
            other => self.wrong_strategy("resolve_keyed", other),
        };
        let deadline = check_ownership(proof)?;
        let res = self
            .bounded(ctx, deadline, |c| resolve(c, provider_id))
            .await;
        let state = if res.is_ok() { "succeeded" } else { "error" };
        tracing::debug!(target: METRICS_TARGET, operation = self.a.operation.as_str(), state, "effect resolution");
        res.map_err(ResolveError::Failed)
    }

    /// Extracts the receiver identity the caller must persist the moment
    /// it is known. `KeyedReplay`-only; panics otherwise.
    pub fn provider_id(&self, res: &R) -> String {
        match &self.a.strategy {
            Strategy::KeyedReplay { provider_id, .. } => provider_id(res),
            other => self.wrong_strategy("provider_id", other),
        }
    }

    /// Names the effect — for logging, phase pinning, and slot routing.
    pub fn operation(&self) -> &str {
        &self.a.operation
    }

    /// The deterministic identity for `args`, or empty when the adapter
    /// declares no key.
    pub fn key_for(&self, args: &A) -> String {
        self.a.key.as_ref().map(|k| k(args)).unwrap_or_default()
    }

    /// The validated clause-2 evidence — effecttest matches on it to pick
    /// the probe suite.
    pub fn strategy(&self) -> &Strategy<A, RA, R> {
        &self.a.strategy
    }

    /// The clause-5 marking.
    pub fn irreversible(&self) -> bool {
        self.a.irreversible
    }

    /// The clause-3 evidence.
    pub fn late_call(&self) -> LateCall {
        self.a.late_call
    }

    /// The adapter's classification of a hypothetical send error —
    /// effecttest probes the classifier floor through it. A `Hold` adapter
    /// classifies everything ambiguous.
    pub fn classify_error(&self, err: &BoxError) -> Verdict {
        match &self.a.classify {
            None => Verdict::Ambiguous,
            Some(c) => c.judge(err),
        }
    }

    /// Runs one remote step under the bound: `call_timeout`, tightened to
    /// the remaining ownership deadline, so no call outlives the lease that
    /// fenced it (clause 6); and the caller's context, whose cancellation
    /// drops the step. Both produce the transport-shaped markers
    /// ([`TimedOut`], [`Cancelled`]) the classifier floor holds. The
    /// context handed to the step carries the bound as its deadline.
    async fn bounded<T>(
        &self,
        ctx: &Ctx,
        deadline: Deadline,
        step: impl FnOnce(Ctx) -> BoxFuture<'static, Result<T, BoxError>>,
    ) -> Result<T, BoxError> {
        let budget = self.a.call_timeout.min(deadline.remaining());
        let fut = step(ctx.with_timeout(budget));
        tokio::select! {
            biased;
            _ = ctx.cancelled() => Err(Box::new(Cancelled) as BoxError),
            out = tokio::time::timeout(budget, fut) => match out {
                Ok(out) => out,
                Err(_) => Err(Box::new(TimedOut { after: budget }) as BoxError),
            },
        }
    }

    fn wrong_strategy(&self, helper: &str, strategy: &Strategy<A, RA, R>) -> ! {
        panic!(
            "externaleffect: effect {:?}: {helper} on a {} adapter",
            self.a.operation,
            strategy.name()
        )
    }
}

impl<A, RA, R> fmt::Debug for Call<A, RA, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call")
            .field("operation", &self.a.operation)
            .field("strategy", &self.a.strategy)
            .field("late_call", &self.a.late_call)
            .field("irreversible", &self.a.irreversible)
            .field("call_timeout", &self.a.call_timeout)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::adapter::{
        intent_age_fn, key_fn, provider_id_fn, resolve_fn, resolve_keyed_fn, send_fn,
    };
    use crate::owner::Unfenced;
    use crate::verdict::{Classifier, TransportError, classify_transport, is_transport};

    #[derive(Debug, Clone, Copy)]
    struct Proof(Deadline);

    impl Owner for Proof {
        fn ownership_deadline(&self) -> Option<Deadline> {
            Some(self.0)
        }
    }

    fn live() -> Proof {
        Proof(Deadline::after(Duration::from_secs(3600)))
    }

    fn expired() -> Proof {
        Proof(Deadline::after(Duration::ZERO))
    }

    fn idempotent(
        send: crate::adapter::SendFn<String, String>,
        classify: Classifier,
    ) -> Call<String, String, String> {
        Call::new(Adapter {
            operation: "probe".into(),
            key: None,
            send,
            classify: Some(classify),
            call_timeout: Duration::from_secs(1),
            irreversible: false,
            late_call: LateCall::Convergent,
            strategy: Strategy::Idempotent,
        })
        .unwrap()
    }

    fn echo() -> Call<String, String, String> {
        idempotent(
            send_fn(|_ctx, s: String| async move { Ok(s) }),
            Classifier::transport(),
        )
    }

    #[test]
    fn the_transport_floor() {
        let ambiguous: [BoxError; 4] = [
            Box::new(TimedOut {
                after: Duration::from_secs(1),
            }),
            Box::new(Cancelled),
            TransportError::wrap("connection reset by simulator"),
            Box::new(std::io::Error::other("broken pipe")),
        ];
        for err in &ambiguous {
            assert_eq!(classify_transport(err), Verdict::Ambiguous, "{err}");
            assert!(is_transport(&**err));
        }
        let wrapped: BoxError = crate::verdict::definitive(TransportError::wrap("deep"));
        assert!(
            is_transport(&*wrapped),
            "a transport failure is found anywhere in the chain"
        );
        let plain: BoxError = "structured rejection".into();
        assert_eq!(classify_transport(&plain), Verdict::Definitive);
    }

    #[test]
    fn the_ownership_fence() {
        assert_eq!(check_ownership(&expired()), Err(OwnershipLost));
        assert!(check_ownership(&live()).is_ok());
        assert!(check_ownership(&Unfenced).is_ok());
        struct Fenced;
        impl Owner for Fenced {
            fn ownership_deadline(&self) -> Option<Deadline> {
                None
            }
        }
        assert_eq!(check_ownership(&Fenced), Err(OwnershipLost));
    }

    #[tokio::test]
    async fn dispatch_fences_before_send() {
        let sent = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&sent);
        let call = idempotent(
            send_fn(move |_ctx, s: String| {
                flag.store(true, Ordering::SeqCst);
                async move { Ok(s) }
            }),
            Classifier::transport(),
        );
        let err = call
            .dispatch(&Ctx::background(), &expired(), "x".into())
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::OwnershipLost), "{err}");
        assert!(
            !sent.load(Ordering::SeqCst),
            "a fenced-out dispatch reached send"
        );
    }

    #[tokio::test]
    async fn dispatch_classifies_verbatim_and_keeps_the_source() {
        #[derive(Debug)]
        struct Declined;
        impl fmt::Display for Declined {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("payment declined")
            }
        }
        impl Error for Declined {}

        let mode = Arc::new(AtomicUsize::new(0));
        let chosen = Arc::clone(&mode);
        let call = idempotent(
            send_fn(move |_ctx, _s: String| {
                let mode = chosen.load(Ordering::SeqCst);
                async move {
                    match mode {
                        0 => Err(TransportError::wrap("connection reset by simulator")),
                        _ => Err(Box::new(Declined) as BoxError),
                    }
                }
            }),
            Classifier::new(|err| {
                if err.is::<Declined>() {
                    return Verdict::Definitive;
                }
                classify_transport(err)
            }),
        );

        let err = call
            .dispatch(&Ctx::background(), &live(), "x".into())
            .await
            .unwrap_err();
        assert!(err.is_ambiguous(), "{err}");
        assert_eq!(err.to_string(), "connection reset by simulator", "verbatim");
        assert!(
            err.source().is_some_and(|s| s.is::<TransportError>()),
            "the source chain keeps the provider error"
        );

        mode.store(1, Ordering::SeqCst);
        let err = call
            .dispatch(&Ctx::background(), &live(), "x".into())
            .await
            .unwrap_err();
        assert!(!err.is_ambiguous(), "{err}");
        assert_eq!(err.to_string(), "payment declined");
        assert!(err.provider_error().is_some_and(|e| e.is::<Declined>()));
    }

    #[tokio::test]
    async fn hold_treats_everything_ambiguous() {
        let call: Call<String, String, String> = Call::new(Adapter {
            operation: "hold_probe".into(),
            key: None,
            send: send_fn(|_ctx, _s: String| async {
                Err::<String, BoxError>("structured rejection".into())
            }),
            classify: None,
            call_timeout: Duration::from_secs(1),
            irreversible: false,
            late_call: LateCall::Convergent,
            strategy: Strategy::Declared {
                slot_identity: SlotIdentity::RowScoped,
                resolution: DeclaredResolution::Hold,
            },
        })
        .unwrap();
        let err = call
            .dispatch(&Ctx::background(), &live(), "x".into())
            .await
            .unwrap_err();
        assert!(
            err.is_ambiguous(),
            "a Hold adapter surfaced a definitive error: {err}"
        );
        assert_eq!(call.classify_error(&"anything".into()), Verdict::Ambiguous);
        let mut slot = call.declare(&"x".into(), Utc::now());
        slot.detail = "why".into();
        let res = call
            .resolve(&Ctx::background(), &live(), "x".into(), &slot)
            .await
            .unwrap();
        assert_eq!(
            res,
            Resolution::Unknown {
                detail: "why".into()
            },
            "unknown, carrying the slot detail"
        );
    }

    #[tokio::test]
    async fn dispatch_bounds_the_step() {
        // The lease bounds the call when it is sooner than call_timeout.
        let seen = Arc::new(std::sync::Mutex::new(None::<Duration>));
        let sink = Arc::clone(&seen);
        let call = idempotent(
            send_fn(move |ctx, s: String| {
                *sink.lock().unwrap() = ctx.deadline().map(|d| d.remaining());
                async move { Ok(s) }
            }),
            Classifier::transport(),
        );
        let lease = Proof(Deadline::after(Duration::from_millis(200)));
        call.dispatch(&Ctx::background(), &lease, "x".into())
            .await
            .unwrap();
        let remaining = seen.lock().unwrap().expect("the send saw a deadline");
        assert!(
            remaining <= Duration::from_millis(200),
            "the step outlives the lease: {remaining:?}"
        );

        // call_timeout bounds it otherwise, and a send that never answers
        // times out as an ambiguous TimedOut.
        let hung: Call<String, String, String> = Call::new(Adapter {
            operation: "hung".into(),
            key: None,
            send: send_fn(|_ctx, _s: String| std::future::pending::<Result<String, BoxError>>()),
            classify: Some(Classifier::transport()),
            call_timeout: Duration::from_millis(30),
            irreversible: false,
            late_call: LateCall::Convergent,
            strategy: Strategy::Idempotent,
        })
        .unwrap();
        let err = hung
            .dispatch(&Ctx::background(), &live(), "x".into())
            .await
            .unwrap_err();
        assert!(err.is_ambiguous(), "{err}");
        assert!(err.provider_error().is_some_and(|e| e.is::<TimedOut>()));

        // A cancelled context drops the step: ambiguous, nothing decided.
        let ctx = Ctx::background().child();
        ctx.cancel();
        let err = hung.dispatch(&ctx, &live(), "x".into()).await.unwrap_err();
        assert!(
            err.provider_error().is_some_and(|e| e.is::<Cancelled>()),
            "{err}"
        );
    }

    fn keyed(window: Duration) -> (Call<String, String, String>, Arc<AtomicUsize>) {
        let sent = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&sent);
        let call = Call::new(Adapter {
            operation: "keyed".into(),
            key: Some(key_fn(|s: &String| format!("key/{s}"))),
            send: send_fn(move |_ctx, s: String| {
                counter.fetch_add(1, Ordering::SeqCst);
                async move { Ok(s) }
            }),
            classify: Some(Classifier::transport()),
            call_timeout: Duration::from_secs(1),
            irreversible: false,
            late_call: LateCall::KeyScoped,
            strategy: Strategy::KeyedReplay {
                window,
                intent_age: intent_age_fn(|s: &String| {
                    if s == "aged" {
                        Duration::from_secs(3600)
                    } else {
                        Duration::from_secs(60)
                    }
                }),
                provider_id: provider_id_fn(|r: &String| r.clone()),
                resolve: resolve_keyed_fn(|_ctx, id: String| async move { Ok(id) }),
            },
        })
        .unwrap();
        (call, sent)
    }

    /// The window gate: a keyed dispatch whose intent is at or past the
    /// provider's replay window is refused before send runs — the provider
    /// may have pruned the key, and a "replay" would then be a second live
    /// effect.
    #[tokio::test]
    async fn dispatch_refuses_past_the_replay_window() {
        let (call, sent) = keyed(Duration::from_secs(3600));
        call.dispatch(&Ctx::background(), &Unfenced, "fresh".into())
            .await
            .unwrap();
        assert_eq!(sent.load(Ordering::SeqCst), 1);
        let err = call
            .dispatch(&Ctx::background(), &Unfenced, "aged".into())
            .await
            .unwrap_err();
        assert!(
            matches!(err, DispatchError::ReplayWindowElapsed { .. }),
            "an intent at the window edge was not refused: {err}"
        );
        assert_eq!(
            sent.load(Ordering::SeqCst),
            1,
            "a past-window dispatch reached send"
        );
        assert_eq!(call.provider_id(&"id".to_owned()), "id");
        assert_eq!(
            call.resolve_keyed(&Ctx::background(), &Unfenced, "id".into())
                .await
                .unwrap(),
            "id"
        );
    }

    #[tokio::test]
    async fn declare_stamps_the_slot_and_refuses_the_wrong_strategy() {
        let keyed: Call<String, String, String> = Call::new(Adapter {
            operation: "declare_probe".into(),
            key: Some(key_fn(|s: &String| format!("k/{s}"))),
            send: send_fn(|_ctx, s: String| async move { Ok(s) }),
            classify: Some(Classifier::transport()),
            call_timeout: Duration::from_secs(1),
            irreversible: false,
            late_call: LateCall::KeyScoped,
            strategy: Strategy::Declared {
                slot_identity: SlotIdentity::Keyed,
                resolution: DeclaredResolution::Resolve(resolve_fn(
                    |_ctx, _s: String, _slot: EffectSlot| async {
                        Ok(Resolution::Unknown {
                            detail: String::new(),
                        })
                    },
                )),
            },
        })
        .unwrap();
        let now = Utc::now();
        let slot = keyed.declare(&"x".into(), now);
        assert_eq!(
            slot,
            EffectSlot {
                operation: "declare_probe".into(),
                key: "k/x".into(),
                declared_at: now,
                detail: String::new(),
            }
        );

        let row_scoped: Call<String, String, String> = Call::new(Adapter {
            operation: "declare_probe_row".into(),
            key: None,
            send: send_fn(|_ctx, s: String| async move { Ok(s) }),
            classify: Some(Classifier::transport()),
            call_timeout: Duration::from_secs(1),
            irreversible: false,
            late_call: LateCall::Convergent,
            strategy: Strategy::Declared {
                slot_identity: SlotIdentity::RowScoped,
                resolution: DeclaredResolution::Resolve(resolve_fn(
                    |_ctx, _s: String, _slot: EffectSlot| async {
                        Ok(Resolution::Unknown {
                            detail: String::new(),
                        })
                    },
                )),
            },
        })
        .unwrap();
        assert!(row_scoped.declare(&"x".into(), now).key.is_empty());

        // Strategy-mismatched helpers panic: wrong wiring is a construction
        // bug, not data.
        let idem = Arc::new(echo());
        let sync_panics = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            idem.declare(&"x".into(), now)
        }));
        assert!(sync_panics.is_err(), "declare on Idempotent did not panic");
        let sync_panics = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            idem.provider_id(&"x".into())
        }));
        assert!(
            sync_panics.is_err(),
            "provider_id on Idempotent did not panic"
        );
        for (name, fut) in [
            ("resolve", {
                let c = Arc::clone(&idem);
                tokio::spawn(async move {
                    let slot = EffectSlot {
                        operation: "probe".into(),
                        key: String::new(),
                        declared_at: now,
                        detail: String::new(),
                    };
                    let _ = c
                        .resolve(&Ctx::background(), &Unfenced, "x".into(), &slot)
                        .await;
                })
            }),
            ("lookup", {
                let c = Arc::clone(&idem);
                tokio::spawn(async move {
                    let _ = c.lookup(&Ctx::background(), &Unfenced, "x".into()).await;
                })
            }),
            ("resolve_keyed", {
                let c = Arc::clone(&idem);
                tokio::spawn(async move {
                    let _ = c
                        .resolve_keyed(&Ctx::background(), &Unfenced, "id".into())
                        .await;
                })
            }),
        ] {
            let err = fut.await.unwrap_err();
            assert!(err.is_panic(), "{name} on Idempotent did not panic");
        }
    }
}
