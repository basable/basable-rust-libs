//! The reference audits: three adapters over the conformance `WidgetSim`,
//! one per strategy family, through the real HTTP client and the ack-loss
//! proxy. They are what a component's own audit file looks like — and they
//! pin the kit itself: a deliberately broken adapter must fail its audit.

use std::sync::Arc;
use std::time::Duration;

use basable_effecttest::{AckLossProxy, Harness, audit, try_run};
use basable_externaleffect::{
    Adapter, Call, Classifier, DeclaredResolution, LateCall, SlotIdentity, Strategy, intent_age_fn,
    key_fn, lookup_fn, provider_id_fn, resolve_by_lookup, resolve_keyed_fn, send_fn,
};
use basable_processingobject_testkit::{Order, Spec, WidgetClient, WidgetSim};

/// The dispatch payload of the widget upsert: the receiver identity and the
/// desired content.
#[derive(Debug, Clone)]
struct WidgetArgs {
    key: String,
    spec: Spec,
}

/// The dispatch payload of an order: its idempotency key, the payload, and
/// how old the durable intent behind it is.
#[derive(Debug, Clone)]
struct OrderArgs {
    key: String,
    widgets: i32,
    age: Duration,
}

const WINDOW: Duration = Duration::from_secs(60 * 60);

/// Idempotent + Convergent: the widget upsert converges at the receiver.
fn upsert_widget(client: WidgetClient) -> Call<WidgetArgs, WidgetArgs, ()> {
    Call::new(Adapter {
        operation: "upsert_widget".into(),
        key: Some(key_fn(|a: &WidgetArgs| a.key.clone())),
        send: send_fn(move |_ctx, a: WidgetArgs| {
            let client = client.clone();
            async move { client.upsert(&a.key, &a.spec).await }
        }),
        classify: Some(Classifier::transport()),
        call_timeout: Duration::from_secs(2),
        irreversible: false,
        late_call: LateCall::Convergent,
        strategy: Strategy::Idempotent,
    })
    .expect("a valid adapter")
}

/// KeyedReplay + irreversible: the order is minted once per key, replayed
/// within the window, and read by its persisted id forever after.
fn place_order(client: WidgetClient) -> Call<OrderArgs, OrderArgs, Order> {
    let resolve_client = client.clone();
    Call::new(Adapter {
        operation: "place_order".into(),
        key: Some(key_fn(|a: &OrderArgs| a.key.clone())),
        send: send_fn(move |_ctx, a: OrderArgs| {
            let client = client.clone();
            async move { client.place_order(&a.key, a.widgets).await }
        }),
        classify: Some(Classifier::fail_closed_on_definitive()),
        call_timeout: Duration::from_secs(2),
        irreversible: true,
        late_call: LateCall::KeyScoped,
        strategy: Strategy::KeyedReplay {
            window: WINDOW,
            intent_age: intent_age_fn(|a: &OrderArgs| a.age),
            provider_id: provider_id_fn(|o: &Order| o.id.clone()),
            resolve: resolve_keyed_fn(move |_ctx, id: String| {
                let client = resolve_client.clone();
                async move { client.get_order(&id).await }
            }),
        },
    })
    .expect("a valid adapter")
}

/// Declared { Keyed } + irreversible: the order is declared on a slot under
/// its key before the send, and a slot found on claim is settled by the
/// receipt the receiver holds under exactly that key. The resolve identity
/// is the key alone (`RA = String`), not the dispatch payload.
fn declare_order(client: WidgetClient) -> Call<OrderArgs, String, Order> {
    let resolve_client = client.clone();
    Call::new(Adapter {
        operation: "declare_order".into(),
        key: Some(key_fn(|a: &OrderArgs| a.key.clone())),
        send: send_fn(move |_ctx, a: OrderArgs| {
            let client = client.clone();
            async move { client.place_order(&a.key, a.widgets).await }
        }),
        classify: Some(Classifier::fail_closed_on_definitive()),
        call_timeout: Duration::from_secs(2),
        irreversible: true,
        late_call: LateCall::KeyScoped,
        strategy: Strategy::Declared {
            slot_identity: SlotIdentity::Keyed,
            resolution: DeclaredResolution::Resolve(resolve_by_lookup(lookup_fn(
                move |_ctx, key: String| {
                    let client = resolve_client.clone();
                    async move { client.find_order_by_key(&key).await }
                },
            ))),
        },
    })
    .expect("a valid adapter")
}

/// A fresh simulator behind a fresh proxy, and the client through it.
async fn fixture() -> (Arc<WidgetSim>, Arc<AckLossProxy>, WidgetClient) {
    let sim = Arc::new(WidgetSim::start().await);
    let proxy = Arc::new(AckLossProxy::start(sim.url()).await);
    let client = sim.client_via(proxy.url());
    (sim, proxy, client)
}

async fn upsert_widget_harness() -> Harness<WidgetArgs, WidgetArgs, ()> {
    let (sim, proxy, client) = fixture().await;
    let key = "conformance/reference-widget".to_owned();
    let counted = Arc::clone(&sim);
    let faulted = Arc::clone(&sim);
    Harness::new(
        Arc::new(upsert_widget(client)),
        {
            let key = key.clone();
            move || WidgetArgs {
                key: key.clone(),
                spec: Spec {
                    widgets: 1,
                    content: "v1".into(),
                },
            }
        },
        move || {
            let sim = Arc::clone(&counted);
            let key = key.clone();
            async move { sim.count(&key) as usize }
        },
        move || {
            let sim = Arc::clone(&faulted);
            async move { sim.fail_next() }
        },
    )
    // A newer payload against the same receiver identity.
    .advance_intent(|mut a| {
        a.spec.content = "v2".into();
        a
    })
    .inject_ambiguity(move || {
        let proxy = Arc::clone(&proxy);
        async move { proxy.arm_next() }
    })
}

audit!(upsert_widget_audit, upsert_widget_harness);

fn order_args() -> OrderArgs {
    OrderArgs {
        key: "intent-1".into(),
        widgets: 3,
        age: Duration::from_secs(60),
    }
}

async fn place_order_harness() -> Harness<OrderArgs, OrderArgs, Order> {
    let (sim, proxy, client) = fixture().await;
    let counted = Arc::clone(&sim);
    let faulted = Arc::clone(&sim);
    Harness::new(
        Arc::new(place_order(client)),
        order_args,
        move || {
            let sim = Arc::clone(&counted);
            async move { sim.order_count() }
        },
        move || {
            let sim = Arc::clone(&faulted);
            async move { sim.reject_next() }
        },
    )
    // The next intent: a new key.
    .advance_intent(|mut a| {
        a.key = "intent-2".into();
        a
    })
    // The same intent, seen once the replay window has elapsed.
    .past_window(|mut a| {
        a.age = WINDOW;
        a
    })
    .inject_ambiguity(move || {
        let proxy = Arc::clone(&proxy);
        async move { proxy.arm(|head| head.method == "POST" && head.path == "/orders", 0) }
    })
}

audit!(place_order_audit, place_order_harness);

async fn declare_order_harness() -> Harness<OrderArgs, String, Order> {
    let (sim, proxy, client) = fixture().await;
    let counted = Arc::clone(&sim);
    let faulted = Arc::clone(&sim);
    Harness::new(
        Arc::new(declare_order(client)),
        order_args,
        move || {
            let sim = Arc::clone(&counted);
            async move { sim.order_count() }
        },
        move || {
            let sim = Arc::clone(&faulted);
            async move { sim.reject_next() }
        },
    )
    .advance_intent(|mut a| {
        a.key = "intent-2".into();
        a
    })
    .resolve_args(|a| a.key)
    .inject_ambiguity(move || {
        let proxy = Arc::clone(&proxy);
        async move { proxy.arm(|head| head.method == "POST" && head.path == "/orders", 0) }
    })
}

audit!(declare_order_audit, declare_order_harness);

/// The kit pins the classifier floor: an irreversible adapter on the
/// transport baseline (which consumes a 5xx as "the receiver decided")
/// fails its audit, and says why.
#[tokio::test]
async fn a_misclassified_irreversible_adapter_fails_its_audit() {
    let report = try_run(|| async {
        let (sim, proxy, client) = fixture().await;
        let resolve_client = client.clone();
        let call: Call<OrderArgs, OrderArgs, Order> = Call::new(Adapter {
            operation: "place_order_leaky".into(),
            key: Some(key_fn(|a: &OrderArgs| a.key.clone())),
            send: send_fn(move |_ctx, a: OrderArgs| {
                let client = client.clone();
                async move { client.place_order(&a.key, a.widgets).await }
            }),
            classify: Some(Classifier::transport()),
            call_timeout: Duration::from_secs(2),
            irreversible: true,
            late_call: LateCall::KeyScoped,
            strategy: Strategy::KeyedReplay {
                window: WINDOW,
                intent_age: intent_age_fn(|a: &OrderArgs| a.age),
                provider_id: provider_id_fn(|o: &Order| o.id.clone()),
                resolve: resolve_keyed_fn(move |_ctx, id: String| {
                    let client = resolve_client.clone();
                    async move { client.get_order(&id).await }
                }),
            },
        })
        .expect("a valid adapter");
        let counted = Arc::clone(&sim);
        let faulted = Arc::clone(&sim);
        Harness::new(
            Arc::new(call),
            order_args,
            move || {
                let sim = Arc::clone(&counted);
                async move { sim.order_count() }
            },
            move || {
                let sim = Arc::clone(&faulted);
                async move { sim.reject_next() }
            },
        )
        .advance_intent(|mut a| {
            a.key = "intent-2".into();
            a
        })
        .past_window(|mut a| {
            a.age = WINDOW;
            a
        })
        .inject_ambiguity(move || {
            let proxy = Arc::clone(&proxy);
            async move { proxy.arm_next() }
        })
    })
    .await;
    let failures = report.failures();
    assert_eq!(
        failures.len(),
        1,
        "exactly the classifier floor fails: {report}"
    );
    assert_eq!(failures[0].0, "classifier_floor");
    assert!(failures[0].1.contains("plain error definitive"), "{report}");
}

/// The kit pins the reversible default: an adapter without ack-loss
/// injection is audited with its ack-loss probe skipped loudly, never
/// silently passed.
#[tokio::test]
async fn a_reversible_adapter_without_injection_skips_the_ack_loss_probe() {
    let report = try_run(|| async {
        let sim = Arc::new(WidgetSim::start().await);
        let client = sim.client();
        let counted = Arc::clone(&sim);
        let faulted = Arc::clone(&sim);
        let key = "conformance/no-injection".to_owned();
        Harness::new(
            Arc::new(upsert_widget(client)),
            {
                let key = key.clone();
                move || WidgetArgs {
                    key: key.clone(),
                    spec: Spec {
                        widgets: 1,
                        content: "v1".into(),
                    },
                }
            },
            move || {
                let sim = Arc::clone(&counted);
                let key = key.clone();
                async move { sim.count(&key) as usize }
            },
            move || {
                let sim = Arc::clone(&faulted);
                async move { sim.fail_next() }
            },
        )
        .advance_intent(|mut a| {
            a.spec.content = "v2".into();
            a
        })
    })
    .await;
    assert!(report.is_ok(), "{report}");
    let skipped = report.skipped();
    assert_eq!(skipped.len(), 1, "{report}");
    assert_eq!(skipped[0].0, "ack_loss_then_retry_lands_once");
}
