//! Mid-attempt fenced status write conformance, ported from
//! `processingobject_writestatus_test.go`: durable across a dropped claim
//! and visible to the adopting successor, fenced after a lease steal,
//! envelope untouched except the lease, lands under a superseded
//! generation, rejected rows write nothing and keep the claim open, the
//! completion's no-status forms preserve it, an ambiguous commit keeps the
//! claim open, a post-completion write is fenced, and a deleting object
//! takes the marker.

use std::time::Duration;

use basable_db::{NanoPool, PoolConfig};
use basable_processingobject::{Error, Outcome, Phase, TypedStore};
use basable_processingobject_testkit::{Conformance, Harness, Spec, Status, conformance_type};
use basable_testkit::{CommitFault, CommitFaultProxy};

fn status(w: i32, e: &str) -> Status {
    Status {
        provisioned_widgets: w,
        external_id: e.into(),
    }
}

/// W1: durable across a dropped claim; the successor's claim-time snapshot
/// is the mid-attempt write.
#[tokio::test]
async fn a_mid_attempt_write_survives_the_declarer_and_reaches_the_successor() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 4,
            content: "declare".into(),
        })
        .await
        .unwrap();
    let mut declarer = h.claim_batch().await.unwrap().remove(0);
    let declared = status(4, "declared:order/1");
    declarer.write_status(declared.clone()).await.unwrap();
    assert_eq!(
        declarer.object.status, declared,
        "the claim's view advances"
    );

    drop(declarer);
    h.force_expire_claim(&r).await.unwrap();
    let successor = h.claim_batch().await.unwrap().remove(0);
    assert!(successor.adopted());
    assert_eq!(
        successor.object.status, declared,
        "the successor sees the write, not the create-time status"
    );

    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.meta.phase, Phase::Pending);
    assert_eq!((obj.meta.observed_generation, obj.meta.attempts), (0, 0));
    assert_eq!(obj.status, declared);
    h.finish().await;
}

/// W2: after a lease steal the zombie's write is fenced, nothing is written,
/// and its later completion is fenced too.
#[tokio::test]
async fn a_zombie_write_after_a_lease_steal_is_fenced_and_sticky() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "steal".into(),
        })
        .await
        .unwrap();
    let mut zombie = h.claim_batch().await.unwrap().remove(0);
    h.force_expire_claim(&r).await.unwrap();
    let mut successor = h.claim_batch().await.unwrap().remove(0);

    assert!(matches!(
        zombie.write_status(status(99, "zombie")).await,
        Err(Error::Fenced)
    ));
    assert_eq!(
        h.store.read(&r).await.unwrap().status,
        Status::default(),
        "a fenced write touches nothing"
    );
    assert!(matches!(
        zombie
            .complete(Ok(Outcome::converged(Some(status(98, "zombie-late")))))
            .await,
        Err(Error::Fenced)
    ));

    successor
        .write_status(status(1, "successor-declared"))
        .await
        .unwrap();
    let s = successor.object.status.clone();
    let done = successor
        .complete(Ok(Outcome::converged(Some(s))))
        .await
        .unwrap();
    assert!(!done.superseded());
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.status.external_id, "successor-declared");
    assert_eq!(obj.meta.phase, Phase::Converged);
    h.finish().await;
}

/// W3: the write touches nothing on the envelope except the lease (and the
/// local proof), and the row stays unstealable.
#[tokio::test]
async fn a_write_leaves_the_envelope_untouched_except_the_lease() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 2,
            content: "untouched".into(),
        })
        .await
        .unwrap();
    let mut claim = h.claim_batch().await.unwrap().remove(0);
    let before = h.envelope(&r).await;
    let lease0 = h.lease_expires_at(&r).await.unwrap();
    let proof0 = claim.ownership_deadline().unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    claim.write_status(status(2, "mid")).await.unwrap();

    let after = h.envelope(&r).await;
    assert_eq!(before, after, "nothing settled, scheduled or re-fenced");
    assert!(after.claim_token.is_some(), "the claim is still held");
    assert!(h.lease_expires_at(&r).await.unwrap() > lease0);
    assert!(claim.ownership_deadline().unwrap().mono() > proof0.mono());
    assert_eq!(h.store.read(&r).await.unwrap().status.external_id, "mid");

    let (pool2, store2) = h.second_store().await;
    assert!(store2.claim_batch(h.config()).await.unwrap().is_empty());
    pool2.close().await;
    h.finish().await;
}

/// W4: a superseded attempt still writes, the fresh generation's reset is
/// preserved, and the eventual completion is superseded and woken with the
/// written status kept.
#[tokio::test]
async fn a_write_lands_under_a_superseded_generation() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "gen1".into(),
        })
        .await
        .unwrap();
    let mut stale = h.claim_batch().await.unwrap().remove(0);
    h.store
        .update_spec(&r, |s, _| {
            s.content = "gen2".into();
            Ok(())
        })
        .await
        .unwrap();
    h.store.nudge(&r).await.unwrap();

    let declared = status(1, "declared-under-gen1");
    stale.write_status(declared.clone()).await.unwrap();
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.status, declared);
    assert_eq!((obj.meta.generation, obj.meta.observed_generation), (2, 0));
    assert_eq!(obj.meta.phase, Phase::Pending);
    assert!(obj.meta.last_error.is_empty());

    let done = stale.complete(Ok(Outcome::converged(None))).await.unwrap();
    assert!(done.superseded() && done.woken());
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(
        obj.status, declared,
        "converged(None) leaves the write in place"
    );
    assert!(!obj.observed_current());
    h.finish().await;
}

/// W5: a status the schema rejects writes nothing, is not a fence, and
/// leaves the claim open.
#[tokio::test]
async fn a_rejected_row_writes_nothing_and_keeps_the_claim() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 3,
            content: "reject".into(),
        })
        .await
        .unwrap();
    let mut claim = h.claim_batch().await.unwrap().remove(0);

    let err = claim.write_status(status(-1, "bad")).await.unwrap_err();
    assert!(matches!(err, Error::Sql { .. }), "{err}");
    assert_eq!(
        claim.object.status,
        Status::default(),
        "the claim's view did not advance"
    );
    assert_eq!(h.store.read(&r).await.unwrap().status, Status::default());

    claim.write_status(status(3, "good")).await.unwrap();
    let s = claim.object.status.clone();
    let done = claim
        .complete(Ok(Outcome::converged(Some(s))))
        .await
        .unwrap();
    assert!(!done.superseded());
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.status.external_id, "good");
    assert_eq!(obj.meta.phase, Phase::Converged);
    h.finish().await;
}

/// W6: the completion's no-status forms preserve a mid-attempt write, driven
/// through drive_once so the write happens inside a real pass.
#[tokio::test]
async fn no_status_completions_preserve_a_mid_attempt_write() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    for (retry, phase) in [(false, Phase::Converged), (true, Phase::Retrying)] {
        let r = h
            .create(Spec {
                widgets: 5,
                content: "keep".into(),
            })
            .await
            .unwrap();
        let mid = status(5, "mid-attempt");
        let (_, done) = h
            .drive_once(|c| {
                let mid = mid.clone();
                Box::pin(async move {
                    c.write_status(mid).await?;
                    Ok(if retry {
                        Outcome::retry(None, "call failed after declare")
                    } else {
                        Outcome::converged(None)
                    })
                })
            })
            .await
            .unwrap();
        assert!(done.outcome().unwrap().status().is_none());
        let obj = h.store.read(&r).await.unwrap();
        assert_eq!(obj.status, mid);
        assert_eq!(obj.meta.phase, phase);
    }
    h.finish().await;
}

/// W7: an ambiguous COMMIT of a mid-attempt write is reported without retry
/// and without closing the claim; the attempt can still complete.
async fn ambiguous_write_keeps_the_claim_open(mode: CommitFault) {
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

    let r = h
        .create(Spec {
            widgets: 2,
            content: "fault".into(),
        })
        .await
        .unwrap();
    let mut claim = store.claim_batch(h.config()).await.unwrap().remove(0);
    let mid = status(2, "maybe-declared");
    proxy.arm(mode);
    let err = claim.write_status(mid.clone()).await.unwrap_err();
    assert!(err.is_commit_unknown(), "{err}");
    assert_eq!(
        claim.object.status,
        Status::default(),
        "the view does not advance on an unacknowledged write"
    );

    let obj = h.store.read(&r).await.unwrap();
    match mode {
        CommitFault::Applied => assert_eq!(obj.status, mid, "applied: the write landed"),
        CommitFault::RolledBack => {
            assert_eq!(obj.status, Status::default(), "rolled back: nothing landed")
        }
    }

    let done = claim
        .complete(Ok(Outcome::retry(None, "declare ambiguous, not sent")))
        .await
        .unwrap();
    assert!(done.outcome().unwrap().is_retry());
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.meta.phase, Phase::Retrying);
    match mode {
        CommitFault::Applied => assert_eq!(obj.status, mid),
        CommitFault::RolledBack => assert_eq!(obj.status, Status::default()),
    }
    pool.close().await;
    drop(proxy);
    h.finish().await;
}

#[tokio::test]
async fn an_ambiguous_write_that_applied_keeps_the_claim_open() {
    ambiguous_write_keeps_the_claim_open(CommitFault::Applied).await;
}

#[tokio::test]
async fn an_ambiguous_write_that_rolled_back_keeps_the_claim_open() {
    ambiguous_write_keeps_the_claim_open(CommitFault::RolledBack).await;
}

/// W8 by type: `complete` consumes the claim, so a write after completion
/// does not compile. The lease handle a worker keeps is fenced instead.
#[tokio::test]
async fn the_lease_handle_is_fenced_after_completion() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "late".into(),
        })
        .await
        .unwrap();
    let claim = h.claim_batch().await.unwrap().remove(0);
    let handle = claim.lease_handle();
    claim
        .complete(Ok(Outcome::converged(Some(status(1, "final")))))
        .await
        .unwrap();
    assert!(matches!(handle.heartbeat().await, Err(Error::Fenced)));
    assert_eq!(h.store.read(&r).await.unwrap().status.external_id, "final");
    h.finish().await;
}

/// W9: deletion intent does not block the marker; the deleting pass may keep
/// waiting with it in place.
#[tokio::test]
async fn a_write_lands_on_a_deleting_object() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "teardown".into(),
        })
        .await
        .unwrap();
    h.store.mark_deleted(&r).await.unwrap();
    let mut claim = h.claim_batch().await.unwrap().remove(0);
    assert!(claim.object.deleting());
    let before = h.envelope(&r).await;

    let marker = status(1, "declared:sweep");
    claim.write_status(marker.clone()).await.unwrap();
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.status, marker);
    assert!(obj.deleting());
    assert_eq!(before, h.envelope(&r).await);

    let done = claim
        .complete(Ok(Outcome::converged_after(None, Duration::from_secs(60))))
        .await
        .unwrap();
    assert!(!done.superseded());
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.status, marker);
    assert!(obj.deleting());
    h.finish().await;
}
