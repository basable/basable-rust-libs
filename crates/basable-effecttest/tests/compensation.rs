//! The reference audit of a compensated `Declared` effect: a card
//! authorization that has no lookup, settled by a reversal its issuer
//! matches to the original even when the reversal arrives first, over an
//! in-process fake of that issuer. It is what a component's own audit of
//! such a pair looks like — the authorization and its reversal are each
//! audited — and it pins the kit: an issuer whose reversal keeps no record
//! fails exactly the dominance probe.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use basable_core::BoxError;
use basable_effecttest::{Harness, audit, try_run};
use basable_externaleffect::{
    Adapter, Call, Classifier, DeclaredResolution, EffectSlot, LateCall, SlotIdentity, Strategy,
    TransportError, compensate_with, definitive, key_fn, send_fn,
};

/// The receiver's own store: the authorizations in force and the keys a
/// reversal has tombstoned, and the one-shot faults the harness arms.
#[derive(Default)]
struct Store {
    in_force: HashSet<String>,
    reversed: HashSet<String>,
    decline_next: bool,
    drop_authorize_ack: bool,
    refuse_next_reverse: bool,
    drop_reverse_ack: bool,
}

/// The issuer. A dominating one refuses an authorization whose key a
/// reversal already named, which is what lets the reversal settle a slot
/// whatever the original does afterwards; a non-dominating one reverses
/// only what it holds and forgets the rest.
struct Issuer {
    dominates: bool,
    store: Mutex<Store>,
}

impl Issuer {
    fn new(dominates: bool) -> Arc<Issuer> {
        Arc::new(Issuer {
            dominates,
            store: Mutex::new(Store::default()),
        })
    }

    fn authorize(&self, key: &str) -> Result<(), BoxError> {
        let mut s = self.store.lock().unwrap();
        if std::mem::take(&mut s.decline_next) {
            return Err(definitive("05 do not honour"));
        }
        if self.dominates && s.reversed.contains(key) {
            return Err(definitive("reversed before it arrived"));
        }
        s.in_force.insert(key.to_owned());
        if std::mem::take(&mut s.drop_authorize_ack) {
            return Err(TransportError::wrap("read: connection reset by peer"));
        }
        Ok(())
    }

    fn reverse(&self, key: &str) -> Result<(), BoxError> {
        let mut s = self.store.lock().unwrap();
        if std::mem::take(&mut s.refuse_next_reverse) {
            return Err("30 format error".into());
        }
        if self.dominates {
            s.reversed.insert(key.to_owned());
        }
        s.in_force.remove(key);
        if std::mem::take(&mut s.drop_reverse_ack) {
            return Err(TransportError::wrap("read: connection reset by peer"));
        }
        Ok(())
    }

    fn in_force(&self) -> usize {
        self.store.lock().unwrap().in_force.len()
    }

    fn reversed(&self) -> usize {
        self.store.lock().unwrap().reversed.len()
    }

    fn arm(&self, f: impl FnOnce(&mut Store)) {
        f(&mut self.store.lock().unwrap());
    }
}

fn authorization_key(intent: &str) -> String {
    format!("auth/{intent}")
}

/// Idempotent + Convergent: reversing one authorization twice is one
/// reversal.
fn reverse_authorization(issuer: Arc<Issuer>) -> Call<String, String, String> {
    Call::new(Adapter {
        operation: "reverse_authorization".into(),
        key: None,
        send: send_fn(move |_ctx, key: String| {
            let outcome = issuer.reverse(&key).map(|()| key);
            async move { outcome }
        }),
        classify: Some(Classifier::transport()),
        call_timeout: Duration::from_secs(1),
        irreversible: false,
        late_call: LateCall::Convergent,
        strategy: Strategy::Idempotent,
    })
    .expect("a valid adapter")
}

/// Declared { Keyed } + irreversible, settled by compensation alone: the
/// issuer offers no lookup, only the reversal.
fn authorize_payment(
    issuer: Arc<Issuer>,
    reverse: Arc<Call<String, String, String>>,
) -> Call<String, String, String> {
    Call::new(Adapter {
        operation: "authorize_payment".into(),
        key: Some(key_fn(|intent: &String| authorization_key(intent))),
        send: send_fn(move |_ctx, intent: String| {
            let key = authorization_key(&intent);
            let outcome = issuer.authorize(&key).map(|()| key);
            async move { outcome }
        }),
        classify: Some(Classifier::fail_closed_on_definitive()),
        call_timeout: Duration::from_secs(1),
        irreversible: true,
        late_call: LateCall::KeyScoped,
        strategy: Strategy::Declared {
            slot_identity: SlotIdentity::Keyed,
            resolution: DeclaredResolution::Compensate {
                resolve: None,
                compensation: compensate_with(reverse, |_intent: String, slot: &EffectSlot| {
                    slot.key.clone()
                }),
            },
        },
    })
    .expect("a valid adapter")
}

fn authorize_harness(dominates: bool) -> Harness<String, String, String> {
    let issuer = Issuer::new(dominates);
    let reverse = Arc::new(reverse_authorization(Arc::clone(&issuer)));
    let (counted, declined, dropped) = (
        Arc::clone(&issuer),
        Arc::clone(&issuer),
        Arc::clone(&issuer),
    );
    Harness::new(
        Arc::new(authorize_payment(issuer, reverse)),
        || "order-1".to_owned(),
        move || {
            let issuer = Arc::clone(&counted);
            async move { issuer.in_force() }
        },
        move || {
            let issuer = Arc::clone(&declined);
            async move { issuer.arm(|s| s.decline_next = true) }
        },
    )
    .advance_intent(|_| "order-2".to_owned())
    .inject_ambiguity(move || {
        let issuer = Arc::clone(&dropped);
        async move { issuer.arm(|s| s.drop_authorize_ack = true) }
    })
}

audit!(authorize_payment_audit, || async {
    authorize_harness(true)
});

audit!(reverse_authorization_audit, || async {
    let issuer = Issuer::new(true);
    let (counted, refused, dropped) = (
        Arc::clone(&issuer),
        Arc::clone(&issuer),
        Arc::clone(&issuer),
    );
    Harness::new(
        Arc::new(reverse_authorization(issuer)),
        || authorization_key("order-1"),
        move || {
            let issuer = Arc::clone(&counted);
            async move { issuer.reversed() }
        },
        move || {
            let issuer = Arc::clone(&refused);
            async move { issuer.arm(|s| s.refuse_next_reverse = true) }
        },
    )
    // A reversal names one authorization: there is no newer intent, only a
    // duplicate.
    .advance_intent(|old| old)
    .inject_ambiguity(move || {
        let issuer = Arc::clone(&dropped);
        async move { issuer.arm(|s| s.drop_reverse_ack = true) }
    })
});

/// The kit pins dominance: an issuer whose reversal of an authorization it
/// does not hold leaves no record lets the late original land live, and
/// the audit says so in exactly one probe.
#[tokio::test]
async fn a_reversal_that_does_not_dominate_fails_its_audit() {
    let report = try_run(|| async { authorize_harness(false) }).await;
    let failures = report.failures();
    assert_eq!(
        failures.len(),
        1,
        "exactly the dominance probe fails: {report}"
    );
    assert_eq!(failures[0].0, "compensation_dominates_late_original");
    assert!(failures[0].1.contains("still in force"), "{report}");
}
