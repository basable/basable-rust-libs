//! The orderly topology over the generated crates: a 1:1 request chain
//! through three nanoservices, a void fan-out in declaration order, an
//! error crossing a route, the composition root's accessors, and every
//! component's loops through the generated `Components` impl.

use std::sync::{Arc, Mutex};

use basable_app::Components;
use basable_core::{Code, Ctx};
use interfaces::ApiSender;
use messages::*;
use messenger::AppMessenger;

fn router() -> (&'static AppMessenger, Arc<Mutex<Vec<String>>>) {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let router: &'static AppMessenger = Box::leak(Box::new(AppMessenger::new(
        catalog::Catalog::new(trace.clone()),
        order::Order::new(),
        notifier::Notifier::new(trace.clone()),
        api::Api::new(),
    )));
    (router, trace)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_chains_through_three_nanoservices_and_fans_out_in_order() {
    let (router, trace) = router();
    let ctx = Ctx::background();
    let order = router
        .api()
        .ensure_order(router, &ctx, "lamp")
        .await
        .unwrap();
    assert_eq!(
        order,
        Order {
            id: 1,
            product: "lamp".into()
        }
    );
    assert_eq!(
        trace.lock().unwrap().clone(),
        vec![
            "catalog:get:lamp",
            "notifier:send:order 1 for lamp",
            "catalog:event:1",
            "notifier:event:1",
        ]
    );
}

#[tokio::test]
async fn an_error_crosses_a_route_with_its_code() {
    let (router, _) = router();
    let ctx = Ctx::background();
    let err = router
        .api()
        .ensure_order(router, &ctx, "missing")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let err = ApiSender::new(router)
        .send_cancel_order_request(&ctx, CancelOrderRequest { order: 7 })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    assert!(err.message().contains("order 7"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_router_is_shared_by_concurrent_requests() {
    let (router, trace) = router();
    let mut tasks = Vec::new();
    for i in 0..32 {
        tasks.push(tokio::spawn(async move {
            let ctx = Ctx::background();
            router
                .api()
                .ensure_order(router, &ctx, &format!("p{i}"))
                .await
                .unwrap()
        }));
    }
    let mut ids: Vec<u64> = Vec::new();
    for t in tasks {
        ids.push(t.await.unwrap().id);
    }
    ids.sort_unstable();
    assert_eq!(ids, (1..=32).collect::<Vec<_>>());
    assert_eq!(trace.lock().unwrap().len(), 32 * 4);
}

#[test]
fn every_component_hands_over_its_loops_under_its_routing_name() {
    let (router, _) = router();
    let loops: Vec<(&str, String)> = router
        .loops()
        .into_iter()
        .map(|(name, loops)| (name, format!("{loops:?}")))
        .collect();
    // Every component in routing.yaml order, the sends-only api included;
    // only order owns a loop, the others take the default.
    assert_eq!(
        loops,
        vec![
            ("catalog", "[]".to_string()),
            ("order", r#"["sweep_abandoned"]"#.to_string()),
            ("notifier", "[]".to_string()),
            ("api", "[]".to_string()),
        ]
    );
}
