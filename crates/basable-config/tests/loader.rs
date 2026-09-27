//! The loader and the repository over a migrated database, the port of
//! `golang/test/config/config_loader_integration_test.go`: the seed
//! directories under `tests/seed/` against the tenant fixture (the framework
//! migration plus the `PricingRuleConfiguration` type of the `orders`
//! nanoservice, whose binder and message live here as the scaffolder would
//! render them).

use std::path::PathBuf;
use std::sync::Arc;

use basable_config::{
    BinderError, ConfigError, ConfigHeader, ConfigMessage, ConfigTypes, ConfigTypesBuilder,
    Environment, LoadResult, Loader, MANAGED_BY_CONFIG, MANAGED_BY_LABEL, MANAGED_BY_RUNTIME,
    NAMESPACE_TYPE, NamespaceConfiguration, Repository, TypeInfo, TypedBinder, load_seed,
};
use basable_db::{MigratorPool, PoolConfig};
use basable_testkit::{TestDb, runfile};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Row as _};
use uuid::Uuid;

/// The fixture's config message, shaped as buffa would generate it from
/// `message PricingRuleConfiguration { ConfigHeader header = 1; int64
/// rate_cents = 2; string currency = 3; string supersedes = 4; }` and
/// decoded from protobuf JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PricingRuleConfiguration {
    #[serde(default)]
    header: ConfigHeader,
    #[serde(default, alias = "rate_cents")]
    rate_cents: i64,
    #[serde(default)]
    currency: String,
    /// A reference: `#{PricingRuleConfiguration:ns:name}` in the seed, the
    /// referenced object's id once loaded, empty for none.
    #[serde(default)]
    supersedes: String,
}

impl ConfigMessage for PricingRuleConfiguration {
    fn header(&self) -> ConfigHeader {
        self.header.clone()
    }
}

const PRICING_RULE: TypeInfo = TypeInfo {
    id: 100,
    name: "PricingRuleConfiguration",
    prefix: "prule",
};

/// The binder over `basable_config.pricing_rule_configuration`: the shape
/// the scaffolder renders for a declared config type.
struct PricingRuleBinder;

impl TypedBinder for PricingRuleBinder {
    type Msg = PricingRuleConfiguration;

    fn type_info(&self) -> TypeInfo {
        PRICING_RULE
    }

    async fn upsert(
        &self,
        tx: &mut PgConnection,
        id: Uuid,
        msg: &PricingRuleConfiguration,
    ) -> Result<(), BinderError> {
        let supersedes = match msg.supersedes.as_str() {
            "" => None,
            raw => Some(Uuid::parse_str(raw)?),
        };
        sqlx::query(
            "INSERT INTO basable_config.pricing_rule_configuration (id, rate_cents, currency, supersedes)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET
                 rate_cents = EXCLUDED.rate_cents,
                 currency = EXCLUDED.currency,
                 supersedes = EXCLUDED.supersedes",
        )
        .bind(id)
        .bind(msg.rate_cents)
        .bind(&msg.currency)
        .bind(supersedes)
        .execute(tx)
        .await?;
        Ok(())
    }

    async fn delete(&self, tx: &mut PgConnection, id: Uuid) -> Result<(), BinderError> {
        // A rule another rule supersedes is still referenced: refuse, so a
        // prune retries once the referrer is gone.
        let (referrers,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM basable_config.pricing_rule_configuration WHERE supersedes = $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if referrers > 0 {
            return Err(BinderError::StillReferenced(format!(
                "{referrers} rule(s) supersede it"
            )));
        }
        sqlx::query("DELETE FROM basable_config.pricing_rule_configuration WHERE id = $1")
            .bind(id)
            .execute(tx)
            .await?;
        Ok(())
    }

    async fn read(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
    ) -> Result<Option<PricingRuleConfiguration>, BinderError> {
        let row: Option<(i64, String, Option<Uuid>)> = sqlx::query_as(
            "SELECT rate_cents, currency, supersedes
             FROM basable_config.pricing_rule_configuration WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(conn)
        .await?;
        Ok(row.map(
            |(rate_cents, currency, supersedes)| PricingRuleConfiguration {
                header: ConfigHeader::default(),
                rate_cents,
                currency,
                supersedes: supersedes.map(|u| u.to_string()).unwrap_or_default(),
            },
        ))
    }
}

fn types() -> Arc<ConfigTypes> {
    let mut b = ConfigTypesBuilder::new();
    b.register(PricingRuleBinder);
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

/// Closed versions in a history table: what the trigger recorded.
async fn closed_versions(db: &TestDb, table: &str) -> i64 {
    sqlx::query(&format!(
        "SELECT count(*) FROM basable_config.{table}_history WHERE upper(system_period) IS NOT NULL"
    ))
    .fetch_one(db.superuser())
    .await
    .unwrap()
    .get::<i64, _>(0)
}

async fn count(db: &TestDb, sql: &str) -> i64 {
    sqlx::query(sql)
        .fetch_one(db.superuser())
        .await
        .unwrap()
        .get::<i64, _>(0)
}

fn rule(rate_cents: i64, supersedes: &str) -> PricingRuleConfiguration {
    PricingRuleConfiguration {
        header: ConfigHeader::default(),
        rate_cents,
        currency: "EUR".into(),
        supersedes: supersedes.into(),
    }
}

#[tokio::test]
async fn a_load_creates_updates_and_prunes() {
    let Some(db) = db().await else {
        return;
    };
    let types = types();
    let loader = Loader::new(db.migrator().clone(), Arc::clone(&types));
    let repo = Repository::new(db.migrator().clone(), Arc::clone(&types));

    // Phase 1: the initial load — two namespaces, three rules; the prod-only
    // file is out of scope.
    let r = loader.load(&seed("v1"), Environment::Dev).await.unwrap();
    assert_eq!(
        r,
        LoadResult {
            created: 5,
            updated: 0,
            deleted: 0
        },
        "{r:?}"
    );
    let default_ns = repo.namespace_id("default").await.unwrap().unwrap();
    let ns: basable_config::Object<NamespaceConfiguration> = repo
        .get(NAMESPACE_TYPE, None, "default")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ns.namespace_id, ns.id, "a namespace object self-references");
    assert_eq!(ns.message.display_name, "Default");
    assert!(ns.external_id.starts_with("ns_"), "{}", ns.external_id);
    assert_eq!(
        ns.labels.get(MANAGED_BY_LABEL).map(String::as_str),
        Some(MANAGED_BY_CONFIG)
    );

    let standard = repo
        .get::<PricingRuleConfiguration>(PRICING_RULE, Some(default_ns), "standard")
        .await
        .unwrap()
        .unwrap();
    let premium = repo
        .get::<PricingRuleConfiguration>(PRICING_RULE, Some(default_ns), "premium")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(standard.namespace_id, default_ns);
    assert_eq!(standard.message.rate_cents, 165);
    assert_eq!(
        premium.message.supersedes,
        standard.id.to_string(),
        "the reference resolved to the target's id"
    );
    assert_eq!(
        standard.labels.get("tier").map(String::as_str),
        Some("standard")
    );
    assert!(
        standard.external_id.starts_with("prule_"),
        "{}",
        standard.external_id
    );
    assert!(
        repo.get::<PricingRuleConfiguration>(PRICING_RULE, Some(default_ns), "prod-only")
            .await
            .unwrap()
            .is_none(),
        "a prod-scoped file does not apply to dev"
    );
    let in_default: Vec<basable_config::Object<PricingRuleConfiguration>> =
        repo.list(PRICING_RULE, Some(default_ns)).await.unwrap();
    assert_eq!(
        in_default
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>(),
        vec!["premium", "standard"]
    );
    assert_eq!(
        repo.list::<PricingRuleConfiguration>(PRICING_RULE, None)
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(closed_versions(&db, "configuration_object").await, 0);

    // Phase 2: an idempotent re-run — every object re-applied, zero history.
    let r = loader.load(&seed("v1"), Environment::Dev).await.unwrap();
    assert_eq!(
        r,
        LoadResult {
            created: 0,
            updated: 5,
            deleted: 0
        }
    );
    assert_eq!(closed_versions(&db, "configuration_object").await, 0);
    assert_eq!(closed_versions(&db, "pricing_rule_configuration").await, 0);

    // A runtime-written object in the same namespace is not the loader's
    // and survives every prune.
    let promo_id = repo
        .upsert(PRICING_RULE, Some(default_ns), "promo", &rule(1, ""))
        .await
        .unwrap();

    // Phase 3: a field edited, an item removed — one closed version, stable
    // identity, the premium rule pruned.
    let r = loader.load(&seed("v2"), Environment::Dev).await.unwrap();
    assert_eq!(
        r,
        LoadResult {
            created: 0,
            updated: 4,
            deleted: 1
        }
    );
    let standard_again = repo
        .get::<PricingRuleConfiguration>(PRICING_RULE, Some(default_ns), "standard")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(standard_again.id, standard.id, "id stable");
    assert_eq!(
        standard_again.external_id, standard.external_id,
        "external id stable"
    );
    assert_eq!(standard_again.message.rate_cents, 180);
    assert_eq!(
        closed_versions(&db, "pricing_rule_configuration").await,
        2,
        "the edit closed standard's version, the prune closed premium's"
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM basable_config.pricing_rule_configuration_history
             WHERE upper(system_period) IS NOT NULL AND rate_cents = 165"
        )
        .await,
        1,
        "the previous value is in history"
    );
    assert!(
        repo.get_by_id::<PricingRuleConfiguration>(PRICING_RULE, premium.id)
            .await
            .unwrap()
            .is_none(),
        "premium left the files and was pruned"
    );
    let promo = repo
        .get_by_id::<PricingRuleConfiguration>(PRICING_RULE, promo_id)
        .await
        .unwrap()
        .expect("the runtime object survived the prune");
    assert_eq!(
        promo.labels.get(MANAGED_BY_LABEL).map(String::as_str),
        Some(MANAGED_BY_RUNTIME)
    );

    // Phase 4: the prod scope applies the extra file (and re-creates
    // premium); the two namespaces and the two remaining rules re-apply.
    let r = loader.load(&seed("v1"), Environment::Prod).await.unwrap();
    assert_eq!(
        r,
        LoadResult {
            created: 2,
            updated: 4,
            deleted: 0
        },
        "{r:?}"
    );

    // Phase 5: only the default namespace stays declared. Everything the
    // loader manages besides it is pruned — the eu namespace last, so its
    // member goes through its own binder first; standard, which premium
    // supersedes, is refused on the first pass and freed once premium is
    // gone. The runtime object stays.
    let r = loader.load(&seed("v3"), Environment::Dev).await.unwrap();
    assert_eq!(
        r,
        LoadResult {
            created: 0,
            updated: 1,
            deleted: 5
        },
        "{r:?}"
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM basable_config.configuration_object"
        )
        .await,
        2,
        "the default namespace and the runtime object"
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM basable_config.pricing_rule_configuration"
        )
        .await,
        1
    );
    assert!(repo.namespace_id("eu").await.unwrap().is_none());
    assert!(
        closed_versions(&db, "configuration_object").await >= 5,
        "the final state of every pruned object is in history"
    );

    db.finish().await;
}

#[tokio::test]
async fn environment_scoped_files_and_the_directory_are_checked() {
    let Some(db) = db().await else {
        return;
    };
    let loader = Loader::new(db.migrator().clone(), types());

    // A typo'd token is a hard error, not an unscoped file.
    let dir = std::env::temp_dir().join(format!("basable-config-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        seed("v3").join("namespaces.json"),
        dir.join("namespaces.json"),
    )
    .unwrap();
    std::fs::copy(
        seed("v1").join("pricing_rules.json"),
        dir.join("pricing_rules.pord.json"),
    )
    .unwrap();
    let err = loader.load(&dir, Environment::Dev).await.unwrap_err();
    assert!(err.to_string().contains("\"pord\""), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();

    // A directory without seed files is a wrong path.
    let empty = std::env::temp_dir().join(format!("basable-config-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&empty).unwrap();
    let err = loader.load(&empty, Environment::Dev).await.unwrap_err();
    assert!(err.to_string().contains("no config files"), "{err}");
    std::fs::remove_dir_all(&empty).unwrap();

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
        matches!(&err, ConfigError::Reference { reason, .. } if reason.contains("ghost") && reason.contains("not declared")),
        "{err}"
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM basable_config.configuration_object"
        )
        .await,
        0
    );
    db.finish().await;
}

#[tokio::test]
async fn the_repository_writes_by_natural_key_and_the_loader_adopts() {
    let Some(db) = db().await else {
        return;
    };
    let types = types();
    let repo = Repository::new(db.migrator().clone(), Arc::clone(&types));
    let loader = Loader::new(db.migrator().clone(), Arc::clone(&types));

    // The namespace first: the repository writes one like any other object.
    let ns_id = repo
        .upsert(
            NAMESPACE_TYPE,
            None,
            "default",
            &NamespaceConfiguration {
                header: ConfigHeader::default(),
                display_name: "Default".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(repo.namespace_id("default").await.unwrap(), Some(ns_id));

    // Create, an unchanged write, a change: identity stable, history only
    // on the change.
    let id = repo
        .upsert(PRICING_RULE, Some(ns_id), "standard", &rule(165, ""))
        .await
        .unwrap();
    let again = repo
        .upsert(PRICING_RULE, Some(ns_id), "standard", &rule(165, ""))
        .await
        .unwrap();
    assert_eq!(id, again, "the natural key is the identity");
    assert_eq!(closed_versions(&db, "pricing_rule_configuration").await, 0);
    repo.upsert(PRICING_RULE, Some(ns_id), "standard", &rule(200, ""))
        .await
        .unwrap();
    let got = repo
        .get::<PricingRuleConfiguration>(PRICING_RULE, Some(ns_id), "standard")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.message.rate_cents, 200);
    assert_eq!(
        got.labels.get(MANAGED_BY_LABEL).map(String::as_str),
        Some(MANAGED_BY_RUNTIME)
    );
    assert_eq!(closed_versions(&db, "pricing_rule_configuration").await, 1);
    assert_eq!(
        repo.lookup_id(PRICING_RULE, Some(ns_id), "standard")
            .await
            .unwrap(),
        Some(id)
    );
    assert!(
        repo.lookup_id(PRICING_RULE, None, "standard")
            .await
            .is_err(),
        "a namespaced lookup without a namespace is a caller error, not an arbitrary row"
    );

    // A binder's refusal surfaces: premium supersedes standard.
    repo.upsert(
        PRICING_RULE,
        Some(ns_id),
        "premium",
        &rule(330, &id.to_string()),
    )
    .await
    .unwrap();
    let err = repo
        .delete(PRICING_RULE, Some(ns_id), "standard")
        .await
        .unwrap_err();
    assert!(err.is_still_referenced(), "{err}");
    assert!(
        repo.delete(PRICING_RULE, Some(ns_id), "premium")
            .await
            .unwrap()
    );
    assert!(
        repo.delete(PRICING_RULE, Some(ns_id), "standard")
            .await
            .unwrap()
    );
    assert!(
        !repo
            .delete(PRICING_RULE, Some(ns_id), "standard")
            .await
            .unwrap(),
        "a second delete finds nothing"
    );

    // The loader adopts a runtime object with the declared name: same id,
    // managed-by flips to config, the declared value wins — and from then
    // on it is the loader's to prune.
    let runtime_id = repo
        .upsert(PRICING_RULE, Some(ns_id), "standard", &rule(1, ""))
        .await
        .unwrap();
    loader.load(&seed("v1"), Environment::Dev).await.unwrap();
    let adopted = repo
        .get::<PricingRuleConfiguration>(PRICING_RULE, Some(ns_id), "standard")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(adopted.id, runtime_id);
    assert_eq!(adopted.message.rate_cents, 165);
    assert_eq!(
        adopted.labels.get(MANAGED_BY_LABEL).map(String::as_str),
        Some(MANAGED_BY_CONFIG)
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
    let mut results = [ra.unwrap().unwrap(), rb.unwrap().unwrap()];
    results.sort_by_key(|r| r.created);
    assert_eq!(
        results[1],
        LoadResult {
            created: 5,
            updated: 0,
            deleted: 0
        },
        "one replica applied the seed: {results:?}"
    );
    assert_eq!(
        results[0],
        LoadResult {
            created: 0,
            updated: 5,
            deleted: 0
        },
        "the other found it applied: {results:?}"
    );
    assert_eq!(closed_versions(&db, "configuration_object").await, 0);

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
