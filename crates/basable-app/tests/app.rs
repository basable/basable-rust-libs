//! The Phase 8 verification: the boot gate on the migration ledger, two
//! nanoservice pools that cannot read each other's schema, the connection
//! budget, readiness over a live server, the wake bus reaching a worker,
//! and a shutdown that drains an in-flight attempt as a retry.

use std::sync::Arc;
use std::time::Duration;

use basable_app::{App, Config, Error};
use basable_db::{Nanoservice, Stateful};
use basable_processingobject::{NoAfterComplete, TypedStore, Worker};
use basable_processingobject_testkit::{
    Conformance, ExampleReconciler, Gate, Spec, Status, TYPE_NAME, WidgetSim, apply_schema,
    conformance_type, fast_config,
};
use basable_testkit::TestDb;
use sqlx::Row;
use uuid::Uuid;

struct Orders;
impl Nanoservice for Orders {
    const NAME: &'static str = "orders";
}
impl Stateful for Orders {}

struct Inventory;
impl Nanoservice for Inventory {
    const NAME: &'static str = "inventory";
}
impl Stateful for Inventory {}

async fn test_db() -> Option<TestDb> {
    TestDb::from_env_with_migrations(&basable_testkit::runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await
}

fn local_config() -> Config {
    let mut cfg = Config::new("postgres://unused");
    cfg.host = "127.0.0.1".parse().unwrap();
    cfg.port = 0;
    cfg.boot_wait_secs = 2;
    cfg.shutdown_grace_secs = 10;
    cfg
}

#[tokio::test]
async fn boot_refuses_a_ledger_behind_the_binary_and_accepts_one_that_is_current() {
    let Some(db) = test_db().await else { return };

    let err = App::new(local_config())
        .connect_options(db.app_options())
        .expect_migrations(&["20260101000001", "20991231235959"])
        .connect()
        .await
        .map(|_| ())
        .expect_err("a missing migration is a boot error");
    match &err {
        Error::Migrations(basable_db::migrate::MigrateError::Missing(v)) => {
            assert_eq!(v, &vec!["20991231235959".to_string()]);
        }
        other => panic!("unexpected boot error: {other}"),
    }
    assert!(err.to_string().contains("20991231235959"), "{err}");

    let app = App::new(local_config())
        .connect_options(db.app_options())
        .expect_migrations(&["20260101000001", "20260101000100"])
        .connect()
        .await
        .expect("a current ledger boots");
    assert_eq!(
        app.connections_reserved(),
        basable_app::APP_POOL_CONNECTIONS
    );
    db.finish().await;
}

#[tokio::test]
async fn a_database_that_never_answers_fails_within_the_boot_wait() {
    let mut cfg = Config::new("postgres://app:x@127.0.0.1:1/nothing");
    cfg.boot_wait_secs = 1;
    let started = std::time::Instant::now();
    let err = App::new(cfg)
        .connect()
        .await
        .map(|_| ())
        .expect_err("no database");
    assert!(
        matches!(err, Error::Connect { attempts, .. } if attempts >= 1),
        "{err}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn nanoservice_pools_are_isolated_and_budgeted() {
    let Some(db) = test_db().await else { return };
    let mut cfg = local_config();
    cfg.pool_max_connections = 4;
    cfg.connection_budget = basable_app::APP_POOL_CONNECTIONS + 2 * 4;
    let app = App::new(cfg)
        .connect_options(db.app_options())
        .connect()
        .await
        .unwrap();

    let orders = app.pool::<Orders>().await.unwrap();
    let inventory = app.pool::<Inventory>().await.unwrap();
    assert_eq!(
        app.connections_reserved(),
        basable_app::APP_POOL_CONNECTIONS + 8
    );

    // The owner reads its own table; the neighbour gets 42501.
    sqlx::query("SELECT count(*) FROM nano_orders.order_spec")
        .execute(orders.pool())
        .await
        .unwrap();
    let err = sqlx::query("SELECT count(*) FROM nano_orders.order_spec")
        .execute(inventory.pool())
        .await
        .unwrap_err();
    assert!(
        basable_db::sqlstate::is_insufficient_privilege(&err),
        "{err}"
    );

    // A third pool would exceed the budget.
    let err = app
        .pool::<Orders>()
        .await
        .map(|_| ())
        .expect_err("over budget");
    assert!(
        matches!(
            err,
            Error::ConnectionBudget {
                requested: 16,
                budget: 12
            }
        ),
        "{err}"
    );

    orders.close().await;
    inventory.close().await;
    db.finish().await;
}

#[tokio::test]
async fn readiness_flips_after_wiring_and_the_routes_are_served() {
    let Some(db) = test_db().await else { return };
    let app = App::new(local_config())
        .connect_options(db.app_options())
        .connect()
        .await
        .unwrap();
    let routes = axum::Router::new().route("/hello", axum::routing::get(|| async { "hi\n" }));
    let running = app.serve().raw(routes).start().await.unwrap();
    let base = format!("http://{}", running.addr());
    let client = reqwest::Client::new();

    let r = client.get(format!("{base}/healthz")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = client.get(format!("{base}/readyz")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.text().await.unwrap().starts_with("ready: database ok"));
    let r = client.get(format!("{base}/hello")).send().await.unwrap();
    assert_eq!(r.text().await.unwrap(), "hi\n");
    assert!(running.is_ready().await);

    tokio::time::timeout(Duration::from_secs(10), running.wake_bus().listening())
        .await
        .expect("the wake bus listens");
    running.shutdown().await.unwrap();
    // The server is gone after shutdown.
    assert!(client.get(format!("{base}/healthz")).send().await.is_err());
    db.finish().await;
}

#[tokio::test]
async fn a_wake_reaches_the_registered_worker_and_shutdown_drains_an_attempt_as_a_retry() {
    let Some(db) = test_db().await else { return };
    apply_schema(db.migrator_pool()).await.unwrap();
    let app = App::new(local_config())
        .connect_options(db.app_options())
        .connect()
        .await
        .unwrap();
    let pool = app.pool::<Conformance>().await.unwrap();
    let store: TypedStore<Spec, Status, _> =
        TypedStore::bind(&pool, conformance_type()).await.unwrap();

    let sim = Arc::new(WidgetSim::start().await);
    let gate = Gate::new();
    let mut cfg = fast_config();
    // A long poll: only a wake from the bus can pick the object up quickly.
    cfg.poll_interval = Duration::from_secs(30);
    let worker = Worker::new(
        store.clone(),
        cfg,
        ExampleReconciler::new(sim.clone()).before(gate.clone()),
        NoAfterComplete,
    )
    .unwrap();
    let running = app
        .serve()
        .worker("conformance", worker)
        .start()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), running.wake_bus().listening())
        .await
        .expect("the wake bus listens");

    // Creating an object nudges the wake channel; the bus wakes the worker,
    // which claims it and blocks at the gate.
    let id = Uuid::new_v4();
    let name = basable_processingobject::NamespacedName::new(
        Uuid::new_v4(),
        basable_processingobject_testkit::identity_name(id),
    );
    store
        .create(
            id,
            name,
            &Spec {
                widgets: 1,
                content: "x".into(),
            },
            &Status::default(),
            basable_processingobject::CreateOptions::none(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.wait_entered())
        .await
        .expect(
            "the worker was woken and claimed the object within 5 s; a 30 s poll could not have",
        );

    // Shutdown cancels the attempt at the gate: it completes as a retry.
    let started = std::time::Instant::now();
    running.shutdown().await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the drain waited on nothing"
    );

    let row = sqlx::query(
        "SELECT attempts, phase, COALESCE(last_error, '') AS last_error, claim_token
         FROM nano_conformance.processing_object_conformance WHERE id = $1",
    )
    .bind(id)
    .fetch_one(db.superuser())
    .await
    .unwrap();
    let attempts: i32 = row.get("attempts");
    let phase: String = row.get("phase");
    let last_error: String = row.get("last_error");
    let claim: Option<Uuid> = row.get("claim_token");
    assert_eq!(attempts, 1, "one attempt was made");
    assert!(claim.is_none(), "the claim was released by the completion");
    assert!(
        last_error.contains("cancelled"),
        "last_error = {last_error:?}"
    );
    assert_ne!(
        phase, "converged",
        "a cancelled attempt is a retry, not a convergence"
    );
    let _ = TYPE_NAME;
    pool.close().await;
    db.finish().await;
}
