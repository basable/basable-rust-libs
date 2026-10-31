//! The bus across replicas over a real database, the port of the
//! monorepo's `golang/test/pubsub_integration_test.go`. Each `Bus` here is
//! one replica, registered in the directory as the `app` login (the
//! framework pool's) with its own listener on localhost; a publish travels
//! from one to another over their own connection.
//!
//! The timings are short so the sweep and the re-registration happen within
//! a test: a heartbeat every 200ms, swept after a second.

use std::time::Duration;

use basable_core::Ctx;
use basable_pubsub::{Bus, Config, Message};
use basable_testkit::TestDb;
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

fn config() -> Config {
    Config {
        listen_address: "127.0.0.1:0".to_string(),
        advertise_host: None,
        heartbeat_interval: Duration::from_millis(200),
        expiry: Duration::from_secs(1),
        send_timeout: Duration::from_secs(2),
    }
}

async fn database() -> Option<(TestDb, PgPool)> {
    let db = TestDb::from_env_with_migrations(&basable_testkit::runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await?;
    let pool = PgPool::connect_with(db.app_options())
        .await
        .expect("a pool as the app login");
    Some((db, pool))
}

/// A registered replica that is never run: it subscribes and publishes, but
/// serves no peer and never heartbeats.
async fn registered(pool: &PgPool) -> Bus {
    Bus::new(pool.clone(), config())
        .await
        .expect("the replica registers")
}

struct Running {
    bus: Bus,
    ctx: Ctx,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    async fn stop(self) {
        self.ctx.cancel();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("the bus stops within 10s of its context ending")
            .expect("the bus task");
    }
}

async fn running(pool: &PgPool) -> Running {
    let bus = registered(pool).await;
    let ctx = Ctx::background();
    let task = {
        let bus = bus.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move { bus.run(ctx).await })
    };
    Running { bus, ctx, task }
}

async fn inbox(bus: &Bus, channel: &str) -> mpsc::UnboundedReceiver<Message> {
    let (tx, rx) = mpsc::unbounded_channel();
    bus.subscribe(channel, move |m| {
        let _ = tx.send(m.clone());
    })
    .await
    .expect("subscribe");
    rx
}

async fn receive(rx: &mut mpsc::UnboundedReceiver<Message>) -> Message {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a message within 5s")
        .expect("the inbox is open")
}

async fn rows(pool: &PgPool, replica: &str) -> (i64, i64) {
    let id = Uuid::parse_str(replica).expect("a replica id");
    let replicas: i64 =
        sqlx::query_scalar("SELECT count(*) FROM basable.pubsub_replica WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("count replicas");
    let subscriptions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM basable.pubsub_subscription WHERE replica_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("count subscriptions");
    (replicas, subscriptions)
}

#[tokio::test]
async fn a_message_reaches_every_subscribed_replica_whole() {
    let Some((db, pool)) = database().await else {
        return;
    };
    let a = running(&pool).await;
    let b = running(&pool).await;
    let c = running(&pool).await;
    let mut at_b = inbox(&b.bus, "orders").await;
    let mut at_c = inbox(&c.bus, "orders").await;

    let payload = b"0123456789abcdef".repeat(64 << 10); // 1 MiB
    assert_eq!(a.bus.publish("orders", &payload).await.unwrap(), 2);
    for inbox in [&mut at_b, &mut at_c] {
        let m = receive(inbox).await;
        assert_eq!(m.channel, "orders");
        assert_eq!(m.origin, a.bus.replica_id());
        assert!(m.data == payload, "the message must arrive whole");
    }

    for r in [a, b, c] {
        r.stop().await;
    }
    pool.close().await;
    db.finish().await;
}

#[tokio::test]
async fn only_the_channels_subscribers_receive_it_the_publisher_included() {
    let Some((db, pool)) = database().await else {
        return;
    };
    let a = running(&pool).await;
    let b = running(&pool).await;
    let c = running(&pool).await;
    let mut at_a = inbox(&a.bus, "orders").await;
    let mut at_b = inbox(&b.bus, "orders").await;
    let mut at_c = inbox(&c.bus, "invoices").await;

    assert_eq!(a.bus.publish("orders", b"o-1").await.unwrap(), 2);
    assert_eq!(receive(&mut at_a).await.data, b"o-1");
    assert_eq!(receive(&mut at_b).await.data, b"o-1");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        at_c.try_recv().is_err(),
        "a replica subscribed to another channel got the message"
    );
    assert_eq!(a.bus.publish("nobody", b"x").await.unwrap(), 0);

    for r in [a, b, c] {
        r.stop().await;
    }
    pool.close().await;
    db.finish().await;
}

#[tokio::test]
async fn a_publishers_messages_arrive_in_order() {
    let Some((db, pool)) = database().await else {
        return;
    };
    let a = running(&pool).await;
    let b = running(&pool).await;
    let mut at_b = inbox(&b.bus, "seq").await;

    for i in 0..200 {
        assert_eq!(
            a.bus
                .publish("seq", i.to_string().as_bytes())
                .await
                .unwrap(),
            1
        );
    }
    for i in 0..200 {
        assert_eq!(receive(&mut at_b).await.data, i.to_string().as_bytes());
    }

    for r in [a, b] {
        r.stop().await;
    }
    pool.close().await;
    db.finish().await;
}

#[tokio::test]
async fn a_replica_that_unsubscribes_or_stops_is_forgotten() {
    let Some((db, pool)) = database().await else {
        return;
    };
    let a = running(&pool).await;
    let b = running(&pool).await;

    let sub = b.bus.subscribe("orders", |_| {}).await.unwrap();
    assert_eq!(a.bus.publish("orders", b"").await.unwrap(), 1);
    sub.unsubscribe().await.unwrap();
    assert_eq!(
        a.bus.publish("orders", b"").await.unwrap(),
        0,
        "b unsubscribed"
    );

    let _at_b = inbox(&b.bus, "invoices").await;
    let b_id = b.bus.replica_id();
    b.stop().await;
    assert_eq!(
        rows(&pool, &b_id).await,
        (0, 0),
        "a stopped replica deregisters"
    );
    assert_eq!(a.bus.publish("invoices", b"").await.unwrap(), 0);

    a.stop().await;
    pool.close().await;
    db.finish().await;
}

#[tokio::test]
async fn a_replica_that_stopped_heartbeating_is_swept() {
    let Some((db, pool)) = database().await else {
        return;
    };
    let a = running(&pool).await;
    let dead = registered(&pool).await; // never run: no heartbeat
    let _at_dead = inbox(&dead, "orders").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while rows(&pool, &dead.replica_id()).await != (0, 0) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the running replica's sweep must delete the dead one"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    a.stop().await;
    pool.close().await;
    db.finish().await;
}

#[tokio::test]
async fn a_replica_swept_while_alive_registers_again() {
    let Some((db, pool)) = database().await else {
        return;
    };
    let a = running(&pool).await;
    let b = running(&pool).await;
    let mut at_a = inbox(&a.bus, "orders").await;
    let (reregistered, mut reregistrations) = mpsc::unbounded_channel();
    a.bus.on_reregister(move || {
        let _ = reregistered.send(());
    });

    sqlx::query("DELETE FROM basable.pubsub_replica WHERE id = $1")
        .bind(Uuid::parse_str(&a.bus.replica_id()).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), reregistrations.recv())
        .await
        .expect("the swept replica registers again")
        .expect("the hook");

    assert_eq!(
        b.bus.publish("orders", b"after").await.unwrap(),
        1,
        "a is subscribed again"
    );
    assert_eq!(receive(&mut at_a).await.data, b"after");

    for r in [a, b] {
        r.stop().await;
    }
    pool.close().await;
    db.finish().await;
}

#[tokio::test]
async fn a_peer_whose_certificate_is_not_listed_is_refused() {
    let Some((db, pool)) = database().await else {
        return;
    };
    let a = running(&pool).await;
    let b = running(&pool).await;
    let _at_a = inbox(&a.bus, "to-a").await;
    let _at_b = inbox(&b.bus, "to-b").await;

    sqlx::query("UPDATE basable.pubsub_replica SET certificate_sha256 = $1 WHERE id = $2")
        .bind(vec![0x42u8; 32])
        .bind(Uuid::parse_str(&b.bus.replica_id()).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        b.bus.publish("to-a", b"x").await.unwrap(),
        0,
        "a must refuse b, whose certificate is not the listed one"
    );
    assert_eq!(
        a.bus.publish("to-b", b"x").await.unwrap(),
        0,
        "a must refuse to publish to b, whose certificate is not the listed one"
    );

    for r in [a, b] {
        r.stop().await;
    }
    pool.close().await;
    db.finish().await;
}
