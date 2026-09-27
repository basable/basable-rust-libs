//! The probe suite: universal probes, one suite per strategy, one per
//! late-call policy. Each probe takes a FRESH harness from the factory and
//! answers with a pass, a loud skip, or the failure text.

use std::fmt;
use std::future::Future;

use basable_core::{BoxError, Ctx};
use basable_externaleffect::{
    Cancelled, DeclaredResolution, DispatchError, LateCall, Resolution, SlotIdentity, Strategy,
    TimedOut, TransportError, Verdict, definitive,
};
use chrono::Utc;

use crate::harness::{Harness, expired_owner, live_owner};

/// How one probe ended when it did not fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The evidence held.
    Passed,
    /// The probe could not run, with the evidence it leaves unaudited.
    Skipped(String),
}

/// One adapter's audit: every applicable probe and how it ended.
#[derive(Debug)]
pub struct ProbeReport {
    /// The adapter's operation.
    pub operation: String,
    /// The probes in the order they ran.
    pub results: Vec<(&'static str, Result<ProbeOutcome, String>)>,
}

impl ProbeReport {
    /// The failed probes with their failure text.
    pub fn failures(&self) -> Vec<(&'static str, &str)> {
        self.results
            .iter()
            .filter_map(|(name, r)| r.as_ref().err().map(|e| (*name, e.as_str())))
            .collect()
    }

    /// The skipped probes with the evidence they leave unaudited.
    pub fn skipped(&self) -> Vec<(&'static str, &str)> {
        self.results
            .iter()
            .filter_map(|(name, r)| match r {
                Ok(ProbeOutcome::Skipped(why)) => Some((*name, why.as_str())),
                _ => None,
            })
            .collect()
    }

    /// Whether every probe passed or skipped.
    pub fn is_ok(&self) -> bool {
        self.failures().is_empty()
    }
}

impl fmt::Display for ProbeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "effecttest audit of {:?}:", self.operation)?;
        for (name, r) in &self.results {
            match r {
                Ok(ProbeOutcome::Passed) => writeln!(f, "  ok      {name}")?,
                Ok(ProbeOutcome::Skipped(why)) => writeln!(f, "  SKIPPED {name}: {why}")?,
                Err(e) => writeln!(f, "  FAILED  {name}: {e}")?,
            }
        }
        Ok(())
    }
}

type ProbeResult = Result<ProbeOutcome, String>;

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(format!($($arg)+));
        }
    };
}

/// Runs the audit for one adapter, panicking with the full report when a
/// probe fails; skips are printed to stderr and returned in the report. The
/// factory builds a fresh harness for every probe.
pub async fn run<A, RA, R, F, Fut>(factory: F) -> ProbeReport
where
    A: Clone + Send + Sync + 'static,
    RA: Send + 'static,
    R: Send + 'static,
    F: Fn() -> Fut,
    Fut: Future<Output = Harness<A, RA, R>>,
{
    let report = try_run(factory).await;
    for (name, why) in report.skipped() {
        eprintln!("effecttest: {:?}: {name} SKIPPED: {why}", report.operation);
    }
    assert!(report.is_ok(), "{report}");
    report
}

/// [`run`] without the panic: the report, whatever it says. A harness that
/// lacks a required input still panics — that is a fixture bug, not a
/// finding.
pub async fn try_run<A, RA, R, F, Fut>(factory: F) -> ProbeReport
where
    A: Clone + Send + Sync + 'static,
    RA: Send + 'static,
    R: Send + 'static,
    F: Fn() -> Fut,
    Fut: Future<Output = Harness<A, RA, R>>,
{
    let shape = factory().await;
    require_harness(&shape);
    let operation = shape.call.operation().to_owned();
    let late_call = shape.call.late_call();
    let strategy = match shape.call.strategy() {
        Strategy::Idempotent => "idempotent",
        Strategy::LookBeforeAct { .. } => "look_before_act",
        Strategy::KeyedReplay { .. } => "keyed_replay",
        Strategy::Declared { .. } => "declared",
    };
    let holds = shape.call.strategy().holds();
    drop(shape);

    let mut results: Vec<(&'static str, ProbeResult)> = Vec::new();
    macro_rules! probe {
        ($name:literal, $f:ident) => {
            results.push(($name, $f(factory().await).await));
        };
    }

    probe!("identity_determinism", identity_determinism);
    probe!("ownership_fence", ownership_fence);
    probe!("classifier_floor", classifier_floor);
    probe!("definitive_honesty", definitive_honesty);

    match strategy {
        "idempotent" => {
            probe!("send_twice_lands_once", send_twice_lands_once);
            probe!(
                "ack_loss_then_retry_lands_once",
                ack_loss_then_retry_lands_once
            );
        }
        "look_before_act" => {
            probe!("lookup_before_send_is_absent", lookup_before_send_is_absent);
            probe!("send_then_lookup_finds", send_then_lookup_finds);
            probe!(
                "ack_loss_adopts_instead_of_resending",
                ack_loss_adopts_instead_of_resending
            );
        }
        "keyed_replay" => {
            probe!("same_key_lands_once", same_key_lands_once);
            probe!(
                "ack_loss_then_same_key_lands_once",
                ack_loss_then_same_key_lands_once
            );
            probe!(
                "resolve_keyed_reflects_landed",
                resolve_keyed_reflects_landed
            );
            probe!("resolve_keyed_unknown_errors", resolve_keyed_unknown_errors);
            probe!("past_window_never_sent", past_window_never_sent);
        }
        _ => {
            probe!("slot_shape", slot_shape);
            if holds {
                probe!(
                    "hold_resolves_unknown_without_io",
                    hold_resolves_unknown_without_io
                );
            } else {
                probe!("resolver_never_sends", resolver_never_sends);
                probe!(
                    "landed_never_judged_resend_safe",
                    landed_never_judged_resend_safe
                );
                probe!(
                    "ack_loss_never_judged_resend_safe",
                    ack_loss_never_judged_resend_safe
                );
            }
        }
    }

    match late_call {
        LateCall::KeyScoped => {
            probe!(
                "key_scoped_advance_changes_key",
                key_scoped_advance_changes_key
            );
        }
        LateCall::Convergent => {
            probe!(
                "convergent_late_call_lands_nothing_new",
                convergent_late_call_lands_nothing_new
            );
        }
        LateCall::Compensated => {
            probe!("compensation", compensation);
        }
    }

    ProbeReport { operation, results }
}

fn require_harness<A, RA, R>(h: &Harness<A, RA, R>) {
    match h.call.late_call() {
        LateCall::KeyScoped => assert!(
            h.advance_intent.is_some(),
            "effecttest: LateCall::KeyScoped requires Harness::advance_intent"
        ),
        LateCall::Convergent => assert!(
            h.advance_intent.is_some(),
            "effecttest: LateCall::Convergent requires Harness::advance_intent (return old unchanged when the effect has no intent scope)"
        ),
        LateCall::Compensated => assert!(
            h.compensation_probe.is_some(),
            "effecttest: LateCall::Compensated requires Harness::compensation_probe"
        ),
    }
    assert!(
        !(h.call.irreversible() && h.inject_ambiguity.is_none()),
        "effecttest: an irreversible adapter requires Harness::inject_ambiguity"
    );
    if matches!(h.call.strategy(), Strategy::KeyedReplay { .. }) {
        assert!(
            h.past_window.is_some(),
            "effecttest: a KeyedReplay adapter requires Harness::past_window — the replay window is a gate, and the audit probes it"
        );
    }
    if matches!(h.call.strategy(), Strategy::Declared { .. }) {
        assert!(
            h.resolve_args.is_some(),
            "effecttest: a Declared adapter whose A and RA differ requires Harness::resolve_args (it defaults to the identity only when they are one type)"
        );
    }
}

fn key_required<A, RA, R>(s: &Strategy<A, RA, R>) -> bool {
    match s {
        Strategy::KeyedReplay { .. } => true,
        Strategy::Declared { slot_identity, .. } => *slot_identity == SlotIdentity::Keyed,
        _ => false,
    }
}

fn resolve_args_of<A, RA, R>(h: &Harness<A, RA, R>, args: A) -> RA {
    (h.resolve_args
        .as_ref()
        .expect("require_harness checked resolve_args"))(args)
}

/// The harness for an ack-loss probe: a skip when the component supplied
/// no injection and the adapter is reversible; a failure for an
/// irreversible adapter without one (checked at the shape, so unreachable
/// here).
fn ack_loss<A, RA, R>(h: &Harness<A, RA, R>) -> Result<(), ProbeOutcome> {
    if h.inject_ambiguity.is_none() {
        return Err(ProbeOutcome::Skipped(
            "no inject_ambiguity provided; ack-loss safety NOT audited for this adapter".into(),
        ));
    }
    Ok(())
}

async fn inject_ambiguity<A, RA, R>(h: &Harness<A, RA, R>) {
    (h.inject_ambiguity
        .as_ref()
        .expect("ack_loss checked the injection"))()
    .await
}

fn bg() -> Ctx {
    Ctx::background()
}

// --- universal ---------------------------------------------------------

async fn identity_determinism<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let k1 = h.call.key_for(&(h.args)());
    let k2 = h.call.key_for(&(h.args)());
    ensure!(
        k1 == k2,
        "key_for is not deterministic for one intent: {k1:?} vs {k2:?}"
    );
    ensure!(
        !(key_required(h.call.strategy()) && k1.is_empty()),
        "adapter requires a key but key_for returned empty"
    );
    Ok(ProbeOutcome::Passed)
}

async fn ownership_fence<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult
where
    A: Clone,
{
    let args = (h.args)();
    let before = (h.landed_count)().await;
    let expired = expired_owner();
    let err = h.call.dispatch(&bg(), &expired, args.clone()).await.err();
    ensure!(
        matches!(err, Some(DispatchError::OwnershipLost)),
        "dispatch under an expired proof: got {err:?}, want OwnershipLost"
    );
    let after = (h.landed_count)().await;
    ensure!(
        after == before,
        "a fenced-out dispatch reached the wire: landed {before} -> {after}"
    );
    match h.call.strategy() {
        Strategy::LookBeforeAct { .. } => {
            let r = h.call.lookup(&bg(), &expired, args).await;
            ensure!(
                matches!(r, Err(basable_externaleffect::ResolveError::OwnershipLost)),
                "lookup under an expired proof: got {:?}, want OwnershipLost",
                r.as_ref().err()
            );
        }
        Strategy::KeyedReplay { .. } => {
            let r = h
                .call
                .resolve_keyed(&bg(), &expired, "effecttest-any".into())
                .await;
            ensure!(
                matches!(r, Err(basable_externaleffect::ResolveError::OwnershipLost)),
                "resolve_keyed under an expired proof: got {:?}, want OwnershipLost",
                r.as_ref().err()
            );
        }
        Strategy::Declared {
            resolution: DeclaredResolution::Resolve(_),
            ..
        } => {
            let slot = h.call.declare(&args, Utc::now());
            let r = h
                .call
                .resolve(&bg(), &expired, resolve_args_of(&h, args), &slot)
                .await;
            ensure!(
                matches!(r, Err(basable_externaleffect::ResolveError::OwnershipLost)),
                "resolve under an expired proof: got {:?}, want OwnershipLost",
                r.as_ref().err()
            );
        }
        _ => {}
    }
    Ok(ProbeOutcome::Passed)
}

async fn classifier_floor<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let transport: [(&str, BoxError); 3] = [
        (
            "timed-out",
            Box::new(TimedOut {
                after: std::time::Duration::from_secs(1),
            }),
        ),
        ("cancelled", Box::new(Cancelled)),
        (
            "transport",
            TransportError::wrap("effecttest: synthetic transport failure"),
        ),
    ];
    for (name, err) in &transport {
        ensure!(
            h.call.classify_error(err) == Verdict::Ambiguous,
            "classifier judged a {name} error definitive — a transport failure can never prove receiver state"
        );
    }
    if h.call.irreversible() {
        // The fail-closed floor: every shape a landed-but-damaged answer
        // takes must be HELD, never consumed as "the receiver decided". A
        // body cut mid-read is an I/O error and a gateway 502 is a status
        // error — both plain. Dispatch reaches a verdict only through the
        // classifier, so pinning it on these shapes pins the dispatch path a
        // real ack loss takes.
        let plain: [(&str, BoxError); 3] = [
            ("plain", "unrecognized failure".into()),
            (
                "truncated-body",
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "read response: unexpected end of file",
                )),
            ),
            (
                "http-5xx",
                "POST /order/server/transaction: http 502 Bad Gateway".into(),
            ),
        ];
        for (name, err) in &plain {
            ensure!(
                h.call.classify_error(err) == Verdict::Ambiguous,
                "irreversible adapter classified a {name} error definitive — only a provider's proof of non-execution may be"
            );
        }
        // ...and a provider-proven rejection IS consumed — except under
        // Hold, which classifies everything ambiguous by policy: with no
        // receiver postcondition to prove "never landed", a pre-flight
        // rejection held forever is the accepted cost (definitive_honesty
        // pins that side).
        if !h.call.strategy().holds() {
            ensure!(
                h.call.classify_error(&definitive("rejected")) == Verdict::Definitive,
                "irreversible adapter classified a provider-proven rejection ambiguous"
            );
        }
    }
    Ok(ProbeOutcome::Passed)
}

async fn definitive_honesty<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let args = (h.args)();
    (h.inject_definitive)().await;
    let err = match h.call.dispatch(&bg(), &live_owner(), args).await {
        Ok(_) => return Err("injected structured rejection produced no error".into()),
        Err(e) => e,
    };
    if h.call.strategy().holds() {
        ensure!(
            err.is_ambiguous(),
            "a Hold adapter classified an error definitive: {err}"
        );
    } else {
        ensure!(
            !err.is_ambiguous(),
            "structured rejection held as ambiguous instead of consumed: {err}"
        );
    }
    let landed = (h.landed_count)().await;
    ensure!(landed == 0, "a rejected send landed {landed} effects");
    Ok(ProbeOutcome::Passed)
}

// --- Idempotent --------------------------------------------------------

async fn send_twice_lands_once<A: Clone, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let args = (h.args)();
    for i in 1..=2 {
        if let Err(e) = h.call.dispatch(&bg(), &live_owner(), args.clone()).await {
            return Err(format!("dispatch #{i}: {e}"));
        }
        let landed = (h.landed_count)().await;
        ensure!(
            landed == 1,
            "after dispatch #{i}: landed {landed} effects, want 1"
        );
    }
    Ok(ProbeOutcome::Passed)
}

async fn ack_loss_then_retry_lands_once<A: Clone, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    if let Err(skip) = ack_loss(&h) {
        return Ok(skip);
    }
    let args = (h.args)();
    inject_ambiguity(&h).await;
    let err = h
        .call
        .dispatch(&bg(), &live_owner(), args.clone())
        .await
        .err();
    ensure!(
        err.as_ref().is_some_and(|e| e.is_ambiguous()),
        "armed ack loss did not surface ambiguous: {err:?}"
    );
    if let Err(e) = h.call.dispatch(&bg(), &live_owner(), args).await {
        return Err(format!("retry after ack loss: {e}"));
    }
    let landed = (h.landed_count)().await;
    ensure!(landed == 1, "ack loss double-landed: {landed} effects");
    Ok(ProbeOutcome::Passed)
}

// --- LookBeforeAct -----------------------------------------------------

async fn lookup_before_send_is_absent<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    match h.call.lookup(&bg(), &live_owner(), (h.args)()).await {
        Err(e) => Err(format!("lookup: {e}")),
        Ok(Some(_)) => Err(
            "lookup found an effect nothing sent — a false receipt would suppress a required send"
                .into(),
        ),
        Ok(None) => Ok(ProbeOutcome::Passed),
    }
}

async fn send_then_lookup_finds<A: Clone, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let args = (h.args)();
    if let Err(e) = h.call.dispatch(&bg(), &live_owner(), args.clone()).await {
        return Err(format!("dispatch: {e}"));
    }
    let landed = (h.landed_count)().await;
    ensure!(landed == 1, "landed {landed} effects, want 1");
    match h.call.lookup(&bg(), &live_owner(), args).await {
        Err(e) => Err(format!("lookup after send: {e}")),
        Ok(None) => Err("lookup missed the landed effect — the gate's premise is false".into()),
        Ok(Some(_)) => Ok(ProbeOutcome::Passed),
    }
}

async fn ack_loss_adopts_instead_of_resending<A: Clone, RA, R>(
    h: Harness<A, RA, R>,
) -> ProbeResult {
    if let Err(skip) = ack_loss(&h) {
        return Ok(skip);
    }
    let args = (h.args)();
    inject_ambiguity(&h).await;
    let err = h
        .call
        .dispatch(&bg(), &live_owner(), args.clone())
        .await
        .err();
    ensure!(
        err.as_ref().is_some_and(|e| e.is_ambiguous()),
        "armed ack loss did not surface ambiguous: {err:?}"
    );
    match h.call.lookup(&bg(), &live_owner(), args).await {
        Err(e) => return Err(format!("lookup after ack loss: {e}")),
        Ok(None) => {
            return Err(
                "ack-lost effect invisible to lookup — the gate would permit a second send".into(),
            );
        }
        Ok(Some(_)) => {}
    }
    let landed = (h.landed_count)().await;
    ensure!(landed == 1, "landed {landed} effects, want 1");
    Ok(ProbeOutcome::Passed)
}

// --- KeyedReplay -------------------------------------------------------

async fn same_key_lands_once<A: Clone, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let args = (h.args)();
    let first = match h.call.dispatch(&bg(), &live_owner(), args.clone()).await {
        Ok(r) => r,
        Err(e) => return Err(format!("dispatch #1: {e}")),
    };
    let replay = match h.call.dispatch(&bg(), &live_owner(), args).await {
        Ok(r) => r,
        Err(e) => return Err(format!("dispatch #2 (same key): {e}")),
    };
    let landed = (h.landed_count)().await;
    ensure!(landed == 1, "same key landed {landed} effects, want 1");
    let (a, b) = (h.call.provider_id(&first), h.call.provider_id(&replay));
    ensure!(
        a == b,
        "same key returned distinct provider identities: {a:?} vs {b:?}"
    );
    Ok(ProbeOutcome::Passed)
}

async fn ack_loss_then_same_key_lands_once<A: Clone, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    if let Err(skip) = ack_loss(&h) {
        return Ok(skip);
    }
    let args = (h.args)();
    inject_ambiguity(&h).await;
    let err = h
        .call
        .dispatch(&bg(), &live_owner(), args.clone())
        .await
        .err();
    ensure!(
        err.as_ref().is_some_and(|e| e.is_ambiguous()),
        "armed ack loss did not surface ambiguous: {err:?}"
    );
    if let Err(e) = h.call.dispatch(&bg(), &live_owner(), args).await {
        return Err(format!("same-key replay after ack loss: {e}"));
    }
    let landed = (h.landed_count)().await;
    ensure!(
        landed == 1,
        "ack loss + same-key replay landed {landed} effects, want 1"
    );
    Ok(ProbeOutcome::Passed)
}

async fn resolve_keyed_reflects_landed<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let res = match h.call.dispatch(&bg(), &live_owner(), (h.args)()).await {
        Ok(r) => r,
        Err(e) => return Err(format!("dispatch: {e}")),
    };
    let id = h.call.provider_id(&res);
    if let Err(e) = h.call.resolve_keyed(&bg(), &live_owner(), id).await {
        return Err(format!("resolve_keyed on the landed identity: {e}"));
    }
    Ok(ProbeOutcome::Passed)
}

async fn resolve_keyed_unknown_errors<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    match h
        .call
        .resolve_keyed(&bg(), &live_owner(), "effecttest-does-not-exist".into())
        .await
    {
        Ok(_) => Err(
            "resolve_keyed on an unknown identity returned no error — unknown must never read as settled"
                .into(),
        ),
        Err(_) => Ok(ProbeOutcome::Passed),
    }
}

async fn past_window_never_sent<A: Clone, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    // The window gate: the provider may have pruned the key by now, so a
    // same-key "replay" could mint a second live effect. Dispatch must
    // refuse before the wire; the caller's only paths are resolve_keyed by
    // the persisted id or its own settlement.
    let args = (h.args)();
    let aged = (h.past_window.as_ref().expect("require_harness"))(args.clone());
    let (a, b) = (h.call.key_for(&args), h.call.key_for(&aged));
    ensure!(
        a == b,
        "past_window changed the key ({a:?} -> {b:?}): it must age the SAME intent, not mint a new one"
    );
    let before = (h.landed_count)().await;
    let err = h.call.dispatch(&bg(), &live_owner(), aged).await.err();
    ensure!(
        matches!(err, Some(DispatchError::ReplayWindowElapsed { .. })),
        "a same-key dispatch past the replay window was not refused: {err:?}"
    );
    let after = (h.landed_count)().await;
    ensure!(
        after == before,
        "a past-window dispatch reached the wire: landed {before} -> {after}"
    );
    Ok(ProbeOutcome::Passed)
}

// --- Declared ----------------------------------------------------------

async fn slot_shape<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let args = (h.args)();
    let slot = h.call.declare(&args, Utc::now());
    ensure!(
        slot.operation == h.call.operation(),
        "slot operation {:?}, want {:?}",
        slot.operation,
        h.call.operation()
    );
    match h.call.strategy() {
        Strategy::Declared {
            slot_identity: SlotIdentity::Keyed,
            ..
        } => ensure!(
            !slot.key.is_empty() && slot.key == h.call.key_for(&args),
            "Keyed slot key {:?}, want the derived key {:?}",
            slot.key,
            h.call.key_for(&args)
        ),
        Strategy::Declared {
            slot_identity: SlotIdentity::RowScoped,
            ..
        } => ensure!(
            slot.key.is_empty(),
            "RowScoped slot carries a key {:?} — its status table has no column for one",
            slot.key
        ),
        _ => unreachable!("the declared suite runs on Declared adapters only"),
    }
    Ok(ProbeOutcome::Passed)
}

async fn hold_resolves_unknown_without_io<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let args = (h.args)();
    let before = (h.landed_count)().await;
    let mut slot = h.call.declare(&args, Utc::now());
    slot.detail = "held ambiguity".into();
    let res = match h
        .call
        .resolve(&bg(), &live_owner(), resolve_args_of(&h, args), &slot)
        .await
    {
        Ok(r) => r,
        Err(e) => return Err(format!("resolve on a Hold adapter: {e}")),
    };
    ensure!(
        matches!(&res, Resolution::Unknown { detail } if detail == "held ambiguity"),
        "Hold resolved {}/{:?}, want unknown with the slot's detail",
        res.state(),
        res.detail()
    );
    let after = (h.landed_count)().await;
    ensure!(
        after == before,
        "Hold resolve performed I/O: landed {before} -> {after}"
    );
    Ok(ProbeOutcome::Passed)
}

async fn resolver_never_sends<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let args = (h.args)();
    let slot = h.call.declare(&args, Utc::now());
    let before = (h.landed_count)().await;
    let res = h
        .call
        .resolve(&bg(), &live_owner(), resolve_args_of(&h, args), &slot)
        .await;
    ensure!(
        !matches!(res, Ok(Resolution::Succeeded(_))),
        "resolver credited a never-sent effect as succeeded"
    );
    let after = (h.landed_count)().await;
    ensure!(
        after == before,
        "resolver dispatched the effect: landed {before} -> {after}"
    );
    Ok(ProbeOutcome::Passed)
}

async fn landed_never_judged_resend_safe<A: Clone, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let args = (h.args)();
    let slot = h.call.declare(&args, Utc::now());
    if let Err(e) = h.call.dispatch(&bg(), &live_owner(), args.clone()).await {
        return Err(format!("dispatch: {e}"));
    }
    assert_landed_resolution(&h, resolve_args_of(&h, args), &slot, "clean send").await
}

async fn ack_loss_never_judged_resend_safe<A: Clone, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    if let Err(skip) = ack_loss(&h) {
        return Ok(skip);
    }
    let args = (h.args)();
    let slot = h.call.declare(&args, Utc::now());
    inject_ambiguity(&h).await;
    let err = h
        .call
        .dispatch(&bg(), &live_owner(), args.clone())
        .await
        .err();
    ensure!(
        err.as_ref().is_some_and(|e| e.is_ambiguous()),
        "armed ack loss did not surface ambiguous: {err:?}"
    );
    assert_landed_resolution(&h, resolve_args_of(&h, args), &slot, "ack-lost send").await
}

/// The core irreversibility audit: after a send that LANDED, the resolver
/// may report succeeded (it found the receipt) or unknown (a grace-based
/// resolver still waiting). Superseded — the verdict that authorizes a
/// (re)send — is fatal for an irreversible adapter, and tolerated for a
/// reversible one whose declared policy is exactly re-send-safe (a reset
/// whose duplicate costs one reboot, presumed lost past its grace).
/// Tolerated is not endorsed: a reversible RECEIVER may still refuse the
/// re-send, so a resolver that can read the receipt must, and the component
/// pins that in its own tests. Failed after a landed send is a lie for
/// every adapter.
async fn assert_landed_resolution<A, RA, R>(
    h: &Harness<A, RA, R>,
    rargs: RA,
    slot: &basable_externaleffect::EffectSlot,
    mode: &str,
) -> ProbeResult {
    let res = match h.call.resolve(&bg(), &live_owner(), rargs, slot).await {
        Ok(r) => r,
        // In production "could not prove either way" keeps the slot, which
        // is safe — but the fixture is healthy and the effect just landed,
        // so a resolver that cannot observe it here can never adopt it in
        // production either: the slot would hold forever.
        Err(e) => return Err(format!("resolver errored on a landed effect ({mode}): {e}")),
    };
    match res {
        Resolution::Succeeded(_) | Resolution::Unknown { .. } => Ok(ProbeOutcome::Passed),
        Resolution::Superseded { .. } => {
            ensure!(
                !h.call.irreversible(),
                "resolver judged a landed IRREVERSIBLE effect ({mode}) safe to re-send"
            );
            Ok(ProbeOutcome::Passed)
        }
        Resolution::Failed { detail } => Err(format!(
            "resolver judged a landed effect ({mode}) failed: {detail}"
        )),
    }
}

// --- late call ---------------------------------------------------------

async fn key_scoped_advance_changes_key<A, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let old = (h.args)();
    let old_key = h.call.key_for(&old);
    let advanced = (h.advance_intent.as_ref().expect("require_harness"))(old);
    ensure!(
        old_key != h.call.key_for(&advanced),
        "LateCall::KeyScoped: an advanced intent scope produced the same key"
    );
    Ok(ProbeOutcome::Passed)
}

/// Clause 3 for a Convergent adapter: a stale attempt's dispatch after a
/// newer intent landed creates NOTHING new at the receiver — it is a no-op
/// or a re-assertion of what is already there, never a second resource, a
/// second release, a second charge. The probe pins exactly that, by landed
/// count; it does NOT pin the re-assertion's content (which payload the
/// receiver holds afterwards is the level-triggered reconciler's job — its
/// next pass re-sends the current intent).
async fn convergent_late_call_lands_nothing_new<A: Clone, RA, R>(
    h: Harness<A, RA, R>,
) -> ProbeResult {
    let stale = (h.args)();
    let newer = (h.advance_intent.as_ref().expect("require_harness"))(stale.clone());
    if let Err(e) = h.call.dispatch(&bg(), &live_owner(), stale.clone()).await {
        return Err(format!("dispatch (the intent that will go stale): {e}"));
    }
    // A receiver may refuse a duplicate it already holds (an armed rescue)
    // — a refusal is not a new effect; a definitive rejection of the newer
    // intent is a fixture bug.
    if let Err(e) = h.call.dispatch(&bg(), &live_owner(), newer).await
        && !e.is_ambiguous()
    {
        return Err(format!("dispatch (the newer intent): {e}"));
    }
    let settled = (h.landed_count)().await;
    ensure!(
        settled != 0,
        "nothing landed before the late call — the probe would prove nothing"
    );
    // The late call: refused or converged, never counted.
    let _ = h.call.dispatch(&bg(), &live_owner(), stale).await;
    let after = (h.landed_count)().await;
    ensure!(
        after == settled,
        "a late dispatch of a stale intent landed a new effect: {settled} -> {after}"
    );
    Ok(ProbeOutcome::Passed)
}

async fn compensation<A: Clone, RA, R>(h: Harness<A, RA, R>) -> ProbeResult {
    let args = (h.args)();
    // Produce residue: a landed effect nothing consumed — through an ack
    // loss when the harness can inject one, a clean send otherwise.
    if h.inject_ambiguity.is_some() {
        inject_ambiguity(&h).await;
        let err = h.call.dispatch(&bg(), &live_owner(), args).await.err();
        ensure!(
            err.as_ref().is_some_and(|e| e.is_ambiguous()),
            "armed ack loss did not surface ambiguous: {err:?}"
        );
    } else if let Err(e) = h.call.dispatch(&bg(), &live_owner(), args).await {
        return Err(format!("dispatch: {e}"));
    }
    (h.compensation_probe.as_ref().expect("require_harness"))()
        .await
        .map(|()| ProbeOutcome::Passed)
}
