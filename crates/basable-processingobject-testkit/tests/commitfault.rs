//! Commit-fault conformance for completion, ported from
//! `processingobject_commitfault_test.go` (behaviour 19): a completion whose
//! COMMIT acknowledgement is lost is retried once and converges
//! idempotently. Applied: the retry adopts the landed transaction and
//! reports Unknown. Rolled back: the retry re-runs cleanly and reports the
//! real outcome. Either way the object is converged with the status written
//! once. Behaviour 20 is the provider-side twin: an effect whose ack is
//! lost is replayed by the worker and lands once. (Behaviour 18, the
//! ambiguous create, lives in `store.rs`.)

use basable_db::{NanoPool, PoolConfig};
use std::time::Duration;

use basable_processingobject::{Completion, NoAfterComplete, Outcome, Phase, TypedStore};
use basable_processingobject_testkit::{
    Conformance, Harness, Spec, Status, TYPE_NAME, canonical_key, conformance_type, reconciled,
};
use basable_testkit::{CommitFault, CommitFaultProxy};

async fn completion_retries_and_converges(mode: CommitFault) {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let (proxy, options) = CommitFaultProxy::for_options(h.db.app_options())
        .await
        .unwrap();
    let pool = NanoPool::<Conformance>::connect(options, PoolConfig::default())
        .await
        .unwrap();
    let store = TypedStore::bind(&pool, conformance_type()).await.unwrap();

    // A clean create; the fault is armed only after the claim landed.
    let r = h
        .create(Spec {
            widgets: 4,
            content: "c".into(),
        })
        .await
        .unwrap();
    let claim = store.claim_batch(h.config()).await.unwrap().remove(0);
    let widgets = claim.object.spec.widgets;
    proxy.arm(mode);
    let done = claim
        .complete(Ok(Outcome::converged(Some(Status {
            provisioned_widgets: widgets,
            external_id: "done".into(),
        }))))
        .await
        .expect("the completion resolves after retrying the ambiguous commit");
    assert!(!proxy.is_armed());
    match mode {
        CommitFault::Applied => assert!(
            done.is_unknown(),
            "the adopted landed completion has an unknown normalized form"
        ),
        CommitFault::RolledBack => {
            assert!(
                matches!(done, Completion::Committed { .. }),
                "a rolled-back commit is re-run with a known outcome"
            );
            assert!(done.outcome().unwrap().is_converged());
        }
    }

    let obj = h.store.read(&r).await.unwrap();
    assert!(obj.observed_current());
    assert_eq!(obj.meta.phase, Phase::Converged);
    assert_eq!(
        obj.status.provisioned_widgets, 4,
        "the observed status committed exactly once"
    );
    assert!(obj.meta.claimed_at.is_none(), "the claim was released");
    pool.close().await;
    drop(proxy);
    h.finish().await;
}

#[tokio::test]
async fn an_applied_ambiguous_completion_is_adopted_as_unknown() {
    completion_retries_and_converges(CommitFault::Applied).await;
}

#[tokio::test]
async fn a_rolled_back_ambiguous_completion_is_rerun() {
    completion_retries_and_converges(CommitFault::RolledBack).await;
}

/// Behaviour 20: an idempotent effect whose provider ack is lost lands
/// exactly once. The first pass applies the resource but sees an error, so
/// it completes as a retry; the worker's next pass replays the upsert,
/// which the provider recognises, and converges.
#[tokio::test]
async fn ack_loss_idempotent_redrive() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    // The provider applies the resource but drops the ack on the first call.
    h.sim.drop_ack_next();
    let replica = h
        .start_worker(h.example_reconciler(), NoAfterComplete)
        .unwrap();

    let r = h
        .create(Spec {
            widgets: 3,
            content: "durable".into(),
        })
        .await
        .unwrap();
    let obj = h
        .wait_for(&r, Duration::from_secs(30), reconciled)
        .await
        .unwrap();
    assert_eq!(obj.status.provisioned_widgets, 3);
    assert_eq!(
        h.sim.count(&canonical_key(TYPE_NAME, r.id)),
        1,
        "the idempotent effect must land exactly once despite the retry"
    );

    replica.stop().await;
    h.finish().await;
}
