//! The running half of the harness: the default conformance reconciler
//! over the simulator with its injection hooks, the gate that holds a pass
//! open, and [`Replica`] — a worker running in the background that
//! [`Replica::stop`] cancels and drains.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use basable_core::{BoxError, Ctx};
use basable_processingobject::{
    Adapter, AfterComplete, Error, Outcome, Reconciler, TypedStore, Worker, WorkerConfig,
};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::conformance::{ConformanceAdapter, Spec, Status, TYPE_NAME};
use crate::harness::ConformanceClaim;
use crate::widgetsim::{WidgetSim, canonical_key};

/// A future a hook returns: it borrows the attempt context and the claim.
pub type HookFuture<'a> = Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + 'a>>;

/// An injection point in [`ExampleReconciler`]: runs before or after the
/// provider call with the attempt context and the claim. An `Err` fails
/// the pass. Build one from a closure with [`hook`], or use a [`Gate`].
pub trait Hook: Send + Sync + 'static {
    /// Runs the hook.
    fn call<'a>(&'a self, ctx: &'a Ctx, claim: &'a mut ConformanceClaim) -> HookFuture<'a>;
}

/// A [`Hook`] from a closure: `hook(|ctx, claim| Box::pin(async move { … }))`.
pub fn hook<F>(f: F) -> impl Hook
where
    F: for<'a> Fn(&'a Ctx, &'a mut ConformanceClaim) -> HookFuture<'a> + Send + Sync + 'static,
{
    FnHook(f)
}

struct FnHook<F>(F);

impl<F> Hook for FnHook<F>
where
    F: for<'a> Fn(&'a Ctx, &'a mut ConformanceClaim) -> HookFuture<'a> + Send + Sync + 'static,
{
    fn call<'a>(&'a self, ctx: &'a Ctx, claim: &'a mut ConformanceClaim) -> HookFuture<'a> {
        (self.0)(ctx, claim)
    }
}

/// The default conformance reconciler: it upserts the object into the
/// simulator (idempotently), returns `Converged` with a status derived from
/// the spec, and on a deleting claim deletes the provider resource and
/// returns `Delete`. The hooks are optional gate/injection points —
/// scenarios build capacity, crash, and race seams by blocking inside them.
pub struct ExampleReconciler {
    sim: Arc<WidgetSim>,
    before: Option<Box<dyn Hook>>,
    after_call: Option<Box<dyn Hook>>,
}

impl ExampleReconciler {
    /// A reconciler over `sim` with no hooks.
    pub fn new(sim: Arc<WidgetSim>) -> ExampleReconciler {
        ExampleReconciler {
            sim,
            before: None,
            after_call: None,
        }
    }

    /// Runs `h` at the start of every pass, before the provider call.
    pub fn before(mut self, h: impl Hook) -> ExampleReconciler {
        self.before = Some(Box::new(h));
        self
    }

    /// Runs `h` after a successful upsert, before the status is derived.
    pub fn after_call(mut self, h: impl Hook) -> ExampleReconciler {
        self.after_call = Some(Box::new(h));
        self
    }
}

impl Reconciler<Spec, Status, ConformanceAdapter> for ExampleReconciler {
    async fn reconcile(
        &self,
        ctx: &Ctx,
        claim: &mut ConformanceClaim,
    ) -> Result<Outcome<Status>, BoxError> {
        if let Some(h) = &self.before {
            h.call(ctx, claim).await?;
        }
        let key = canonical_key(TYPE_NAME, claim.object.id);
        if claim.object.meta.deleted_at.is_some() {
            self.sim.delete(&key).await?;
            return Ok(Outcome::delete());
        }
        self.sim.upsert(&key, &claim.object.spec).await?;
        if let Some(h) = &self.after_call {
            h.call(ctx, claim).await?;
        }
        Ok(Outcome::converged(Some(Status {
            provisioned_widgets: claim.object.spec.widgets,
            external_id: key,
        })))
    }
}

/// Blocks each pass that reaches it until the test releases it, signalling
/// entry on the way in. It is the substrate for capacity, heartbeat and
/// two-replica scenarios that must hold an attempt open while asserting
/// other state. Clones share the gate; pass one to
/// [`ExampleReconciler::before`] or `after_call`.
#[derive(Clone)]
pub struct Gate {
    entered: Arc<Notify>,
    release: watch::Sender<bool>,
}

impl Default for Gate {
    fn default() -> Gate {
        Gate::new()
    }
}

impl Gate {
    /// An unreleased gate.
    pub fn new() -> Gate {
        Gate {
            entered: Arc::new(Notify::new()),
            release: watch::Sender::new(false),
        }
    }

    /// Resolves once at least one pass has reached the gate. Entries
    /// coalesce: two passes at the gate satisfy one wait.
    pub async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    /// Unblocks every pass waiting at the gate, and every later one.
    /// Idempotent.
    pub fn release(&self) {
        self.release.send_replace(true);
    }
}

impl Hook for Gate {
    fn call<'a>(&'a self, ctx: &'a Ctx, _claim: &'a mut ConformanceClaim) -> HookFuture<'a> {
        Box::pin(async move {
            self.entered.notify_one();
            let mut released = self.release.subscribe();
            tokio::select! {
                r = released.wait_for(|open| *open) => {
                    r.map(|_| ()).map_err(|_| BoxError::from("the gate was dropped"))
                }
                _ = ctx.cancelled() => Err("the attempt was cancelled at the gate".into()),
            }
        })
    }
}

/// A worker running on a background task, driving one type on one
/// simulated replica. [`Replica::stop`] cancels it and waits for the
/// graceful drain. Dropping an unstopped replica cancels it without
/// waiting.
///
/// There is intentionally no kill: the worker has no non-graceful stop.
/// Use `claim_batch` + `force_expire_claim` for crash / expired-lease
/// adoption.
pub struct Replica {
    ctx: Ctx,
    task: Option<JoinHandle<()>>,
}

impl Replica {
    /// Cancels the worker and waits until every in-flight attempt has
    /// completed.
    pub async fn stop(mut self) {
        self.ctx.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Replica {
    fn drop(&mut self) {
        self.ctx.cancel();
    }
}

/// Starts a worker on `store` under `cfg` on a background task. The free
/// function two-replica scenarios use to run a second worker over a
/// separately pooled store, or a variant adapter.
pub fn run_worker<A, R, F>(
    store: TypedStore<Spec, Status, A>,
    cfg: WorkerConfig,
    rec: R,
    after: F,
) -> Result<Replica, Error>
where
    A: Adapter<Spec, Status>,
    R: Reconciler<Spec, Status, A>,
    F: AfterComplete<Spec, Status>,
{
    let worker = Worker::new(store, cfg, rec, after)?;
    let ctx = Ctx::background().child();
    let task = tokio::spawn(worker.run(ctx.clone()));
    Ok(Replica {
        ctx,
        task: Some(task),
    })
}
