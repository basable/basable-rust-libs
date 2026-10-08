//! The Phase 8 verification: the boot gate on the migration ledger, two
//! nanoservice pools that cannot read each other's schema, the connection
//! budget, readiness over a live server, a write through the store of a
//! component's worker waking it, a shutdown that drains an in-flight
//! attempt as a retry and names a loop that does not drain, a component
//! taking the default running nothing, and a duplicate loop name refused
//! before anything runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use basable_app::{APP_POOL_CONNECTIONS, App, Component, Components, Config, Error, Loops, Ticker};
use basable_db::{Nanoservice, Stateful};
use basable_processingobject::{NoAfterComplete, Ref, TypedStore, Worker, WorkerConfig};
use basable_processingobject_testkit::{
    Conformance, ConformanceStore, ExampleReconciler, Gate, Hook, Spec, Status, WidgetSim,
    apply_schema, conformance_type, fast_config, hook, reconciled,
};
use basable_testkit::TestDb;
use sqlx::Row;
use uuid::Uuid;

/// The test's router, shaped like the generated messenger: the components
/// by name, and the `Components` impl listing each with its loops in
/// declaration order.
struct Messenger(Vec<Named>);

type Named = (&'static str, Box<dyn Component<Messenger>>);

impl Components for Messenger {
    fn loops(&'static self) -> Vec<(&'static str, Loops)> {
        self.0
            .iter()
            .map(|(name, component)| (*name, component.loops(self)))
            .collect()
    }
}

/// Leaks the router, as a tenant's `main` does.
fn messenger(components: Vec<Named>) -> &'static Messenger {
    Box::leak(Box::new(Messenger(components)))
}

/// A component owning one processing-object worker over the conformance
/// type, built when the app asks for its loops. The gate holds only the
/// object whose content is `held`.
struct Reconciling {
    store: ConformanceStore,
    sim: Arc<WidgetSim>,
    gate: Gate,
    cfg: WorkerConfig,
}

impl<R> Component<R> for Reconciling {
    fn loops(&'static self, _router: &'static R) -> Loops {
        let rec = ExampleReconciler::new(self.sim.clone()).before(hook(move |ctx, claim| {
            Box::pin(async move {
                if claim.object.spec.content == "held" {
                    self.gate.call(ctx, claim).await
                } else {
                    Ok(())
                }
            })
        }));
        let worker = Worker::new(self.store.clone(), self.cfg.clone(), rec, NoAfterComplete)
            .expect("a valid worker config");
        Loops::new().worker(worker)
    }
}

/// A component owning one hourly ticker per name, each tick counted; with
/// `stall`, a tick ignores its context and does not end.
struct Ticking {
    names: &'static [&'static str],
    stall: bool,
    ticks: Arc<AtomicU32>,
}

impl<R> Component<R> for Ticking {
    fn loops(&'static self, _router: &'static R) -> Loops {
        self.names.iter().fold(Loops::new(), |loops, name| {
            loops.ticker(Ticker::new(*name, Duration::from_secs(3600), move |_ctx| {
                Box::pin(async move {
                    self.ticks.fetch_add(1, Ordering::SeqCst);
                    if self.stall {
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                    Ok(())
                })
            }))
        })
    }
}

/// A plain executor: it takes the default.
struct Idle;

impl<R> Component<R> for Idle {}

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
    assert_eq!(app.connections_reserved(), APP_POOL_CONNECTIONS);
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
    cfg.connection_budget = APP_POOL_CONNECTIONS + 2 * 4;
    let app = App::new(cfg)
        .connect_options(db.app_options())
        .connect()
        .await
        .unwrap();

    let orders = app.pool::<Orders>().await.unwrap();
    let inventory = app.pool::<Inventory>().await.unwrap();
    assert_eq!(app.connections_reserved(), APP_POOL_CONNECTIONS + 8);

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
            Error::ConnectionBudget { requested, budget }
                if requested == APP_POOL_CONNECTIONS + 12 && budget == APP_POOL_CONNECTIONS + 8
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

    running.shutdown().await.unwrap();
    // The server is gone after shutdown.
    assert!(client.get(format!("{base}/healthz")).send().await.is_err());
    db.finish().await;
}

async fn create(store: &ConformanceStore, content: &str) -> Ref {
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
                content: content.into(),
            },
            &Status::default(),
            basable_processingobject::CreateOptions::none(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn a_write_through_its_store_wakes_the_worker_and_shutdown_drains_the_attempt() {
    let Some(db) = test_db().await else { return };
    apply_schema(db.migrator_pool()).await.unwrap();
    let app = App::new(local_config())
        .connect_options(db.app_options())
        .connect()
        .await
        .unwrap();
    let pool = app.pool::<Conformance>().await.unwrap();
    let store: ConformanceStore = TypedStore::bind(&pool, conformance_type()).await.unwrap();

    // The gate holds only the object created once the worker is running.
    let gate = Gate::new();
    let mut cfg = fast_config();
    // A long poll: only a wake can pick an object up quickly.
    cfg.poll_interval = Duration::from_secs(30);
    let component = Reconciling {
        store: store.clone(),
        sim: Arc::new(WidgetSim::start().await),
        gate: gate.clone(),
        cfg,
    };

    // The first object is there before the app starts, so the worker's
    // first scan drives it: once it converged, the worker is registered on
    // the store and past that scan.
    let first = create(&store, "first").await;
    let running = app
        .serve()
        .components(messenger(vec![("conformance", Box::new(component))]))
        .start()
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !reconciled(&store.read(&first).await.unwrap()) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first scan converges the first object"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // A create through the same store wakes the worker, which claims the
    // object and blocks at the gate.
    let r = create(&store, "held").await;
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
    .bind(r.id)
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
    pool.close().await;
    db.finish().await;
}

async fn connect(db: &TestDb, cfg: Config) -> App {
    App::new(cfg)
        .connect_options(db.app_options())
        .connect()
        .await
        .unwrap()
}

async fn readyz(running: &basable_app::Running) -> String {
    reqwest::get(format!("http://{}/readyz", running.addr()))
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

async fn wait_for_ticks(ticks: &AtomicU32, n: u32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while ticks.load(Ordering::SeqCst) < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{n} tick(s) within 5 s"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_component_taking_the_default_runs_nothing() {
    let Some(db) = test_db().await else { return };
    let ticks = Arc::new(AtomicU32::new(0));
    let router = messenger(vec![
        ("api", Box::new(Idle)),
        (
            "sweeper",
            Box::new(Ticking {
                names: &["sweep"],
                stall: false,
                ticks: ticks.clone(),
            }),
        ),
    ]);
    let running = connect(&db, local_config())
        .await
        .serve()
        .components(router)
        .start()
        .await
        .unwrap();
    wait_for_ticks(&ticks, 1).await;
    // The ticker is the one loop: the default contributed none.
    assert_eq!(
        readyz(&running).await,
        "ready: database ok, 1 worker loop(s) alive\n"
    );
    running.shutdown().await.unwrap();
    db.finish().await;
}

#[tokio::test]
async fn a_duplicate_loop_name_fails_start_before_anything_runs() {
    let Some(db) = test_db().await else { return };

    // The components registered twice.
    let ticks = Arc::new(AtomicU32::new(0));
    let router = messenger(vec![(
        "sweeper",
        Box::new(Ticking {
            names: &["sweep"],
            stall: false,
            ticks: ticks.clone(),
        }),
    )]);
    let err = connect(&db, local_config())
        .await
        .serve()
        .components(router)
        .components(router)
        .start()
        .await
        .map(|_| ())
        .expect_err("a loop registered twice");
    assert!(
        matches!(&err, Error::DuplicateLoop(name) if name == "sweeper/sweep"),
        "{err}"
    );
    assert!(err.to_string().contains("sweeper/sweep"), "{err}");

    // One component with two loops of one name.
    let twice = messenger(vec![(
        "sweeper",
        Box::new(Ticking {
            names: &["sweep", "sweep"],
            stall: false,
            ticks: ticks.clone(),
        }),
    )]);
    let err = connect(&db, local_config())
        .await
        .serve()
        .components(twice)
        .start()
        .await
        .map(|_| ())
        .expect_err("two loops of one name");
    assert!(
        matches!(&err, Error::DuplicateLoop(name) if name == "sweeper/sweep"),
        "{err}"
    );

    // A ticker's first tick is immediate: none ran, so nothing was spawned.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(ticks.load(Ordering::SeqCst), 0);
    db.finish().await;
}

#[tokio::test]
async fn shutdown_names_a_loop_that_does_not_drain_under_its_component() {
    let Some(db) = test_db().await else { return };
    let mut cfg = local_config();
    cfg.shutdown_grace_secs = 1;
    let ticks = Arc::new(AtomicU32::new(0));
    let router = messenger(vec![
        (
            "sweeper",
            Box::new(Ticking {
                names: &["sweep"],
                stall: false,
                ticks: ticks.clone(),
            }),
        ),
        (
            "stuck",
            Box::new(Ticking {
                names: &["stall"],
                stall: true,
                ticks: ticks.clone(),
            }),
        ),
    ]);
    let running = connect(&db, cfg)
        .await
        .serve()
        .components(router)
        .start()
        .await
        .unwrap();
    wait_for_ticks(&ticks, 2).await;
    assert_eq!(
        readyz(&running).await,
        "ready: database ok, 2 worker loop(s) alive\n"
    );

    // The stalled tick ignores the cancelled context; the other drains.
    let started = std::time::Instant::now();
    let err = running
        .shutdown()
        .await
        .expect_err("a loop that does not drain");
    assert!(
        matches!(&err, Error::Stuck(names) if names == &["stuck/stall".to_string()]),
        "{err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "abandoned at the grace"
    );
    db.finish().await;
}
