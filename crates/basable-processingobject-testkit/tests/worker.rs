//! Worker-runtime conformance, ported from `processingobject_worker_test.go`:
//! multi-replica claim exclusivity, crash adoption, panic containment,
//! parallelism-bounded claiming, transactional-wake vs poll scheduling, and
//! `after_complete` containment. These run the real worker loop against a
//! migrated Postgres.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use basable_core::Ctx;
use basable_processingobject::{Completion, NoAfterComplete, Object, Phase};
use basable_processingobject_testkit::{
    Gate, Harness, Spec, Status, TYPE_KEY, fast_config, hook, identity_name, reconciled,
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(30);

/// Behaviour 11: two replicas claiming concurrently take disjoint batches
/// (`FOR UPDATE SKIP LOCKED`). Every seeded row is claimed by exactly one.
/// The race is run twenty times over fresh rows; the earlier rounds' claims
/// stay leased and out of the way.
#[tokio::test]
async fn two_replica_claim_exclusivity() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let (pool2, store2) = h.second_store().await;
    let cfg = h.config();

    for round in 0..20 {
        const N: usize = 8;
        let mut seeded = Vec::with_capacity(N);
        for i in 0..N {
            let r = h
                .create(Spec {
                    widgets: i as i32,
                    content: format!("round-{round}-row-{i}"),
                })
                .await
                .unwrap();
            seeded.push(r.id);
        }

        let (a, b) = tokio::join!(
            h.store.claim_batch(cfg.clone()),
            store2.claim_batch(cfg.clone())
        );
        let a = a.unwrap();
        let b = b.unwrap();

        let mut seen: HashMap<Uuid, usize> = HashMap::new();
        for c in a.iter().chain(b.iter()) {
            *seen.entry(c.object.id).or_insert(0) += 1;
        }
        for (id, count) in &seen {
            assert_eq!(
                *count, 1,
                "round {round}: object {id} was claimed {count} times — claims must be exclusive"
            );
        }
        for id in &seeded {
            assert!(
                seen.contains_key(id),
                "round {round}: every seeded row must be claimed exactly once between the two replicas"
            );
        }
        assert_eq!(seen.len(), N);
    }

    pool2.close().await;
    h.finish().await;
}

/// Behaviour 12: a crashed attempt (claim dropped without completing, lease
/// expired) is adopted by a running worker and driven to convergence.
#[tokio::test]
async fn crash_mid_reconcile_successor_redrives() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 5,
            content: "orphan".into(),
        })
        .await
        .unwrap();

    // Claim then "crash": drop the claim without completing, and collapse
    // the lease so a successor can adopt without waiting it out.
    let claims = h.claim_batch().await.unwrap();
    assert_eq!(claims.len(), 1);
    drop(claims);
    h.force_expire_claim(&r).await.unwrap();

    let replica = h
        .start_worker(h.example_reconciler(), NoAfterComplete)
        .unwrap();
    let obj = h.wait_for(&r, WAIT, reconciled).await.unwrap();
    assert_eq!(obj.status.provisioned_widgets, 5);

    replica.stop().await;
    h.finish().await;
}

/// Behaviour 14: a panicking reconcile pass fails closed to a loud `Retry`
/// (the worker's panic recovery), the worker keeps running, and unrelated
/// objects still reconcile.
#[tokio::test]
async fn panic_fails_closed_to_retry() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let rec = h.example_reconciler().before(hook(|_, c| {
        Box::pin(async move {
            if c.object.spec.content == "panic" {
                panic!("boom inside reconcile");
            }
            Ok(())
        })
    }));
    let replica = h.start_worker(rec, NoAfterComplete).unwrap();

    let r_panic = h
        .create(Spec {
            widgets: 1,
            content: "panic".into(),
        })
        .await
        .unwrap();
    let r_ok = h
        .create(Spec {
            widgets: 2,
            content: "ok".into(),
        })
        .await
        .unwrap();

    // The worker survived the panic: an unrelated object still reconciles.
    let ok = h.wait_for(&r_ok, WAIT, reconciled).await.unwrap();
    assert_eq!(ok.status.provisioned_widgets, 2);

    // The panicking object is left retrying with the panic recorded.
    let panicked = h
        .wait_for(&r_panic, WAIT, |o| {
            o.meta.phase == Phase::Retrying && o.meta.attempts >= 1
        })
        .await
        .unwrap();
    assert!(
        panicked.meta.last_error.contains("reconciler panic"),
        "{}",
        panicked.meta.last_error
    );
    assert!(
        panicked.meta.last_error.contains("boom inside reconcile"),
        "{}",
        panicked.meta.last_error
    );

    replica.stop().await;
    h.finish().await;
}

/// Behaviour 15: a worker only claims as many objects as it has free
/// slots. With parallelism 2 and gate-blocked reconcilers, no more than 2
/// rows are ever claimed at once; the rest wait for a slot to free.
#[tokio::test]
async fn worker_claims_only_free_slots() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let gate = Gate::new();
    let rec = h.example_reconciler().before(gate.clone());
    let mut cfg = h.config();
    cfg.parallelism = 2;
    // A long attempt timeout so the two gated attempts stay claimed for the
    // whole assertion window instead of timing out and churning the claim.
    cfg.attempt_timeout = Duration::from_secs(150);
    let replica = h.start_worker_config(cfg, rec, NoAfterComplete).unwrap();

    const N: usize = 4;
    let mut refs = Vec::with_capacity(N);
    for i in 0..N {
        refs.push(
            h.create(Spec {
                widgets: i as i32,
                content: format!("slot-{i}"),
            })
            .await
            .unwrap(),
        );
    }

    // Exactly the parallelism worth of rows get claimed while the gate
    // holds.
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if h.claimed_count().await == 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "two rows should be claimed while the gate holds"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // And it never exceeds 2 (2 objects stay unclaimed until a slot frees).
    for _ in 0..10 {
        assert!(
            h.claimed_count().await <= 2,
            "claimed count must never exceed parallelism"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(h.claimed_count().await, 2);

    // Releasing the gate lets everything drain.
    gate.release();
    for r in &refs {
        h.wait_for(r, WAIT, reconciled).await.unwrap();
    }

    replica.stop().await;
    h.finish().await;
}

/// Behaviour 16a: scheduling is poll-first with transactional wakes as a
/// latency hint. A nudge reconciles a due object promptly even under a long
/// poll interval (NOTIFY).
#[tokio::test]
async fn wake_shortens_latency_under_a_long_poll() {
    let mut cfg = fast_config();
    cfg.poll_interval = Duration::from_secs(60); // polling alone would never fire in-window
    let Some(h) = Harness::from_env_config(cfg).await else {
        return;
    };

    let passes = Arc::new(AtomicI64::new(0));
    let counter = Arc::clone(&passes);
    let rec = h.example_reconciler().before(hook(move |_, _| {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }));

    // Create before starting the worker so the FIRST convergence is driven
    // by the worker's initial scan (deterministic), not the create-time
    // NOTIFY (which would race the wake listener's startup under a 60 s
    // poll).
    let r = h
        .create(Spec {
            widgets: 1,
            content: "wake".into(),
        })
        .await
        .unwrap();
    let replica = h.start_worker(rec, NoAfterComplete).unwrap();
    h.wait_for(&r, WAIT, reconciled).await.unwrap();

    // Let the wake listener settle so the nudge's NOTIFY is delivered.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = passes.load(Ordering::SeqCst);
    h.store.nudge(&r).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while passes.load(Ordering::SeqCst) <= before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a nudge must re-reconcile promptly via NOTIFY, far under the 60 s poll"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    replica.stop().await;
    h.finish().await;
}

/// Behaviour 16b: polling alone still converges a due object with no wake
/// at all.
#[tokio::test]
async fn poll_converges_a_due_object_with_no_wake() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let replica = h
        .start_worker(h.example_reconciler(), NoAfterComplete)
        .unwrap();

    // Insert envelope + typed rows directly, atomically in one transaction
    // (no store, so no pg_notify wake fires, and the worker never observes
    // a bare envelope). The worker must find this object by POLLING alone.
    let id = Uuid::new_v4();
    let mut tx = h.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO processing_object_conformance
             (processing_object_type_key, id, external_id, name, namespace)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(TYPE_KEY)
    .bind(id)
    .bind(h.store.public_id(id))
    .bind(identity_name(id))
    .bind(h.namespace)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO conformance_spec (id, widgets, content) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(4)
        .bind("polled")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO conformance_status (id, provisioned_widgets, external_id) VALUES ($1, $2, $3)",
    )
    .bind(id)
    .bind(0)
    .bind("")
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let obj = h
        .wait_for(&h.store.r#ref(id), WAIT, reconciled)
        .await
        .unwrap();
    assert_eq!(obj.status.provisioned_widgets, 4);

    replica.stop().await;
    h.finish().await;
}

/// Behaviour 17: an `after_complete` callback that panics is contained — it
/// neither crashes nor wedges the worker, and subsequent objects still
/// reconcile.
#[tokio::test]
async fn after_complete_containment() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let after = |_: &Ctx, _: Object<Spec, Status>, _: Completion<Status>| async {
        panic!("after-complete boom");
    };
    let replica = h.start_worker(h.example_reconciler(), after).unwrap();

    let r1 = h
        .create(Spec {
            widgets: 1,
            content: "a1".into(),
        })
        .await
        .unwrap();
    let r2 = h
        .create(Spec {
            widgets: 2,
            content: "a2".into(),
        })
        .await
        .unwrap();

    h.wait_for(&r1, WAIT, reconciled).await.unwrap();
    h.wait_for(&r2, WAIT, reconciled)
        .await
        .expect("the worker must survive a panicking after_complete and keep reconciling");

    replica.stop().await;
    h.finish().await;
}

/// Shutdown drains: a replica stopped while a pass is held at the gate
/// completes that attempt as a retry (the pass is cancelled, nothing is
/// abandoned to lease expiry), and a fresh replica then converges it.
#[tokio::test]
async fn stop_drains_in_flight_attempts_as_retries() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let gate = Gate::new();
    let rec = h.example_reconciler().before(gate.clone());
    let replica = h.start_worker(rec, NoAfterComplete).unwrap();
    let r = h
        .create(Spec {
            widgets: 3,
            content: "drain".into(),
        })
        .await
        .unwrap();
    gate.wait_entered().await;

    replica.stop().await;
    let env = h.envelope(&r).await;
    assert!(
        env.claim_token.is_none(),
        "the cancelled attempt completed and released the claim"
    );
    assert_eq!(env.phase, "retrying");
    assert!(env.last_error.contains("cancelled"), "{}", env.last_error);

    let replica = h
        .start_worker(h.example_reconciler(), NoAfterComplete)
        .unwrap();
    let obj = h.wait_for(&r, WAIT, reconciled).await.unwrap();
    assert_eq!(obj.status.provisioned_widgets, 3);

    replica.stop().await;
    h.finish().await;
}
