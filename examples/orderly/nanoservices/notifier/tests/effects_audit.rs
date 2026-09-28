//! The audit index: one `audit!` per adapter in effects.rs, against the
//! simulator. Green on day one; the first thing a real provider must keep
//! green. Ack-loss probes are mandatory for irreversible adapters.

use std::sync::Arc;

use basable_effecttest::{Harness, audit};
use notifier::effects::*;
use notifier::simulator::{Fault, Simulator};

audit!(send_email, || async {
    let sim = Arc::new(Simulator::new());
    let calls = Calls::new(sim.clone());
    let landed = sim.clone();
    let definitive = sim.clone();
    let ambiguity = sim.clone();
    Harness::new(
        calls.send_email.clone(),
        || SendEmailArgs {
            key: "send_email-1".into(),
            intent_declared_at: chrono::Utc::now(),
        },
        move || {
            let sim = landed.clone();
            async move { sim.landed_count("send_email") }
        },
        move || {
            let sim = definitive.clone();
            async move { sim.inject("send_email", Fault::Refused, 1) }
        },
    )
    .inject_ambiguity(move || {
        let sim = ambiguity.clone();
        async move { sim.inject("send_email", Fault::AckLoss, 1) }
    })
    // A newer intent is a new key.
    .advance_intent(|a| SendEmailArgs {
        key: format!("{}-next", a.key),
        ..a
    })
    .past_window(|a| SendEmailArgs {
        intent_declared_at: a.intent_declared_at - chrono::Duration::days(2),
        ..a
    })
});
