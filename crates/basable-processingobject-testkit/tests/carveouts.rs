//! The claim-path carve-outs, ported from
//! `processingobject_carveouts_test.go`: label routing across logical
//! workers, the unroutable unlabelled row, the adoption flag on a claim,
//! and the status-sighted compare-and-set of `update_spec` — against a
//! committed status and against an attempt still in flight. (The label
//! check on an ambiguous create retry lives in `store.rs`.)

use std::collections::HashSet;

use basable_core::labels::Labels;
use basable_processingobject::{CreateOptions, Error, Outcome, WorkerConfig};
use basable_processingobject_testkit::{Harness, Spec, Status};
use uuid::Uuid;

fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Creates a conformance object carrying labels.
async fn create_labelled(h: &Harness, labels: Labels) -> Uuid {
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
            CreateOptions::labels(labels),
        )
        .await
        .unwrap();
    id
}

/// The harness policy narrowed to one logical worker.
fn selector_config(h: &Harness, selector: Labels) -> WorkerConfig {
    let mut cfg = h.config();
    cfg.label_selector = selector;
    cfg
}

/// Two disjoint selectors over one type each claim exactly their own
/// labelled slice — the split that lets cloud and bare-metal server rows
/// run under different lease policies without duplicating the type.
#[tokio::test]
async fn label_routing() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let mut cloud = HashSet::new();
    let mut robot = HashSet::new();
    for _ in 0..3 {
        cloud.insert(create_labelled(&h, labels(&[("infra_type", "cloud")])).await);
    }
    for _ in 0..2 {
        robot.insert(create_labelled(&h, labels(&[("infra_type", "bare_metal")])).await);
    }

    let claims = h
        .store
        .claim_batch(selector_config(&h, labels(&[("infra_type", "cloud")])))
        .await
        .unwrap();
    assert_eq!(
        claims.len(),
        3,
        "the cloud worker claims exactly the cloud slice"
    );
    for c in &claims {
        assert!(
            cloud.contains(&c.object.id),
            "claimed {} is not a cloud object",
            c.object.id
        );
        assert!(!c.adopted(), "a fresh claim is not a takeover");
    }

    let claims = h
        .store
        .claim_batch(selector_config(&h, labels(&[("infra_type", "bare_metal")])))
        .await
        .unwrap();
    assert_eq!(
        claims.len(),
        2,
        "the bare-metal worker claims exactly its slice"
    );
    for c in &claims {
        assert!(robot.contains(&c.object.id));
    }
    h.finish().await;
}

/// The dead-row scenario the covering obligation exists for: an object
/// created with NO labels matches no filtered worker and is claimed by
/// neither; only an unfiltered worker picks it up. (The framework's WARN
/// canary fires on the empty filtered scans; a label-routed component
/// prevents the row at its create sites — a partition CHECK cannot serve,
/// since the boot probe inserts a bare envelope row.)
#[tokio::test]
async fn unlabelled_row_is_unroutable() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: String::new(),
        })
        .await
        .unwrap(); // no labels

    for selector in [
        labels(&[("infra_type", "cloud")]),
        labels(&[("infra_type", "bare_metal")]),
    ] {
        let claims = h
            .store
            .claim_batch(selector_config(&h, selector))
            .await
            .unwrap();
        assert!(
            claims.is_empty(),
            "a filtered worker must never claim an unlabelled row"
        );
    }

    let claims = h.store.claim_batch(h.config()).await.unwrap();
    assert_eq!(claims.len(), 1, "the unfiltered worker claims it");
    assert_eq!(claims[0].object.id, r.id);
    h.finish().await;
}

/// A fresh claim reports `adopted` false; the claim that replaces an
/// expired lease reports true — the signal a successor uses to re-anchor
/// wall-clock budgets instead of billing the predecessor's stall to the
/// object.
#[tokio::test]
async fn claim_adopted() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: String::new(),
        })
        .await
        .unwrap();

    let claims = h.claim_batch().await.unwrap();
    assert_eq!(claims.len(), 1);
    assert!(
        !claims[0].adopted(),
        "an unclaimed object is claimed, not adopted"
    );
    drop(claims);

    // The holder dies without completing; its lease is force-expired and
    // the successor's claim is a takeover.
    h.force_expire_claim(&r).await.unwrap();
    let claims = h.claim_batch().await.unwrap();
    assert_eq!(claims.len(), 1);
    assert!(
        claims[0].adopted(),
        "replacing an expired lease is a takeover"
    );
    h.finish().await;
}

/// The `update_spec` closure decides on the last committed status under
/// the envelope lock. A rejection writes nothing and advances nothing; an
/// acceptance writes the spec and advances the generation. (Go proved that
/// a write to the status COPY inside the closure is discarded; here the
/// closure sees `&Status`, so the write does not compile.)
#[tokio::test]
async fn status_sighted_cas() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 3,
            content: String::new(),
        })
        .await
        .unwrap();

    // Commit a status through a real fenced completion first, so the CAS
    // has something committed to see.
    h.drive_once(|_| {
        Box::pin(async move {
            Ok(Outcome::converged(Some(Status {
                provisioned_widgets: 3,
                external_id: String::new(),
            })))
        })
    })
    .await
    .unwrap();
    let before = h.meta(&r).await.unwrap();

    // Reject on the status predicate: nothing written, generation unchanged.
    let err = h
        .store
        .update_spec(&r, |s, st| {
            if st.provisioned_widgets != 0 {
                return Err("not claimable".into());
            }
            s.widgets = 99;
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Mutate(e) if e.to_string() == "not claimable"),
        "the closure's rejection surfaces as Mutate: {err}"
    );
    let after = h.meta(&r).await.unwrap();
    assert_eq!(
        before.generation, after.generation,
        "a rejected CAS advances nothing"
    );
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.spec.widgets, 3, "a rejected CAS writes nothing");

    // Accept on the status predicate.
    h.store
        .update_spec(&r, |s, st| {
            if st.provisioned_widgets != 3 {
                return Err("not claimable".into());
            }
            s.widgets = 5;
            Ok(())
        })
        .await
        .unwrap();
    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(obj.spec.widgets, 5, "the accepted CAS wrote the spec");
    assert_eq!(obj.status.provisioned_widgets, 3, "the status is untouched");
    let after = h.meta(&r).await.unwrap();
    assert_eq!(
        before.generation + 1,
        after.generation,
        "the accepted CAS is new intent"
    );
    h.finish().await;
}

/// A status-gated mutation admitted while an attempt is RUNNING does not
/// race it — the attempt's completion commits its observation but is
/// generation-stale, so the object is left due and the next pass runs
/// against the new spec.
#[tokio::test]
async fn status_sighted_cas_vs_in_flight_attempt() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 2,
            content: String::new(),
        })
        .await
        .unwrap();

    // Claim and hold the attempt open (the reconciler is "running").
    let mut claims = h.claim_batch().await.unwrap();
    assert_eq!(claims.len(), 1);
    let claim = claims.remove(0);

    // The CAS lands mid-attempt: it sees the last COMMITTED status (zero —
    // the running attempt has committed nothing), accepts, and advances the
    // generation.
    h.store
        .update_spec(&r, |s, st| {
            if st.provisioned_widgets != 0 {
                return Err("unexpected committed status".into());
            }
            s.widgets = 7;
            Ok(())
        })
        .await
        .unwrap();

    // The in-flight attempt completes afterwards: its observation commits,
    // but the claim-time generation is stale, so the object stays due for
    // an immediate re-pass against the new spec.
    let done = claim
        .complete(Ok(Outcome::converged(Some(Status {
            provisioned_widgets: 2,
            external_id: String::new(),
        }))))
        .await
        .unwrap();
    assert!(done.superseded(), "the completion lost the generation race");

    let meta = h.meta(&r).await.unwrap();
    assert!(!meta.observed_current(), "the new intent is unobserved");
    let claims = h.claim_batch().await.unwrap();
    assert_eq!(
        claims.len(),
        1,
        "the object is immediately claimable for the new spec"
    );
    assert_eq!(claims[0].object.spec.widgets, 7);
    h.finish().await;
}
