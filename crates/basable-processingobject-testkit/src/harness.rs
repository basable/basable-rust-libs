//! The harness: a conformance-typed store over a per-test database, a
//! provider simulator, and the default policy. The store half of the Go
//! harness; the drive, worker, crash and gate helpers arrive with the
//! claim and worker phases.

use std::time::Duration;

use basable_db::NanoPool;
use basable_processingobject::{
    Backoff, CreateOptions, Error, Meta, NamespacedName, Object, Ref, TypedStore, WorkerConfig,
};
use basable_testkit::TestDb;
use uuid::Uuid;

use crate::conformance::{
    Conformance, ConformanceAdapter, Spec, Status, apply_schema, conformance_type, identity_name,
};
use crate::widgetsim::WidgetSim;

/// The default worker and claim policy for conformance: short poll and
/// backoff so scenarios converge quickly, a generous attempt timeout so a
/// gate-held attempt is not timed out mid-test, no attempt cap.
pub fn fast_config() -> WorkerConfig {
    WorkerConfig {
        resync: Duration::from_secs(3600),
        backoff: Backoff {
            base: Duration::from_millis(20),
            max: Duration::from_millis(200),
        },
        max_attempts: 0,
        attempt_timeout: Duration::from_secs(15),
        poll_interval: Duration::from_millis(50),
        batch_size: 10,
        parallelism: 4,
        after_complete_timeout: Duration::from_secs(1),
        label_selector: Default::default(),
    }
}

/// The conformance store type.
pub type ConformanceStore = TypedStore<Spec, Status, ConformanceAdapter>;

/// A conformance-typed store over a test database, a simulator, and the
/// default policy.
pub struct Harness {
    /// The database. Its migrator and superuser pools serve assertions that
    /// cross the nanoservice boundary.
    pub db: TestDb,
    /// The conformance nanoservice's pool, as production would hold it.
    pub pool: NanoPool<Conformance>,
    /// The provider simulator.
    pub sim: WidgetSim,
    /// The store.
    pub store: ConformanceStore,
    /// The config-namespace id the harness stamps on every identity it
    /// mints: a fresh uuid per harness (the suites need a namespace value,
    /// not a config database).
    pub namespace: Uuid,
    cfg: WorkerConfig,
}

impl Harness {
    /// A harness over a fresh database with the tenant fixture and the
    /// conformance schema applied, or `None` without `TEST_DATABASE_URL`.
    pub async fn from_env() -> Option<Harness> {
        Harness::from_env_config(fast_config()).await
    }

    /// [`Harness::from_env`] with an explicit default policy.
    pub async fn from_env_config(cfg: WorkerConfig) -> Option<Harness> {
        let fixtures = basable_testkit::runfile("crates/basable-testkit/tests/fixtures/migrations");
        let db = TestDb::from_env_with_migrations(&fixtures).await?;
        Some(Harness::over(db, cfg).await)
    }

    /// A harness over a database the caller made: installs the conformance
    /// schema, opens the nanoservice pool, binds the store.
    pub async fn over(db: TestDb, cfg: WorkerConfig) -> Harness {
        apply_schema(db.migrator_pool())
            .await
            .expect("the conformance migrations apply");
        let pool = db.nano_pool::<Conformance>().await;
        let store = TypedStore::bind(&pool, conformance_type())
            .await
            .expect("the store binds");
        Harness {
            db,
            pool,
            sim: WidgetSim::start().await,
            store,
            namespace: Uuid::new_v4(),
            cfg,
        }
    }

    /// The harness's default policy.
    pub fn config(&self) -> WorkerConfig {
        self.cfg.clone()
    }

    /// A second store over its own pool, the way a second replica holds one.
    pub async fn second_store(&self) -> (NanoPool<Conformance>, ConformanceStore) {
        let pool = self.db.nano_pool::<Conformance>().await;
        let store = TypedStore::bind(&pool, conformance_type())
            .await
            .expect("the store binds");
        (pool, store)
    }

    /// The deterministic identity for an object id: the harness namespace
    /// plus [`identity_name`]. Stable across create replays.
    pub fn identity(&self, id: Uuid) -> NamespacedName {
        NamespacedName::new(self.namespace, identity_name(id))
    }

    /// Mints an id and creates an object with a zero status.
    pub async fn create(&self, spec: Spec) -> Result<Ref, Error> {
        self.create_with_id(Uuid::new_v4(), spec).await
    }

    /// Creates an object under a caller-supplied id — the seam for
    /// ambiguous-create adoption tests, where the same id is presented
    /// twice.
    pub async fn create_with_id(&self, id: Uuid, spec: Spec) -> Result<Ref, Error> {
        self.store
            .create(
                id,
                self.identity(id),
                &spec,
                &Status::default(),
                CreateOptions::none(),
            )
            .await
    }

    /// The envelope snapshot for `r`.
    pub async fn meta(&self, r: &Ref) -> Result<Meta, Error> {
        self.store.read(r).await.map(|o| o.meta)
    }

    /// Polls `r` until `predicate` holds or `timeout` passes. Not-found is
    /// "keep waiting" (a deletion scenario waits for the row to vanish
    /// through a predicate on a separate read); any other error is returned.
    pub async fn wait_for(
        &self,
        r: &Ref,
        timeout: Duration,
        predicate: impl Fn(&Object<Spec, Status>) -> bool,
    ) -> Result<Object<Spec, Status>, Error> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.store.read(r).await {
                Ok(obj) if predicate(&obj) => return Ok(obj),
                Ok(_) | Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::InvalidConfig(format!("waiting for {r}: timed out")));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Whether a durable teardown row exists for `id` — proof
    /// `finalize_delete` committed. The archive outlives the envelope.
    pub async fn is_archived(&self, id: Uuid) -> Result<bool, sqlx::Error> {
        let (exists,): (bool,) =
            sqlx::query_as("SELECT EXISTS (SELECT 1 FROM conformance_archive WHERE id = $1)")
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    /// Drops the database.
    pub async fn finish(self) {
        self.pool.close().await;
        drop(self.sim);
        self.db.finish().await;
    }
}

/// The terminal-success shape most conformance tests wait for: the current
/// generation observed and the phase converged.
pub fn reconciled(obj: &Object<Spec, Status>) -> bool {
    obj.observed_current() && obj.meta.phase == basable_processingobject::Phase::Converged
}
