//! Two buses over one database, the port of pubsub_test.go: a publish
//! reaches the sibling and not the publisher, a transactional publish
//! with `IncludeSelf` reaches both, a rolled-back one reaches nobody, and
//! a killed listen connection comes back with the reconnect hook fired.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use basable_core::Ctx;
use basable_pubsub::{Bus, Delivery, Message};
use basable_testkit::TestDb;
use sqlx::PgPool;

struct Replica {
    bus: Arc<Bus>,
    seen: Arc<Mutex<Vec<Message>>>,
    reconnects: Arc<Mutex<u32>>,
    ctx: Ctx,
    task: tokio::task::JoinHandle<()>,
}

async fn replica(pool: PgPool, deliver_self: bool) -> Replica {
    let mut bus = Bus::new(pool);
    if deliver_self {
        bus = bus.deliver_to_self();
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    bus.subscribe("events", move |m| s.lock().unwrap().push(m))
        .unwrap();
    let reconnects = Arc::new(Mutex::new(0));
    let r = reconnects.clone();
    bus.on_reconnect(move || *r.lock().unwrap() += 1).unwrap();
    let bus = Arc::new(bus);
    let ctx = Ctx::background();
    let task = {
        let bus = bus.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move { bus.run(ctx).await })
    };
    // A publish before the LISTEN is in place is lost.
    tokio::time::timeout(Duration::from_secs(10), bus.listening())
        .await
        .expect("the bus started listening");
    Replica {
        bus,
        seen,
        reconnects,
        ctx,
        task,
    }
}

impl Replica {
    async fn stop(self) {
        self.ctx.cancel();
        let _ = self.task.await;
    }

    async fn wait_seen(&self, n: usize) -> Vec<Message> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let got = self.seen.lock().unwrap().clone();
            if got.len() >= n {
                return got;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "saw {} of {n} messages",
                got.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn a_publish_reaches_the_sibling_and_not_the_publisher() {
    let Some(db) = TestDb::from_env_with_migrations(&basable_testkit::runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await
    else {
        return;
    };
    let a = replica(db.migrator().clone(), false).await;
    let b = replica(db.migrator().clone(), false).await;
    settle().await;

    a.bus.publish("events", "one").await.unwrap();
    let got = b.wait_seen(1).await;
    assert_eq!(got[0].data, "one");
    assert_eq!(got[0].origin, a.bus.instance_id());
    assert_eq!(got[0].channel, "events");
    settle().await;
    assert!(
        a.seen.lock().unwrap().is_empty(),
        "the publisher saw its own message"
    );

    // A channel nobody subscribed to is silently dropped by Postgres.
    a.bus.publish("nobody", "x").await.unwrap();
    settle().await;
    assert_eq!(b.seen.lock().unwrap().len(), 1);

    a.stop().await;
    b.stop().await;
    db.finish().await;
}

#[tokio::test]
async fn deliver_to_self_and_include_self_reach_the_publisher_too() {
    let Some(db) = TestDb::from_env_with_migrations(&basable_testkit::runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await
    else {
        return;
    };
    let a = replica(db.migrator().clone(), true).await;
    let b = replica(db.migrator().clone(), false).await;
    settle().await;

    a.bus.publish("events", "loud").await.unwrap();
    assert_eq!(a.wait_seen(1).await[0].data, "loud");
    assert_eq!(b.wait_seen(1).await[0].data, "loud");

    // A committed transactional publish with IncludeSelf reaches b's own
    // handlers although b does not deliver to itself.
    let mut tx = db.migrator().begin().await.unwrap();
    b.bus
        .publish_tx(&mut tx, "events", "committed", Delivery::IncludeSelf)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let got = b.wait_seen(2).await;
    assert_eq!(got[1].data, "committed");
    assert_eq!(got[1].origin, "00000000-0000-0000-0000-000000000000");

    // A rolled-back one reaches nobody.
    let mut tx = db.migrator().begin().await.unwrap();
    b.bus
        .publish_tx(&mut tx, "events", "rolled back", Delivery::IncludeSelf)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    settle().await;
    assert_eq!(b.seen.lock().unwrap().len(), 2);
    assert_eq!(a.seen.lock().unwrap().len(), 2);

    a.stop().await;
    b.stop().await;
    db.finish().await;
}

#[tokio::test]
async fn a_killed_listen_connection_comes_back_and_fires_the_hook() {
    let Some(db) = TestDb::from_env_with_migrations(&basable_testkit::runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await
    else {
        return;
    };
    let a = replica(db.migrator().clone(), false).await;
    let b = replica(db.migrator().clone(), false).await;
    settle().await;
    a.bus.publish("events", "before").await.unwrap();
    b.wait_seen(1).await;

    // Kill every backend that ran LISTEN on this database (both buses).
    let killed: Vec<(bool,)> = sqlx::query_as(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
         WHERE datname = current_database() AND query LIKE 'LISTEN%'",
    )
    .fetch_all(db.superuser())
    .await
    .unwrap();
    assert!(!killed.is_empty(), "no listen backends found");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while *b.reconnects.lock().unwrap() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "b never reconnected"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    settle().await;
    a.bus.publish("events", "after").await.unwrap();
    let got = b.wait_seen(2).await;
    assert_eq!(got[1].data, "after");

    a.stop().await;
    b.stop().await;
    db.finish().await;
}
