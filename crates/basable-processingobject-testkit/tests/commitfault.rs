//! Commit-fault conformance for completion, ported from
//! `processingobject_commitfault_test.go` (behaviour 19): a completion whose
//! COMMIT acknowledgement is lost is retried once and converges
//! idempotently. Applied: the retry adopts the landed transaction and
//! reports Unknown. Rolled back: the retry re-runs cleanly and reports the
//! real outcome. Either way the object is converged with the status written
//! once. (Behaviour 18, the ambiguous create, lives in `store.rs`; behaviour
//! 20, the ack-loss redrive, needs the worker.)

use basable_db::{NanoPool, PoolConfig};
use basable_processingobject::{Completion, Outcome, Phase, TypedStore};
use basable_processingobject_testkit::{Conformance, Harness, Spec, Status, conformance_type};
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
