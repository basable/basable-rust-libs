//! Fencing, lease adoption, superseded and wake conformance, ported from
//! `processingobject_fencing_test.go`: driven through manual claim_batch,
//! complete and heartbeat plus raw SQL so each scenario targets one object at
//! a precise point in its lifecycle.

use std::time::Duration;

use basable_processingobject::{
    Adapter, Error, Outcome, Phase, ProcessingObjectType, Row, Tx, TypedStore,
};
use basable_processingobject_testkit::{
    ConformanceAdapter, Harness, PUBLIC_ID_PREFIX, Spec, Status, TYPE_KEY, TYPE_NAME,
};
use chrono::Utc;
use uuid::Uuid;

fn status(w: i32, e: &str) -> Status {
    Status {
        provisioned_widgets: w,
        external_id: e.into(),
    }
}

/// Behaviour 1: an expired lease is adopted by a successor with a fresh
/// token, and the original holder's zombie completion is fenced.
#[tokio::test]
async fn expired_lease_adoption_fences_the_zombie_completion() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 4,
            content: "adopt".into(),
        })
        .await
        .unwrap();

    let mut first = h.claim_batch().await.unwrap();
    assert_eq!(first.len(), 1);
    let zombie = first.remove(0);
    assert!(!zombie.adopted());

    h.force_expire_claim(&r).await.unwrap();
    let mut second = h.claim_batch().await.unwrap();
    assert_eq!(
        second.len(),
        1,
        "the successor adopts the expired-lease row"
    );
    let successor = second.remove(0);
    assert_eq!(successor.object.id, r.id);
    assert!(successor.adopted());

    // The zombie still holds a live LOCAL proof, but the token was replaced.
    let err = zombie
        .complete(Ok(Outcome::converged(Some(status(99, "zombie")))))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Fenced), "{err}");

    let done = successor
        .complete(Ok(Outcome::converged(Some(status(7, "winner")))))
        .await
        .unwrap();
    assert!(!done.superseded());

    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.meta.phase, Phase::Converged);
    assert!(obj.observed_current());
    assert_eq!(
        obj.status.provisioned_widgets, 7,
        "the fenced zombie wrote nothing"
    );
    h.finish().await;
}

/// Behaviour 2: a superseded completion records nothing into the fresh
/// generation's phase and last_error, and leaves the object due now.
#[tokio::test]
async fn a_superseded_completion_preserves_the_new_generation_reset() {
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
    let stale = h.claim_batch().await.unwrap().remove(0);

    h.store
        .update_spec(&r, |s, _| {
            s.content = "gen2".into();
            Ok(())
        })
        .await
        .unwrap();

    let done = stale
        .complete(Err("stale attempt failed".into()))
        .await
        .unwrap();
    assert!(done.superseded());
    assert!(done.outcome().unwrap().is_retry());

    let obj = h.store.read(&r).await.unwrap();
    assert_eq!((obj.meta.generation, obj.meta.observed_generation), (2, 1));
    assert!(!obj.observed_current());
    assert_eq!(
        obj.meta.phase,
        Phase::Pending,
        "the stale failure did not write its phase"
    );
    assert!(obj.meta.last_error.is_empty());
    assert!(obj.meta.next_reconcile_at < Utc::now() + chrono::Duration::minutes(1));
    h.finish().await;
}

/// Behaviour 3: even a superseded completion commits the status it observed.
#[tokio::test]
async fn a_superseded_completion_still_commits_its_status() {
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
    let stale = h.claim_batch().await.unwrap().remove(0);
    h.store
        .update_spec(&r, |s, _| {
            s.content = "gen2".into();
            Ok(())
        })
        .await
        .unwrap();

    let done = stale
        .complete(Ok(Outcome::converged(Some(status(7, "observed")))))
        .await
        .unwrap();
    assert!(done.superseded());
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(
        obj.status,
        status(7, "observed"),
        "the observation committed under claim authority"
    );
    assert_eq!((obj.meta.generation, obj.meta.observed_generation), (2, 1));
    assert!(!obj.observed_current());
    h.finish().await;
}

/// Behaviour 4: a nudge that arrives mid-claim is not swallowed by a long
/// schedule — the completion is woken and the object is left due now.
#[tokio::test]
async fn a_wake_is_not_swallowed_by_a_long_schedule() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 2,
            content: "wake".into(),
        })
        .await
        .unwrap();
    let claim = h.claim_batch().await.unwrap().remove(0);
    h.store.nudge(&r).await.unwrap();

    let done = claim
        .complete(Ok(Outcome::converged_after(
            Some(status(9, "w")),
            Duration::from_secs(3600),
        )))
        .await
        .unwrap();
    assert!(done.woken());
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.status.provisioned_widgets, 9);
    assert!(
        obj.meta.next_reconcile_at < Utc::now() + chrono::Duration::minutes(1),
        "due now, not an hour out"
    );
    h.finish().await;
}

/// Behaviour 7: a completion whose envelope vanished mid-attempt is fenced on
/// a first attempt; it never resurrects the row.
#[tokio::test]
async fn first_attempt_absence_is_fenced() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "vanish".into(),
        })
        .await
        .unwrap();
    let claim = h.claim_batch().await.unwrap().remove(0);
    sqlx::query("DELETE FROM nano_conformance.processing_object_conformance WHERE id = $1")
        .bind(r.id)
        .execute(h.db.superuser())
        .await
        .unwrap();
    let err = claim
        .complete(Ok(Outcome::converged(Some(status(1, "x")))))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Fenced), "{err}");
    assert!(matches!(h.store.read(&r).await, Err(Error::NotFound(_))));
    h.finish().await;
}

/// Behaviour 9 and 13: a heartbeat advances the local proof and the database
/// lease, so a competing replica cannot steal the object and the holder
/// completes normally.
#[tokio::test]
async fn a_heartbeat_extends_the_proof_and_keeps_the_lease() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 3,
            content: "long".into(),
        })
        .await
        .unwrap();
    let claim = h.claim_batch().await.unwrap().remove(0);

    let proof0 = claim
        .ownership_deadline()
        .expect("a live claim has a proof");
    let lease0 = h
        .lease_expires_at(&r)
        .await
        .expect("a claimed row has a lease");
    tokio::time::sleep(Duration::from_millis(10)).await;
    claim.heartbeat().await.unwrap();
    assert!(
        claim.ownership_deadline().unwrap().mono() > proof0.mono(),
        "the proof moved"
    );
    assert!(
        h.lease_expires_at(&r).await.unwrap() > lease0,
        "the lease moved"
    );

    // The handle heartbeats on its own, the way a worker would.
    let handle = claim.lease_handle();
    handle.heartbeat().await.unwrap();

    let (pool2, store2) = h.second_store().await;
    let stolen = store2.claim_batch(h.config()).await.unwrap();
    assert!(
        stolen.is_empty(),
        "a heartbeated live lease is not stealable"
    );

    let done = claim
        .complete(Ok(Outcome::converged(Some(status(3, "done")))))
        .await
        .unwrap();
    assert!(!done.superseded());
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.meta.phase, Phase::Converged);
    assert_eq!(obj.status.provisioned_widgets, 3);
    assert!(obj.meta.claimed_at.is_none() && obj.meta.lease_expires_at.is_none());

    // The claim is closed: the handle's heartbeat is fenced now.
    assert!(matches!(handle.heartbeat().await, Err(Error::Fenced)));
    assert!(handle.ownership_deadline().is_none());
    pool2.close().await;
    h.finish().await;
}

/// Behaviour 10: generation_changed_at tracks accepted intent only.
#[tokio::test]
async fn generation_changed_at_tracks_intent_only() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "v1".into(),
        })
        .await
        .unwrap();
    let at_create = h.gen_changed_at(&r).await;
    h.store
        .update_spec(&r, |s, _| {
            s.content = "v2".into();
            Ok(())
        })
        .await
        .unwrap();
    let at_update = h.gen_changed_at(&r).await;
    assert!(at_update > at_create);

    h.drive_once(|_| Box::pin(async { Ok(Outcome::converged(None)) }))
        .await
        .unwrap();
    assert_eq!(
        h.gen_changed_at(&r).await,
        at_update,
        "a completion does not move it"
    );
    h.store.nudge(&r).await.unwrap();
    assert_eq!(
        h.gen_changed_at(&r).await,
        at_update,
        "a nudge does not move it"
    );
    h.store.mark_deleted(&r).await.unwrap();
    assert!(
        h.gen_changed_at(&r).await > at_update,
        "teardown is new intent"
    );
    h.finish().await;
}

/// An adapter returning the typed rows in reverse order.
struct Reversed(ConformanceAdapter);

impl Adapter<Spec, Status> for Reversed {
    async fn insert_spec(
        &self,
        tx: &mut Tx<'_>,
        r: &basable_processingobject::Ref,
        spec: &Spec,
    ) -> Result<(), sqlx::Error> {
        self.0.insert_spec(tx, r, spec).await
    }
    async fn insert_status(
        &self,
        tx: &mut Tx<'_>,
        r: &basable_processingobject::Ref,
        s: &Status,
    ) -> Result<(), sqlx::Error> {
        self.0.insert_status(tx, r, s).await
    }
    async fn read_rows(
        &self,
        tx: &mut Tx<'_>,
        ids: &[Uuid],
    ) -> Result<Vec<Row<Spec, Status>>, sqlx::Error> {
        let mut rows = self.0.read_rows(tx, ids).await?;
        rows.reverse();
        Ok(rows)
    }
    async fn write_spec(
        &self,
        tx: &mut Tx<'_>,
        r: &basable_processingobject::Ref,
        spec: &Spec,
    ) -> Result<(), sqlx::Error> {
        self.0.write_spec(tx, r, spec).await
    }
    async fn write_status(
        &self,
        tx: &mut Tx<'_>,
        r: &basable_processingobject::Ref,
        s: &Status,
    ) -> Result<(), sqlx::Error> {
        self.0.write_status(tx, r, s).await
    }
}

/// An adapter that omits one object's typed row.
struct Omitting(ConformanceAdapter, Uuid);

impl Adapter<Spec, Status> for Omitting {
    async fn insert_spec(
        &self,
        tx: &mut Tx<'_>,
        r: &basable_processingobject::Ref,
        spec: &Spec,
    ) -> Result<(), sqlx::Error> {
        self.0.insert_spec(tx, r, spec).await
    }
    async fn insert_status(
        &self,
        tx: &mut Tx<'_>,
        r: &basable_processingobject::Ref,
        s: &Status,
    ) -> Result<(), sqlx::Error> {
        self.0.insert_status(tx, r, s).await
    }
    async fn read_rows(
        &self,
        tx: &mut Tx<'_>,
        ids: &[Uuid],
    ) -> Result<Vec<Row<Spec, Status>>, sqlx::Error> {
        let mut rows = self.0.read_rows(tx, ids).await?;
        rows.retain(|r| r.id != self.1);
        Ok(rows)
    }
    async fn write_spec(
        &self,
        tx: &mut Tx<'_>,
        r: &basable_processingobject::Ref,
        spec: &Spec,
    ) -> Result<(), sqlx::Error> {
        self.0.write_spec(tx, r, spec).await
    }
    async fn write_status(
        &self,
        tx: &mut Tx<'_>,
        r: &basable_processingobject::Ref,
        s: &Status,
    ) -> Result<(), sqlx::Error> {
        self.0.write_status(tx, r, s).await
    }
}

/// Invariant 1: the claim path matches typed rows by id, so an adapter that
/// returns them in reverse order is tolerated.
#[tokio::test]
async fn a_claim_tolerates_reordered_typed_rows() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let a = h
        .create(Spec {
            widgets: 11,
            content: "A".into(),
        })
        .await
        .unwrap();
    let b = h
        .create(Spec {
            widgets: 22,
            content: "B".into(),
        })
        .await
        .unwrap();
    let store = TypedStore::bind(
        &h.pool,
        ProcessingObjectType::new(
            TYPE_NAME,
            TYPE_KEY,
            PUBLIC_ID_PREFIX,
            Reversed(ConformanceAdapter),
        ),
    )
    .await
    .unwrap();
    let claims = store.claim_batch(h.config()).await.unwrap();
    assert_eq!(claims.len(), 2);
    for c in &claims {
        let want = if c.object.id == a.id { 11 } else { 22 };
        assert!(c.object.id == a.id || c.object.id == b.id);
        assert_eq!(
            c.object.spec.widgets, want,
            "each claim carries its own spec"
        );
    }
    h.finish().await;
}

/// Invariant 1, the claim contract: an envelope whose typed row is missing
/// from a claim batch is dropped and logged, not surfaced; the rest of the
/// batch is unaffected.
#[tokio::test]
async fn a_claim_drops_an_envelope_with_a_missing_typed_row() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let kept = h
        .create(Spec {
            widgets: 1,
            content: "kept".into(),
        })
        .await
        .unwrap();
    let dropped = h
        .create(Spec {
            widgets: 2,
            content: "dropped".into(),
        })
        .await
        .unwrap();
    let store = TypedStore::bind(
        &h.pool,
        ProcessingObjectType::new(
            TYPE_NAME,
            TYPE_KEY,
            PUBLIC_ID_PREFIX,
            Omitting(ConformanceAdapter, dropped.id),
        ),
    )
    .await
    .unwrap();
    let claims = store.claim_batch(h.config()).await.unwrap();
    assert_eq!(
        claims.len(),
        1,
        "the row with no typed data is dropped from the batch"
    );
    assert_eq!(claims[0].object.id, kept.id);
    h.finish().await;
}

/// Label routing (the carve-outs): two disjoint selectors over one type
/// each claim exactly their own labelled slice; an unlabelled row matches
/// no filtered worker and only the unfiltered one picks it up.
#[tokio::test]
async fn label_routing_claims_disjoint_slices_and_unlabelled_rows_are_unroutable() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let labels = |v: &str| {
        basable_processingobject::CreateOptions::labels(
            [("infra_type".to_string(), v.to_string())]
                .into_iter()
                .collect(),
        )
    };
    let mut cloud = Vec::new();
    for _ in 0..3 {
        let id = Uuid::new_v4();
        h.store
            .create(
                id,
                h.identity(id),
                &Spec {
                    widgets: 1,
                    content: String::new(),
                },
                &Status::default(),
                labels("cloud"),
            )
            .await
            .unwrap();
        cloud.push(id);
    }
    let mut robot = Vec::new();
    for _ in 0..2 {
        let id = Uuid::new_v4();
        h.store
            .create(
                id,
                h.identity(id),
                &Spec {
                    widgets: 1,
                    content: String::new(),
                },
                &Status::default(),
                labels("bare_metal"),
            )
            .await
            .unwrap();
        robot.push(id);
    }
    let bare = h
        .create(Spec {
            widgets: 1,
            content: String::new(),
        })
        .await
        .unwrap();

    let selector = |v: &str| basable_processingobject::WorkerConfig {
        label_selector: [("infra_type".to_string(), v.to_string())]
            .into_iter()
            .collect(),
        ..h.config()
    };
    let claims = h.store.claim_batch(selector("cloud")).await.unwrap();
    assert_eq!(
        claims.len(),
        3,
        "the cloud worker claims exactly the cloud slice"
    );
    assert!(
        claims
            .iter()
            .all(|c| cloud.contains(&c.object.id) && !c.adopted())
    );
    let claims = h.store.claim_batch(selector("bare_metal")).await.unwrap();
    assert_eq!(claims.len(), 2);
    assert!(claims.iter().all(|c| robot.contains(&c.object.id)));
    // Nothing filtered claims the unlabelled row; the unfiltered worker does.
    assert!(
        h.store
            .claim_batch(selector("cloud"))
            .await
            .unwrap()
            .is_empty()
    );
    let claims = h.store.claim_batch(h.config()).await.unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].object.id, bare.id);
    h.finish().await;
}
