//! The reconciler of `shipment` and its worker policy.
//!
//! The pass is LEVEL-TRIGGERED: read the whole claim-time snapshot
//! (`claim.object` — meta, spec, status) and do whatever that state still
//! requires. `deleting()` first; check-then-act, never fire-and-forget;
//! every provider write through `self.this.calls.<x>.dispatch(ctx, claim,
//! args)` with the match spelled out; exactly ONE `Outcome`. Teardown
//! settles only on confirmed absence (the Directive §3 invariant 6).
//!
//! An `Err` from the pass is any `BoxError` (a `?` on a query, a dispatch)
//! and resolves to `Retry { status: None }`; a pass that knows what it
//! wants written returns an `Outcome` instead.
//!
//! `WorkerConfig`'s scheduling fields (resync, backoff, max_attempts,
//! attempt_timeout) MUST be identical on every replica.

use std::time::Duration;

use basable_core::{BoxError, Ctx};
use basable_processingobject::{
    Claim, NoAfterComplete, Outcome, Reconciler as ReconcilerTrait, Worker, WorkerConfig,
};

use super::{Adapter, Spec, Status};
use crate::Order;

/// The pass's two handles: this nanoservice's sender (exactly its declared
/// sends, built once from the router) and the component itself.
pub struct Reconciler<R: 'static> {
    pub(crate) sender: interfaces::OrderSender<'static, R>,
    pub(crate) this: &'static Order,
}

impl<R> ReconcilerTrait<Spec, Status, Adapter> for Reconciler<R>
where
    R: interfaces::OrderRoutes + Send + Sync + 'static,
{
    async fn reconcile(&self, ctx: &Ctx, claim: &mut Claim<Spec, Status, Adapter>) -> Result<Outcome<Status>, BoxError> {
        if claim.object.meta.deleting() {
            return self.reconcile_deleting(ctx, claim).await;
        }

        // TODO: the pass. Sketch:
        //   0. `let mut status = claim.object.status.clone();` — ONE working
        //      value; the completion writes the whole row.
        //   1. observe: re-check external state (a lookup adapter / a query);
        //   2. act: `match self.this.calls.x.dispatch(ctx, claim, args).await {
        //          Ok(res) => { /* consume res into status */ }
        //          Err(DispatchError::OwnershipLost) => return Ok(Outcome::retry(None, "ownership lost")),
        //          Err(DispatchError::Ambiguous(e)) => return Ok(Outcome::retry(Some(status), e)), // may have landed: re-drive
        //          Err(DispatchError::Definitive(e)) => return Ok(Outcome::blocked(Some(status), e)),
        //          Err(DispatchError::ReplayWindowElapsed { .. }) => return Ok(Outcome::retry(Some(status), "replay window elapsed: hold")),
        //      }` (`basable_externaleffect::DispatchError`);
        //   3. one outcome: `Outcome::converged(Some(status))`, `Outcome::converged_after(.., d)`, `Outcome::requeue_now(..)`.
        //   A send to another nanoservice is `self.sender.send_<message>(ctx, m).await`.
        let _ = self.sender;
        tracing::warn!(step = "order.shipment.reconcile", "unimplemented step reached");
        Ok(Outcome::converged(None))
    }
}

impl<R> Reconciler<R>
where
    R: interfaces::OrderRoutes + Send + Sync + 'static,
{
    async fn reconcile_deleting(&self, _ctx: &Ctx, _claim: &mut Claim<Spec, Status, Adapter>) -> Result<Outcome<Status>, BoxError> {
        // TODO: re-drive the teardown, then CONFIRM absence (a lookup); only
        // then `Outcome::delete()` (or `Outcome::settled(..)` for a
        // tombstone). Anything else is a loud `Outcome::retry`.
        Ok(Outcome::retry(None, "order.shipment: teardown not implemented"))
    }
}

/// The worker `main.rs` registers for this type: one per replica, identical
/// policy everywhere. The app hands it the wake bus and joins it on
/// shutdown.
pub fn worker<R>(router: &'static R, this: &'static Order) -> Worker<Spec, Status, Adapter, Reconciler<R>, NoAfterComplete>
where
    R: interfaces::OrderRoutes + Send + Sync + 'static,
{
    Worker::new(
        this.shipment_store.clone(),
        WorkerConfig {
            resync: Duration::from_secs(10 * 60),
            attempt_timeout: Duration::from_secs(5 * 60),
            max_attempts: 0,
            ..WorkerConfig::default()
        },
        Reconciler { sender: interfaces::OrderSender::new(router), this },
        NoAfterComplete,
    )
    .expect("order.shipment: a valid worker policy")
}
