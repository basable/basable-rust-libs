//! Re-entrancy over the generated traits: `a → b → a → …` as plain nested
//! async calls (the boxed back-edge on the heap), under a multi-thread
//! runtime with many requests in flight; and the fail-fast error fan-out
//! in declaration order (the port of error_fanout_test.go's assertions at
//! run time).

use std::sync::{Arc, Mutex};

use basable_core::{Code, Ctx};
use interfaces::ApiSender;
use messages::*;
use messenger::AppMessenger;

fn router() -> (&'static AppMessenger, Arc<Mutex<Vec<String>>>) {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let router: &'static AppMessenger = Box::leak(Box::new(AppMessenger::new(
        api::Api::new(),
        a::A::new(trace.clone()),
        b::B::new(trace.clone()),
    )));
    (router, trace)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_cycle_is_a_nested_call_chain() {
    let (router, _) = router();
    let ctx = Ctx::background();
    // depth 0: a answers alone. depth n: a → b → a … with n Pongs and n
    // more Pings, 2n + 1 handler calls.
    for depth in [0u32, 1, 2, 10, 200] {
        let count = ApiSender::new(router)
            .send_ping(&ctx, Ping { depth })
            .await
            .unwrap();
        assert_eq!(count.hops, 2 * depth + 1, "depth {depth}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_re_entrant_requests_share_one_router() {
    let (router, _) = router();
    let tasks: Vec<_> = (0..64u32)
        .map(|i| {
            tokio::spawn(async move {
                let ctx = Ctx::background();
                ApiSender::new(router)
                    .send_ping(&ctx, Ping { depth: i })
                    .await
                    .unwrap()
                    .hops
            })
        })
        .collect();
    for (i, t) in tasks.into_iter().enumerate() {
        assert_eq!(t.await.unwrap(), 2 * i as u32 + 1);
    }
}

#[tokio::test]
async fn an_error_fanout_calls_handlers_in_declaration_order_and_stops_at_the_first_error() {
    let (router, trace) = router();
    let ctx = Ctx::background();
    let sender = ApiSender::new(router);
    let send = |fail_at| sender.send_event(&ctx, Event { fail_at });

    send(None).await.unwrap();
    assert_eq!(trace.lock().unwrap().clone(), vec!["a", "b"]);

    trace.lock().unwrap().clear();
    let err = send(Some("a")).await.unwrap_err();
    assert_eq!(err.code(), Code::Internal);
    assert_eq!(err.message(), "a failed");
    assert!(trace.lock().unwrap().is_empty(), "b ran after a failed");

    trace.lock().unwrap().clear();
    let err = send(Some("b")).await.unwrap_err();
    assert_eq!(err.message(), "b failed");
    assert_eq!(trace.lock().unwrap().clone(), vec!["a"]);
}
