//! Deletion and finalization conformance, ported from
//! `processingobject_deletion_test.go`: one-way deletion, intent guards,
//! explicit hard delete versus a settled soft-delete tombstone, and the
//! savepoint rollback of a failed finalizer.

use basable_processingobject::{
    Adapter, Error, Object, Outcome, Phase, ProcessingObjectType, Ref, Row, Tx, TypedStore,
};
use basable_processingobject_testkit::{
    ConformanceAdapter, Harness, PUBLIC_ID_PREFIX, Spec, Status, TYPE_KEY, TYPE_NAME,
};
use uuid::Uuid;

/// Behaviour 5: mark_deleted is one-way and idempotent, a later update_spec
/// is refused, a nudge is permitted.
#[tokio::test]
async fn deletion_is_one_way_and_guards_intent() {
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
    assert_eq!(h.meta(&r).await.unwrap().generation, 1);

    assert!(h.store.mark_deleted(&r).await.unwrap());
    let first = h.meta(&r).await.unwrap();
    assert!(first.deleted_at.is_some());
    assert_eq!(first.generation, 2);

    assert!(!h.store.mark_deleted(&r).await.unwrap());
    let second = h.meta(&r).await.unwrap();
    assert_eq!(second.deleted_at, first.deleted_at, "deleted_at is stable");
    assert_eq!(second.generation, 2);

    let err = h
        .store
        .update_spec(&r, |s, _| {
            s.content = "resurrect".into();
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Deleting(_)), "{err}");
    h.store.nudge(&r).await.unwrap();
    h.finish().await;
}

/// Behaviour 6: a deleting object is not finalized by an ordinary Converged;
/// only a confirmed Delete removes the envelope.
#[tokio::test]
async fn converged_does_not_finalize_a_deleting_object() {
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

    let (_, done) = h
        .drive_once(|_| Box::pin(async { Ok(Outcome::requeue_now(None)) }))
        .await
        .unwrap();
    assert!(!done.outcome().unwrap().is_delete());
    let obj = h.store.read(&r).await.unwrap();
    assert!(
        obj.deleting(),
        "a converged pass leaves the deleting row present"
    );

    let (snapshot, done) = h
        .drive_once(|_| Box::pin(async { Ok(Outcome::delete()) }))
        .await
        .unwrap();
    assert!(done.outcome().unwrap().is_delete());
    assert_eq!(snapshot.id, r.id);
    assert!(matches!(h.store.read(&r).await, Err(Error::NotFound(_))));
    h.finish().await;
}

/// Settled on a deleting object is a parked soft-delete tombstone; a nudge
/// re-arms it and Delete can still finalize.
#[tokio::test]
async fn settled_retains_a_parked_tombstone() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "soft-delete".into(),
        })
        .await
        .unwrap();
    h.store.mark_deleted(&r).await.unwrap();

    let (_, done) = h
        .drive_once(|_| {
            Box::pin(async {
                Ok(Outcome::settled(Some(Status {
                    provisioned_widgets: 7,
                    external_id: String::new(),
                })))
            })
        })
        .await
        .unwrap();
    assert!(done.outcome().unwrap().is_settled());
    let obj = h.store.read(&r).await.unwrap();
    assert!(obj.deleting());
    assert_eq!(obj.meta.phase, Phase::Converged);
    assert_eq!(obj.status.provisioned_widgets, 7);
    assert!(obj.meta.parked());
    assert!(
        h.claim_batch().await.unwrap().is_empty(),
        "a settled tombstone is outside the due-work index"
    );

    h.store.nudge(&r).await.unwrap();
    let (_, done) = h
        .drive_once(|_| Box::pin(async { Ok(Outcome::delete()) }))
        .await
        .unwrap();
    assert!(done.outcome().unwrap().is_delete());
    assert!(matches!(h.store.read(&r).await, Err(Error::NotFound(_))));
    h.finish().await;
}

/// Behaviour 8: a deletion requested mid-claim drives the object to
/// finalization on the next teardown pass, with the archive committed.
#[tokio::test]
async fn a_deletion_during_reconcile_drives_to_finalization() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "midflight".into(),
        })
        .await
        .unwrap();
    let claim = h.claim_batch().await.unwrap().remove(0);
    h.store.mark_deleted(&r).await.unwrap();

    let done = claim
        .complete(Ok(Outcome::converged(Some(Status {
            provisioned_widgets: 1,
            external_id: "obs".into(),
        }))))
        .await
        .unwrap();
    assert!(done.superseded());

    let (snapshot, done) = h
        .drive_once(|c| {
            assert!(c.object.deleting(), "the re-drive observes the deletion");
            Box::pin(async { Ok(Outcome::delete()) })
        })
        .await
        .unwrap();
    assert!(done.outcome().unwrap().is_delete());
    assert_eq!(snapshot.status.external_id, "obs");
    assert!(matches!(h.store.read(&r).await, Err(Error::NotFound(_))));
    assert!(
        h.is_archived(r.id).await.unwrap(),
        "finalize_delete committed the archive row"
    );
    h.finish().await;
}

/// An adapter whose finalizer always fails.
struct FailingFinalize(ConformanceAdapter);

impl Adapter<Spec, Status> for FailingFinalize {
    async fn insert_spec(&self, tx: &mut Tx<'_>, r: &Ref, spec: &Spec) -> Result<(), sqlx::Error> {
        self.0.insert_spec(tx, r, spec).await
    }
    async fn insert_status(&self, tx: &mut Tx<'_>, r: &Ref, s: &Status) -> Result<(), sqlx::Error> {
        self.0.insert_status(tx, r, s).await
    }
    async fn read_rows(
        &self,
        tx: &mut Tx<'_>,
        ids: &[Uuid],
    ) -> Result<Vec<Row<Spec, Status>>, sqlx::Error> {
        self.0.read_rows(tx, ids).await
    }
    async fn write_spec(&self, tx: &mut Tx<'_>, r: &Ref, spec: &Spec) -> Result<(), sqlx::Error> {
        self.0.write_spec(tx, r, spec).await
    }
    async fn write_status(&self, tx: &mut Tx<'_>, r: &Ref, s: &Status) -> Result<(), sqlx::Error> {
        self.0.write_status(tx, r, s).await
    }
    async fn finalize_delete(
        &self,
        _: &mut Tx<'_>,
        _: &Object<Spec, Status>,
    ) -> Result<(), sqlx::Error> {
        Err(sqlx::Error::Protocol("teardown wedged".into()))
    }
}

/// A failing finalizer is rolled back to its savepoint and the completion is
/// committed as a loud Retry: the row survives, no archive is written.
#[tokio::test]
async fn a_failed_finalizer_reports_retry_not_delete() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "wedged".into(),
        })
        .await
        .unwrap();
    h.store.mark_deleted(&r).await.unwrap();
    let store = TypedStore::bind(
        &h.pool,
        ProcessingObjectType::new(
            TYPE_NAME,
            TYPE_KEY,
            PUBLIC_ID_PREFIX,
            FailingFinalize(ConformanceAdapter),
        ),
    )
    .await
    .unwrap();

    let (_, done) = Harness::drive_once_with(&store, h.config(), |_| {
        Box::pin(async { Ok(Outcome::delete()) })
    })
    .await
    .expect("a failed finalizer commits a Retry, never errors the completion");
    let out = done.outcome().unwrap();
    assert!(!out.is_delete() && out.is_retry());
    assert!(
        out.cause()
            .unwrap()
            .to_string()
            .contains("delete finalization failed")
    );

    let obj = h.store.read(&r).await.unwrap();
    assert!(obj.deleting());
    assert_eq!(obj.meta.phase, Phase::Retrying);
    assert!(
        obj.meta.last_error.contains("delete finalization failed"),
        "{}",
        obj.meta.last_error
    );
    assert!(!h.is_archived(r.id).await.unwrap());
    h.finish().await;
}
