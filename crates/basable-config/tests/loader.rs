//! The loader and the repository over a migrated database: the seed
//! directories under `tests/seed/` against the tenant fixture (the
//! framework migration plus the `pricing_rule` config type of the `orders`
//! nanoservice).

use std::path::PathBuf;
use std::sync::Arc;

use basable_config::{
    ConfigError, ConfigTypes, ConfigTypesBuilder, Environment, LoadResult, Loader,
    MANAGED_BY_CONFIG, MANAGED_BY_LABEL, MANAGED_BY_RUNTIME, NAMESPACE_TYPE, Namespace, Repository,
    TypeInfo, load_seed,
};
use basable_db::{MigratorPool, PoolConfig};
use basable_testkit::{TestDb, runfile};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

/// The fixture's config type: `orders`' pricing rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PricingRule {
    rate_cents: i64,
    currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    home: Option<String>,
}

const PRICING_RULE: TypeInfo = TypeInfo {
    id: 100,
    name: "pricing_rule",
    prefix: "prule",
};

fn types() -> Arc<ConfigTypes> {
    let mut b = ConfigTypesBuilder::new();
    b.register::<PricingRule>(PRICING_RULE);
    b.build().unwrap().shared()
}

fn seed(name: &str) -> PathBuf {
    runfile(&format!("crates/basable-config/tests/seed/{name}"))
}

async fn db() -> Option<TestDb> {
    TestDb::from_env_with_migrations(&runfile("crates/basable-testkit/tests/fixtures/migrations"))
        .await
}

/// A second `app`-login pool, the way a second replica holds one.
async fn app_pool(db: &TestDb) -> PgPool {
    MigratorPool::connect(db.app_options(), PoolConfig::default())
        .await
        .unwrap()
        .pool()
        .clone()
}

async fn history_rows(db: &TestDb) -> i64 {
    sqlx::query("SELECT count(*) FROM basable_config.configuration_object_history")
        .fetch_one(db.superuser())
        .await
        .unwrap()
        .get::<i64, _>(0)
}

#[tokio::test]
async fn a_load_creates_updates_deletes_and_never_prunes() {
    let Some(db) = db().await else {
        return;
    };
    let types = types();
    let loader = Loader::new(db.migrator().clone(), Arc::clone(&types));
    let repo = Repository::new(db.migrator().clone(), Arc::clone(&types));

    // The first load: the framework-seeded `default` namespace is adopted
    // (its spec changes from {} to the declared one), `eu` and the three
    // rules are created.
    let r = loader.load(&seed("v1"), Environment::Dev).await.unwrap();
    assert_eq!(
        r,
        LoadResult {
            created: 4,
            updated: 1,
            unchanged: 0,
            deleted: 0
        },
        "{r:?}"
    );
    assert_eq!(
        history_rows(&db).await,
        1,
        "the adopted namespace's {{}} spec"
    );

    let default_ns = repo.namespace_id("default").await.unwrap().unwrap();
    assert_eq!(
        default_ns,
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
        "the framework's namespace row was adopted, not replaced"
    );
    let eu = repo.namespace_id("eu").await.unwrap().unwrap();
    let ns: basable_config::Object<Namespace> =
        repo.get(NAMESPACE_TYPE, None, "eu").await.unwrap().unwrap();
    assert_eq!(ns.spec.display_name, "Europe");
    assert_eq!(ns.namespace_id, None);
    assert!(ns.public_id.starts_with("ns_"));
    assert_eq!(
        ns.labels.get(MANAGED_BY_LABEL).map(String::as_str),
        Some(MANAGED_BY_CONFIG)
    );

    // References resolved to ids, labels stamped, the prod-only file skipped.
    let standard = repo
        .get::<PricingRule>(PRICING_RULE, Some(default_ns), "standard")
        .await
        .unwrap()
        .unwrap();
    let premium = repo
        .get::<PricingRule>(PRICING_RULE, Some(default_ns), "premium")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        premium.spec.supersedes.as_deref(),
        Some(standard.id.to_string().as_str())
    );
    assert_eq!(
        standard.labels.get("tier").map(String::as_str),
        Some("standard")
    );
    assert_eq!(standard.version, 1);
    assert!(standard.public_id.starts_with("prule_"));
    let eu_standard = repo
        .get::<PricingRule>(PRICING_RULE, Some(eu), "standard")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        eu_standard.spec.home.as_deref(),
        Some(eu.to_string().as_str())
    );
    assert!(
        repo.get::<PricingRule>(PRICING_RULE, Some(default_ns), "prod-only")
            .await
            .unwrap()
            .is_none(),
        "a prod-scoped file does not apply to dev"
    );
    let all: Vec<basable_config::Object<PricingRule>> =
        repo.list(PRICING_RULE, None).await.unwrap();
    assert_eq!(all.len(), 3);
    let in_default: Vec<basable_config::Object<PricingRule>> =
        repo.list(PRICING_RULE, Some(default_ns)).await.unwrap();
    assert_eq!(
        in_default
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>(),
        vec!["premium", "standard"]
    );

    // A re-run changes nothing: no history row, no version bump.
    let r = loader.load(&seed("v1"), Environment::Dev).await.unwrap();
    assert_eq!(
        r,
        LoadResult {
            created: 0,
            updated: 0,
            unchanged: 5,
            deleted: 0
        }
    );
    assert_eq!(history_rows(&db).await, 1);

    // A runtime-written object survives a load (the loader never prunes).
    let runtime_id = repo
        .upsert(
            PRICING_RULE,
            Some(default_ns),
            "promo",
            &Default::default(),
            &PricingRule {
                rate_cents: 1,
                currency: "EUR".into(),
                supersedes: None,
                home: None,
            },
        )
        .await
        .unwrap();

    // The second seed: standard's rate changes (one history row, version
    // 2), premium is deleted (its last version recorded), everything else
    // is unchanged.
    let r = loader.load(&seed("v2"), Environment::Dev).await.unwrap();
    assert_eq!(
        r,
        LoadResult {
            created: 0,
            updated: 1,
            unchanged: 3,
            deleted: 1
        }
    );
    assert_eq!(history_rows(&db).await, 3);
    let standard = repo
        .get::<PricingRule>(PRICING_RULE, Some(default_ns), "standard")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(standard.spec.rate_cents, 180);
    assert_eq!(standard.version, 2);
    assert!(
        repo.get::<PricingRule>(PRICING_RULE, Some(default_ns), "premium")
            .await
            .unwrap()
            .is_none()
    );
    let promo = repo
        .get_by_id::<PricingRule>(PRICING_RULE, runtime_id)
        .await
        .unwrap()
        .expect("the runtime object survived the load");
    assert_eq!(
        promo.labels.get(MANAGED_BY_LABEL).map(String::as_str),
        Some(MANAGED_BY_RUNTIME)
    );

    // The prod scope applies the extra file.
    let r = loader.load(&seed("v1"), Environment::Prod).await.unwrap();
    assert_eq!(r.created, 2, "prod-only, and premium again: {r:?}");
    assert_eq!(r.updated, 1, "standard back to 165: {r:?}");

    db.finish().await;
}

#[tokio::test]
async fn a_dangling_reference_fails_before_any_write() {
    let Some(db) = db().await else {
        return;
    };
    let loader = Loader::new(db.migrator().clone(), types());
    let err = loader
        .load(&seed("dangling"), Environment::Dev)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ConfigError::Reference { reason, .. } if reason.contains("ghost")),
        "{err}"
    );
    let n: i64 = sqlx::query("SELECT count(*) FROM basable_config.configuration_object")
        .fetch_one(db.superuser())
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1, "only the framework's namespace row exists");
    db.finish().await;
}

#[tokio::test]
async fn the_repository_writes_by_natural_key_and_adopts_into_config() {
    let Some(db) = db().await else {
        return;
    };
    let types = types();
    let repo = Repository::new(db.migrator().clone(), Arc::clone(&types));
    let default_ns = repo.namespace_id("default").await.unwrap().unwrap();
    let rule = PricingRule {
        rate_cents: 165,
        currency: "EUR".into(),
        supersedes: None,
        home: None,
    };

    // Create, then an unchanged write, then a change.
    let id = repo
        .upsert(
            PRICING_RULE,
            Some(default_ns),
            "standard",
            &Default::default(),
            &rule,
        )
        .await
        .unwrap();
    let again = repo
        .upsert(
            PRICING_RULE,
            Some(default_ns),
            "standard",
            &Default::default(),
            &rule,
        )
        .await
        .unwrap();
    assert_eq!(id, again, "the natural key is the identity");
    assert_eq!(
        history_rows(&db).await,
        0,
        "an unchanged write writes nothing"
    );
    let mut changed = rule.clone();
    changed.rate_cents = 200;
    repo.upsert(
        PRICING_RULE,
        Some(default_ns),
        "standard",
        &Default::default(),
        &changed,
    )
    .await
    .unwrap();
    let got = repo
        .get::<PricingRule>(PRICING_RULE, Some(default_ns), "standard")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.version, 2);
    assert_eq!(got.spec, changed);
    assert_eq!(history_rows(&db).await, 1);
    assert_eq!(
        repo.lookup_id(PRICING_RULE, Some(default_ns), "standard")
            .await
            .unwrap(),
        Some(id)
    );

    // A namespaced lookup without a namespace is a caller error, not an
    // arbitrary row.
    assert!(
        repo.lookup_id(PRICING_RULE, None, "standard")
            .await
            .is_err()
    );

    // The loader adopts the runtime object: same id, managed-by flips to
    // config, the declared spec wins.
    let loader = Loader::new(db.migrator().clone(), Arc::clone(&types));
    loader.load(&seed("v1"), Environment::Dev).await.unwrap();
    let adopted = repo
        .get::<PricingRule>(PRICING_RULE, Some(default_ns), "standard")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(adopted.id, id);
    assert_eq!(adopted.spec.rate_cents, 165);
    assert_eq!(
        adopted.labels.get(MANAGED_BY_LABEL).map(String::as_str),
        Some(MANAGED_BY_CONFIG)
    );

    // Delete: gone, its last version in history, a second delete is false.
    assert!(
        repo.delete(PRICING_RULE, Some(default_ns), "standard")
            .await
            .unwrap()
    );
    assert!(
        !repo
            .delete(PRICING_RULE, Some(default_ns), "standard")
            .await
            .unwrap()
    );
    assert!(
        repo.get_by_id::<PricingRule>(PRICING_RULE, id)
            .await
            .unwrap()
            .is_none()
    );
    db.finish().await;
}

#[tokio::test]
async fn concurrent_seed_loads_serialise() {
    let Some(db) = db().await else {
        return;
    };
    let types = types();
    let (a, b) = (app_pool(&db).await, app_pool(&db).await);
    let dir = seed("v1");
    let (ra, rb) = tokio::join!(
        load_seed(&a, Arc::clone(&types), &dir, Environment::Dev),
        load_seed(&b, Arc::clone(&types), &dir, Environment::Dev),
    );
    let (ra, rb) = (ra.unwrap().unwrap(), rb.unwrap().unwrap());
    let mut results = [ra, rb];
    results.sort_by_key(|r| r.created);
    assert_eq!(
        results[1],
        LoadResult {
            created: 4,
            updated: 1,
            unchanged: 0,
            deleted: 0
        },
        "one replica applied the seed: {results:?}"
    );
    assert_eq!(
        results[0],
        LoadResult {
            created: 0,
            updated: 0,
            unchanged: 5,
            deleted: 0
        },
        "the other found it applied: {results:?}"
    );
    assert_eq!(history_rows(&db).await, 1);

    // An absent directory is a no-op.
    assert_eq!(
        load_seed(&a, types, &seed("absent"), Environment::Dev)
            .await
            .unwrap(),
        None
    );
    a.close().await;
    b.close().await;
    db.finish().await;
}

/// A typed binder: a type whose objects also live in a table of its own.
/// The loader and the repository call it inside their transaction, after
/// the base row, and it sees every reference already resolved.
struct RuleIndex;

impl basable_config::TypedBinder for RuleIndex {
    type Msg = PricingRule;

    fn type_info(&self) -> TypeInfo {
        PRICING_RULE
    }

    async fn upsert(
        &self,
        tx: &mut sqlx::PgConnection,
        id: Uuid,
        msg: &PricingRule,
    ) -> Result<(), basable_core::BoxError> {
        let supersedes = msg.supersedes.as_deref().map(Uuid::parse_str).transpose()?;
        sqlx::query(
            "INSERT INTO public.pricing_rule_index (id, currency, supersedes)
             VALUES ($1, $2, $3)
             ON CONFLICT (id) DO UPDATE SET currency = EXCLUDED.currency, supersedes = EXCLUDED.supersedes",
        )
        .bind(id)
        .bind(&msg.currency)
        .bind(supersedes)
        .execute(tx)
        .await?;
        Ok(())
    }

    async fn delete(
        &self,
        tx: &mut sqlx::PgConnection,
        id: Uuid,
    ) -> Result<(), basable_core::BoxError> {
        sqlx::query("DELETE FROM public.pricing_rule_index WHERE id = $1")
            .bind(id)
            .execute(tx)
            .await?;
        Ok(())
    }
}

#[tokio::test]
async fn a_typed_binder_writes_its_own_table_in_the_same_transaction() {
    let Some(db) = db().await else {
        return;
    };
    sqlx::query(
        "CREATE TABLE public.pricing_rule_index (
             id UUID PRIMARY KEY, currency TEXT NOT NULL, supersedes UUID
         )",
    )
    .execute(db.superuser())
    .await
    .unwrap();
    sqlx::query("GRANT SELECT, INSERT, UPDATE, DELETE ON public.pricing_rule_index TO app")
        .execute(db.superuser())
        .await
        .unwrap();

    let mut b = ConfigTypesBuilder::new();
    b.register_binder(RuleIndex);
    let types = b.build().unwrap().shared();
    let loader = Loader::new(db.migrator().clone(), Arc::clone(&types));
    let repo = Repository::new(db.migrator().clone(), Arc::clone(&types));

    loader.load(&seed("v1"), Environment::Dev).await.unwrap();
    let default_ns = repo.namespace_id("default").await.unwrap().unwrap();
    let standard = repo
        .lookup_id(PRICING_RULE, Some(default_ns), "standard")
        .await
        .unwrap()
        .unwrap();
    let premium = repo
        .lookup_id(PRICING_RULE, Some(default_ns), "premium")
        .await
        .unwrap()
        .unwrap();
    let indexed: Vec<(Uuid, Option<Uuid>)> =
        sqlx::query_as("SELECT id, supersedes FROM public.pricing_rule_index")
            .fetch_all(db.superuser())
            .await
            .unwrap();
    assert_eq!(indexed.len(), 3, "one index row per rule");
    assert!(
        indexed.contains(&(premium, Some(standard))),
        "the binder saw the resolved reference"
    );

    // v2 deletes premium: the binder's row goes with it.
    loader.load(&seed("v2"), Environment::Dev).await.unwrap();
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM public.pricing_rule_index WHERE id = $1")
            .bind(premium)
            .fetch_one(db.superuser())
            .await
            .unwrap();
    assert_eq!(n, 0);

    // A repository write reaches the binder too.
    repo.delete(PRICING_RULE, Some(default_ns), "standard")
        .await
        .unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM public.pricing_rule_index")
        .fetch_one(db.superuser())
        .await
        .unwrap();
    assert_eq!(n, 1, "only the eu rule remains");
    db.finish().await;
}
