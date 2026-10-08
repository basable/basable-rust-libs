//! The attempt runtime: the loop that turns due envelope rows into
//! reconcile attempts. One [`Worker`] drives one processing object type on
//! one replica — claim a batch, run each claim through the type's
//! [`Reconciler`] under a heartbeated lease, complete with the fenced
//! transaction, then hand the committed outcome to the [`AfterComplete`]
//! callback.
//!
//! Scheduling is poll-first: the `poll_interval` scan is the correctness
//! path, and the store's in-process wake only shortens latency. A running
//! worker registers its wake on the store it was built with: a write
//! through that store (or a clone of it) that makes an object due rescans
//! at once, and a completion that leaves its object due within one poll
//! interval rescans when it is due. A write anywhere else — another
//! replica, a separately bound store — is found by the poll.
//!
//! Shutdown is cancellation of the context `run` was given: claiming stops,
//! in-flight attempts see their contexts cancelled and complete as retries
//! on a detached bounded timeout, and `run` returns once every attempt has
//! completed. Anything that cannot complete in time is abandoned to the
//! single recovery path — lease expiry and adoption.

use std::any::Any;
use std::future::{Future, poll_fn};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use basable_core::{BoxError, Ctx};
use tokio::sync::Notify;
use tokio::task::{JoinError, JoinSet};

use crate::claim::{Claim, LEASE_SLACK};
use crate::complete::Completion;
use crate::decl::{Adapter, WorkerConfig};
use crate::error::Error;
use crate::model::Object;
use crate::outcome::Outcome;
use crate::store::TypedStore;

/// Bounds the fenced completion transaction. It runs detached from the
/// attempt: a timed-out or cancelled attempt must still complete as a
/// `Retry`.
pub const COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);

/// Invariant 6's loud-by-age threshold: claiming a deleting object whose
/// `deleted_at` is older than this logs an error on every pass. Teardown
/// may legitimately wait across many passes, so retry counts prove nothing —
/// age is the only honest wedge signal.
const DELETING_ALARM_AGE: Duration = Duration::from_secs(30 * 60);

/// The domain logic for one processing object type: one level-triggered
/// pass over one claimed object. It reads `claim.object` (the claim-time
/// snapshot), may write status mid-attempt through the claim, and may
/// consult [`Claim::ownership_deadline`] before remote effects; completion
/// and heartbeats belong to the worker. Remote I/O is allowed here — and
/// only here (invariant 7): no framework transaction or lock is held while
/// a pass runs.
///
/// The attempt context carries the attempt deadline and is cancelled on
/// shutdown or when a heartbeat is fenced. The worker enforces both by
/// dropping the pass's future, so a reconciler need not poll the context;
/// one that holds an external resource across the deadline should.
pub trait Reconciler<S, T, A: Adapter<S, T>>: Send + Sync + 'static {
    /// One pass. `Err` is the attempt's failure and resolves to `Retry`.
    fn reconcile(
        &self,
        ctx: &Ctx,
        claim: &mut Claim<S, T, A>,
    ) -> impl Future<Output = Result<Outcome<T>, BoxError>> + Send;
}

/// Observes a committed completion: emit events, notify other components
/// (invariant 8). It runs on a bounded, detached context, never for
/// [`Completion::Unknown`], and a panic in it is contained — the completion
/// already committed, so there is nothing to fail.
pub trait AfterComplete<S, T>: Send + Sync + 'static {
    /// The callback. `object` is the claim-time snapshot as the reconciler
    /// left it.
    fn after_complete(
        &self,
        ctx: &Ctx,
        object: Object<S, T>,
        completion: Completion<T>,
    ) -> impl Future<Output = ()> + Send;
}

/// The absent callback.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAfterComplete;

impl<S, T> AfterComplete<S, T> for NoAfterComplete
where
    S: Send + Sync + 'static,
    T: Send + Sync + 'static,
{
    async fn after_complete(&self, _ctx: &Ctx, _object: Object<S, T>, _done: Completion<T>) {}
}

impl<S, T, F, Fut> AfterComplete<S, T> for F
where
    S: Send + Sync + 'static,
    T: Send + Sync + 'static,
    F: Fn(&Ctx, Object<S, T>, Completion<T>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    fn after_complete(
        &self,
        ctx: &Ctx,
        object: Object<S, T>,
        completion: Completion<T>,
    ) -> impl Future<Output = ()> + Send {
        self(ctx, object, completion)
    }
}

/// Drives one processing object type on this replica, woken by writes
/// through the store it was built with.
pub struct Worker<S, T, A: Adapter<S, T>, R, F> {
    inner: Arc<Inner<S, T, A, R, F>>,
}

struct Inner<S, T, A: Adapter<S, T>, R, F> {
    store: TypedStore<S, T, A>,
    cfg: WorkerConfig,
    rec: R,
    after: F,
}

impl<S, T, A, R, F> Worker<S, T, A, R, F>
where
    S: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    A: Adapter<S, T>,
    R: Reconciler<S, T, A>,
    F: AfterComplete<S, T>,
{
    /// Binds a typed store, its worker policy, and the type's reconciler.
    /// `after` is [`NoAfterComplete`] when nothing observes completions.
    /// Give it a clone of the store the type's writers use: that sharing is
    /// what lets their writes wake it.
    pub fn new(
        store: TypedStore<S, T, A>,
        cfg: WorkerConfig,
        rec: R,
        after: F,
    ) -> Result<Worker<S, T, A, R, F>, Error> {
        let cfg = cfg.validated()?;
        Ok(Worker {
            inner: Arc::new(Inner {
                store,
                cfg,
                rec,
                after,
            }),
        })
    }

    /// The processing object type this worker drives.
    pub fn type_name(&self) -> &'static str {
        self.inner.store.name()
    }

    /// Runs the loop, scanning for due work every `poll_interval` (or sooner
    /// on a wake), until `ctx` is cancelled — then drains in-flight attempts
    /// and returns. The worker's wake is registered on its store for the
    /// whole call.
    pub async fn run(self, ctx: Ctx) {
        let inner = self.inner;
        let name = inner.store.name();
        tracing::info!(
            processing_object_type = name,
            parallelism = inner.cfg.parallelism,
            batch_size = inner.cfg.batch_size,
            poll_interval_ms = inner.cfg.poll_interval.as_millis() as u64,
            attempt_timeout_ms = inner.cfg.attempt_timeout.as_millis() as u64,
            resync_ms = inner.cfg.resync.as_millis() as u64,
            max_attempts = inner.cfg.max_attempts,
            "processing object worker starting"
        );

        // The wake, registered on the store before the first scan, so a
        // write that commits after that scan began is never missed: a
        // stored permit, so wakes coalesce like Go's one-slot channel — many
        // signals while a scan runs mean one more scan.
        let registration = inner.store.inner.wakes.register();
        let wake = registration.notify();

        let mut tasks: JoinSet<()> = JoinSet::new();
        let mut ticker = tokio::time::interval(inner.cfg.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // the first tick is immediate; the loop scans first anyway
        loop {
            scan_once(&inner, &ctx, &mut tasks, wake).await;
            tokio::select! {
                _ = ctx.cancelled() => break,
                _ = ticker.tick() => {}
                _ = wake.notified() => {}
            }
        }

        while let Some(joined) = tasks.join_next().await {
            log_join(name, joined);
        }
        drop(registration);
        tracing::info!(
            processing_object_type = name,
            "processing object worker drained"
        );
    }
}

/// Claims up to the currently free parallelism and spawns one attempt per
/// claim. Claiming more than can start immediately would burn lease time in
/// a local queue, so the batch is capped by free slots; each attempt
/// self-wakes the loop when its slot frees, so a due backlog drains at
/// completion speed rather than one batch per poll interval.
async fn scan_once<S, T, A, R, F>(
    inner: &Arc<Inner<S, T, A, R, F>>,
    ctx: &Ctx,
    tasks: &mut JoinSet<()>,
    wake: &Arc<Notify>,
) where
    S: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    A: Adapter<S, T>,
    R: Reconciler<S, T, A>,
    F: AfterComplete<S, T>,
{
    if ctx.is_done() {
        return;
    }
    let name = inner.store.name();
    while let Some(joined) = tasks.try_join_next() {
        log_join(name, joined);
    }
    let in_flight = u32::try_from(tasks.len()).unwrap_or(u32::MAX);
    let free = inner.cfg.parallelism.saturating_sub(in_flight);
    if free == 0 {
        return;
    }
    let mut cfg = inner.cfg.clone();
    cfg.batch_size = cfg.batch_size.min(free);
    let claims = match inner.store.claim_batch(cfg).await {
        Ok(claims) => claims,
        Err(e) => {
            if !ctx.is_done() {
                tracing::warn!(
                    processing_object_type = name,
                    error = %e,
                    "processing object claim scan failed"
                );
            }
            return;
        }
    };
    for claim in claims {
        let inner = Arc::clone(inner);
        let ctx = ctx.clone();
        let wake = Arc::clone(wake);
        tasks.spawn(async move {
            attempt(&inner, &ctx, claim).await;
            // Capacity freed: rescan immediately. This is the local signal
            // that drains a due backlog at completion speed instead of one
            // batch per poll interval — the store's wake only says "an
            // object became due", never "a slot opened here", so it goes to
            // this worker alone.
            wake.notify_one();
        });
    }
}

/// An attempt task is shielded end to end; a panic escaping it anyway is a
/// runtime bug worth a loud log, never a dead worker.
fn log_join(name: &str, joined: Result<(), JoinError>) {
    if let Err(e) = joined
        && e.is_panic()
    {
        tracing::error!(
            processing_object_type = name,
            panic = panic_message(&*e.into_panic()),
            "processing object attempt task panicked outside the guarded sections"
        );
    }
}

/// Runs one claim to completion: heartbeat pump, panic-safe reconcile,
/// fenced completion on a detached timeout, then `after_complete` for an
/// identified committed outcome.
async fn attempt<S, T, A, R, F>(
    inner: &Arc<Inner<S, T, A, R, F>>,
    run_ctx: &Ctx,
    mut claim: Claim<S, T, A>,
) where
    S: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    A: Adapter<S, T>,
    R: Reconciler<S, T, A>,
    F: AfterComplete<S, T>,
{
    let start = Instant::now();
    let name = inner.store.name();
    let id = claim.object.id;
    if let Some(deleted_at) = claim.object.meta.deleted_at {
        let age = chrono::Utc::now()
            .signed_duration_since(deleted_at)
            .to_std()
            .unwrap_or_default();
        if age > DELETING_ALARM_AGE {
            tracing::error!(
                processing_object_type = name,
                id = %id,
                deleted_at = %deleted_at,
                age_secs = age.as_secs(),
                invariant = 6,
                "processing object teardown has been running for a long time"
            );
        }
    }

    let attempt_ctx = run_ctx.with_timeout(inner.cfg.attempt_timeout);

    // Heartbeat pump: extends lease and proof while the reconciler runs. It
    // exits when the attempt context ends — cancelled once the reconciler
    // returns, so a finished attempt issues no further heartbeats — and a
    // fenced heartbeat cancels the attempt: the work is already forfeit, so
    // stop paying for it. Transient errors just wait for the next tick; the
    // proof countdown self-limits a real outage.
    let pump = {
        let handle = claim.lease_handle();
        let tick = (inner.cfg.attempt_timeout + LEASE_SLACK) / 3;
        let pump_ctx = attempt_ctx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = pump_ctx.cancelled() => return,
                    _ = tokio::time::sleep(tick) => {}
                }
                if pump_ctx.is_done() {
                    return;
                }
                if let Err(Error::Fenced) = handle.heartbeat().await {
                    pump_ctx.cancel();
                    return;
                }
            }
        })
    };

    let out = reconcile_safely(inner, &attempt_ctx, &mut claim).await;
    // Stop the pump and JOIN it before completing: no heartbeat may run
    // concurrently with (or outlive) the completion, and once the attempt
    // returns, no task of it may still touch the pool. Abort rather than
    // wait — a heartbeat stalled on a dead connection would otherwise hold
    // the completion for the rest of the attempt budget, burning the lease
    // slack the completion needs.
    attempt_ctx.cancel();
    pump.abort();
    let _ = pump.await;

    // Completion must survive attempt cancellation and shutdown: it takes no
    // context and is bounded on its own.
    let object = claim.object.clone();
    let completion = match tokio::time::timeout(COMPLETION_TIMEOUT, claim.complete(out)).await {
        Ok(Ok(done)) => done,
        Ok(Err(Error::Fenced)) => {
            tracing::debug!(
                processing_object_type = name,
                id = %id,
                duration_ms = start.elapsed().as_millis() as u64,
                "processing object attempt fenced — a successor owns the object"
            );
            return;
        }
        Ok(Err(e)) => {
            // Nothing committed; the object stays claimed until its lease
            // expires and a successor adopts — the single recovery path.
            tracing::warn!(
                processing_object_type = name,
                id = %id,
                error = %e,
                "processing object completion failed — lease expiry will recover"
            );
            return;
        }
        Err(_) => {
            tracing::warn!(
                processing_object_type = name,
                id = %id,
                timeout_ms = COMPLETION_TIMEOUT.as_millis() as u64,
                "processing object completion timed out — lease expiry will recover"
            );
            return;
        }
    };
    let Some(outcome) = completion.outcome() else {
        tracing::warn!(
            processing_object_type = name,
            id = %id,
            duration_ms = start.elapsed().as_millis() as u64,
            "processing object completion adopted after ambiguous commit — outcome unknown, callbacks suppressed"
        );
        return;
    };
    tracing::debug!(
        processing_object_type = name,
        id = %id,
        outcome = outcome.name(),
        duration_ms = start.elapsed().as_millis() as u64,
        superseded = completion.superseded(),
        woken = completion.woken(),
        cause = outcome.cause().map(|c| c.to_string()).unwrap_or_default(),
        "processing object attempt completed"
    );

    // The callback runs detached from the run context — a shutdown must not
    // truncate the event for a completion that already committed — under
    // its own bound, and a panic in it loses nothing but the event.
    let after_ctx = Ctx::new(run_ctx.request_id()).with_timeout(inner.cfg.after_complete_timeout);
    let guarded = catch_unwind_async(inner.after.after_complete(&after_ctx, object, completion));
    match tokio::time::timeout(inner.cfg.after_complete_timeout, guarded).await {
        Ok(Ok(())) => {}
        Ok(Err(panic)) => tracing::error!(
            processing_object_type = name,
            id = %id,
            panic = panic,
            "processing object after_complete panicked"
        ),
        Err(_) => tracing::warn!(
            processing_object_type = name,
            id = %id,
            "processing object after_complete timed out"
        ),
    }
}

/// Runs the reconciler under the attempt deadline and cancellation, with
/// panic recovery: a panicking pass completes as a loud `Retry` instead of
/// killing the replica's whole worker. The pass's future is dropped at the
/// deadline or on cancellation, so a stuck reconciler cannot hold the claim
/// past its budget.
async fn reconcile_safely<S, T, A, R, F>(
    inner: &Inner<S, T, A, R, F>,
    ctx: &Ctx,
    claim: &mut Claim<S, T, A>,
) -> Result<Outcome<T>, BoxError>
where
    S: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    A: Adapter<S, T>,
    R: Reconciler<S, T, A>,
    F: AfterComplete<S, T>,
{
    let budget = ctx
        .deadline()
        .map(|d| d.remaining())
        .unwrap_or(inner.cfg.attempt_timeout);
    let guarded = catch_unwind_async(inner.rec.reconcile(ctx, claim));
    tokio::select! {
        out = guarded => match out {
            Ok(out) => out,
            Err(panic) => Err(format!("reconciler panic: {panic}").into()),
        },
        _ = tokio::time::sleep(budget) => Err("attempt timed out".into()),
        _ = ctx.cancelled() => Err("attempt cancelled".into()),
    }
}

/// Polls `fut` to completion with each poll under `catch_unwind`; a panic
/// becomes `Err` with its message. The future is pinned on the heap so no
/// projection is needed. Requires the default `unwind` panic strategy — with
/// `panic = "abort"` a panic ends the process before anything here runs.
async fn catch_unwind_async<Fut: Future>(fut: Fut) -> Result<Fut::Output, String> {
    let mut fut: Pin<Box<Fut>> = Box::pin(fut);
    poll_fn(
        |cx| match catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(out)) => Poll::Ready(Ok(out)),
            Err(payload) => Poll::Ready(Err(panic_message(&*payload))),
        },
    )
    .await
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_caught_panic_carries_its_message() {
        let literal = catch_unwind_async(async {
            if std::hint::black_box(true) {
                panic!("boom");
            }
            1
        })
        .await;
        assert_eq!(literal.unwrap_err(), "boom", "a literal payload is a &str");

        let formatted = catch_unwind_async(async {
            let n = std::hint::black_box(7);
            if n > 0 {
                panic!("boom {n}");
            }
            n
        })
        .await;
        assert_eq!(
            formatted.unwrap_err(),
            "boom 7",
            "a formatted payload is a String"
        );

        let clean = catch_unwind_async(async { 3 }).await;
        assert_eq!(clean.unwrap(), 3);
    }
}
