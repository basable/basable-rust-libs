//! Schema-level conformance, ported from `processingobject_schema_test.go`:
//! the claim scan rides the partial due_at index (not a sequential scan),
//! and a status constraint violation is classified into a loud Retry by the
//! completion's savepoint.

use basable_processingobject::{Outcome, Phase, SCHEDULE_PARKED_SQL};
use basable_processingobject_testkit::{Harness, PUBLIC_ID_PREFIX, Spec, Status};
use uuid::Uuid;

fn seed(count: usize) -> (Vec<Uuid>, Vec<String>) {
    let ids: Vec<Uuid> = (0..count).map(|_| Uuid::new_v4()).collect();
    let external: Vec<String> = ids
        .iter()
        .map(|id| basable_publicid::encode(PUBLIC_ID_PREFIX, *id))
        .collect();
    (ids, external)
}

/// Behaviour 21: seeded with mostly-parked rows so the range predicate is
/// selective, the planner must choose the partition's child of
/// idx_processing_object_scan.
#[tokio::test]
async fn the_claim_scan_rides_the_partial_due_at_index() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let (parked, parked_ext) = seed(3000);
    let (due, due_ext) = seed(5);
    let su = h.db.superuser();
    for (ids, ext, observed, next) in [
        (&parked, &parked_ext, 1i64, SCHEDULE_PARKED_SQL),
        (
            &due,
            &due_ext,
            0i64,
            "'1970-01-01 00:00:00+00'::timestamptz",
        ),
    ] {
        sqlx::query(&format!(
            "INSERT INTO nano_conformance.processing_object_conformance
                 (processing_object_type_key, id, external_id, name, namespace, generation, observed_generation, next_reconcile_at)
             SELECT 32000, u, e, 'conformance-' || u::text, $1, 1, $2, {next}
             FROM unnest($3::uuid[], $4::text[]) AS seed(u, e)"
        ))
        .bind(h.namespace)
        .bind(observed)
        .bind(ids)
        .bind(ext)
        .execute(su)
        .await
        .unwrap();
    }
    sqlx::query("ANALYZE nano_conformance.processing_object_conformance")
        .execute(su)
        .await
        .unwrap();

    let (child,): (String,) = sqlx::query_as(
        "SELECT c.relname FROM pg_inherits i
         JOIN pg_class c ON c.oid = i.inhrelid
         JOIN pg_class p ON p.oid = i.inhparent
         WHERE p.relname = 'idx_processing_object_scan' AND c.relname LIKE 'processing_object_conformance%'",
    )
    .fetch_one(su)
    .await
    .unwrap();

    let plan: Vec<(String,)> = sqlx::query_as(&format!(
        "EXPLAIN SELECT id, claim_token IS NOT NULL AS adopted
         FROM nano_conformance.processing_object_conformance
         WHERE due_at < {SCHEDULE_PARKED_SQL}
           AND due_at <= now()
           AND (claim_token IS NULL OR lease_expires_at <= now())
         ORDER BY due_at ASC, last_reconciled_at ASC NULLS FIRST, id ASC
         LIMIT 10"
    ))
    .fetch_all(su)
    .await
    .unwrap();
    let text: String = plan.into_iter().map(|(l,)| l + "\n").collect();
    assert!(
        !text.contains("Seq Scan"),
        "the claim scan must not fall back to a sequential scan:\n{text}"
    );
    assert!(
        text.contains(&child),
        "the claim scan must ride the partial due_at index {child}:\n{text}"
    );
    h.finish().await;
}

/// Behaviour 22: a status the typed table rejects is not a completion
/// livelock — the savepoint rolls it back and classifies a loud Retry,
/// preserving the prior status.
#[tokio::test]
async fn a_status_constraint_violation_classifies_as_retry() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 5,
            content: "c".into(),
        })
        .await
        .unwrap();
    let good = Status {
        provisioned_widgets: 5,
        external_id: "good".into(),
    };
    let (_, done) = h
        .drive_once(|_| {
            Box::pin(async {
                Ok(Outcome::converged(Some(Status {
                    provisioned_widgets: 5,
                    external_id: "good".into(),
                })))
            })
        })
        .await
        .unwrap();
    assert!(done.outcome().unwrap().is_converged());

    h.store.nudge(&r).await.unwrap();
    let (_, done) = h
        .drive_once(|_| {
            Box::pin(async {
                Ok(Outcome::converged(Some(Status {
                    provisioned_widgets: -1,
                    external_id: "bad".into(),
                })))
            })
        })
        .await
        .expect("the constraint violation commits as a Retry, never errors the completion");
    let out = done.outcome().unwrap();
    assert!(out.is_retry());
    assert!(
        out.cause().unwrap().to_string().contains("constraint"),
        "{}",
        out.cause().unwrap()
    );

    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.meta.phase, Phase::Retrying);
    assert!(obj.meta.last_error.contains("constraint"));
    assert_eq!(
        obj.status, good,
        "the rejected write rolled back; the prior status survives"
    );
    h.finish().await;
}
