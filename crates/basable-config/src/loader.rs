//! The loader: a seed directory for one environment, applied in one
//! transaction. Objects may also be created at runtime (managed-by
//! `runtime`); the loader matches by natural key and adopts such an object
//! — stamping `managed-by=config` — the first time it appears in the files,
//! after which it is managed declaratively.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::ConfigError;
use crate::item::{Environment, Item, ItemName, Operation, read_items};
use crate::reference::{resolve_refs, topo_sort, validate_deps};
use crate::store::{self, MANAGED_BY_CONFIG};
use crate::types::{ConfigTypes, NAMESPACE_TYPE};

/// Applies declarative configuration from seed files into the
/// `basable_config` schema.
pub struct Loader {
    pool: PgPool,
    types: Arc<ConfigTypes>,
}

/// What one load did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadResult {
    /// Objects that did not exist.
    pub created: usize,
    /// Objects whose spec or labels changed (a history row each).
    pub updated: usize,
    /// Objects already exactly as declared: nothing written.
    pub unchanged: usize,
    /// Objects removed by an `operation: delete` item.
    pub deleted: usize,
}

impl Loader {
    /// A loader over the pool that writes the config schema (the `app`
    /// login: nanoservice roles hold `SELECT` only).
    pub fn new(pool: PgPool, types: Arc<ConfigTypes>) -> Loader {
        Loader { pool, types }
    }

    /// Reads every `*.json` under `dir` whose filename scope includes `env`,
    /// checks that every dependency is declared in the same file set,
    /// orders the items so dependencies come first, and reconciles the
    /// desired state into the database in ONE transaction, under a table
    /// lock that serialises concurrent loads (replicas booting together).
    /// Objects present in the database but absent from the files are left
    /// untouched (no prune); only items carrying `operation: delete` are
    /// removed.
    pub async fn load(&self, dir: &Path, env: Environment) -> Result<LoadResult, ConfigError> {
        let items = read_items(&self.types, dir, env)?;
        validate_deps(&items)?;
        let ordered = topo_sort(&items)?;

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ConfigError::sql("begin load", e))?;
        // The lock conflicts with itself and with row writers, not with
        // readers: a second replica's load waits for this one and then
        // finds everything unchanged.
        sqlx::query("LOCK TABLE basable_config.configuration_object IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(|e| ConfigError::sql("lock configuration_object", e))?;

        let mut run = Run {
            types: &self.types,
            resolved: BTreeMap::new(),
            result: LoadResult::default(),
        };
        for item in ordered {
            run.apply(&mut tx, item).await?;
        }
        tx.commit()
            .await
            .map_err(|e| ConfigError::sql("commit load", e))?;
        tracing::info!(
            dir = %dir.display(),
            env = %env,
            created = run.result.created,
            updated = run.result.updated,
            unchanged = run.result.unchanged,
            deleted = run.result.deleted,
            "config seed loaded"
        );
        Ok(run.result)
    }
}

struct Run<'a> {
    types: &'a ConfigTypes,
    resolved: BTreeMap<ItemName, Uuid>,
    result: LoadResult,
}

impl Run<'_> {
    async fn apply(&mut self, tx: &mut PgConnection, item: &Item) -> Result<(), ConfigError> {
        let at = format!("{} ({})", item.name, item.source_file);
        let binder = self
            .types
            .binder(item.type_info.id)
            .ok_or_else(|| ConfigError::UnknownType(item.type_info.name.to_owned()))?;

        // Namespace membership: a namespace is root-scoped; everything else
        // lives in one declared earlier in this run.
        let namespace_id = if item.type_info.id == NAMESPACE_TYPE.id {
            None
        } else {
            Some(self.resolve(&ItemName::namespace(item.name.namespace.clone()))?)
        };

        let existing =
            store::lookup_id(&mut *tx, item.type_info.id, namespace_id, &item.name.name).await?;

        if item.operation == Operation::Delete {
            let Some(id) = existing else {
                return Ok(());
            };
            let Some(row) = store::read_for_update(tx, id).await? else {
                return Ok(());
            };
            binder
                .remove(tx, id)
                .await
                .map_err(|source| ConfigError::Binder {
                    item: at.clone(),
                    source,
                })?;
            store::delete(tx, &row).await?;
            self.result.deleted += 1;
            return Ok(());
        }

        // Resolve #{…} references to ids, then canonicalise through the
        // type's message so the stored spec is what the type decodes.
        let resolved = resolve_refs(&item.spec, &mut |r| {
            self.resolve(r).map(|id| id.to_string())
        })?;
        let spec = binder
            .canonicalize(resolved)
            .map_err(|source| ConfigError::Decode {
                item: at.clone(),
                source,
            })?;
        let labels = store::stamped(&item.labels, MANAGED_BY_CONFIG);

        let id = match existing {
            Some(id) => {
                let row = store::read_for_update(tx, id)
                    .await?
                    .ok_or_else(|| ConfigError::Conflict(at.clone()))?;
                if store::update(tx, &row, &labels, &spec).await? {
                    self.result.updated += 1;
                } else {
                    self.result.unchanged += 1;
                }
                id
            }
            None => {
                let id = Uuid::new_v4();
                store::insert(
                    tx,
                    id,
                    item.type_info.id,
                    namespace_id,
                    &item.name.name,
                    &labels,
                    &spec,
                )
                .await?;
                self.result.created += 1;
                id
            }
        };
        binder
            .apply(tx, id, spec)
            .await
            .map_err(|source| ConfigError::Binder { item: at, source })?;
        self.resolved.insert(item.name.clone(), id);
        Ok(())
    }

    /// The id of an object applied earlier this run. References resolve ONLY
    /// to objects declared in the files; the order guarantees every
    /// dependency was applied before its dependent, and `validate_deps`
    /// already rejected dangling references, so reaching the error means a
    /// target that failed to apply.
    fn resolve(&self, r: &ItemName) -> Result<Uuid, ConfigError> {
        self.resolved
            .get(r)
            .copied()
            .ok_or_else(|| ConfigError::Reference {
                item: r.to_string(),
                reason: "unresolved reference: not declared in the config files".into(),
            })
    }
}

/// Applies the seed in `dir` (baked into the image at `/config/base`) for
/// `env`, or does nothing when the directory is absent (tests, the binary
/// run outside the image). Concurrent loads by replicas booting together
/// serialise on the loader's table lock; the load itself is idempotent by
/// natural key. Returns `None` when the directory is absent.
pub async fn load_seed(
    pool: &PgPool,
    types: Arc<ConfigTypes>,
    dir: &Path,
    env: Environment,
) -> Result<Option<LoadResult>, ConfigError> {
    if !dir.is_dir() {
        tracing::info!(dir = %dir.display(), "config seed dir absent, skipping seed load");
        return Ok(None);
    }
    Loader::new(pool.clone(), types)
        .load(dir, env)
        .await
        .map(Some)
}

/// The canonical spec of `value` under `types`' binder for the type, for a
/// test or a tool that wants to compare against what the loader would store.
pub fn canonical_spec(
    types: &ConfigTypes,
    type_id: i16,
    value: Value,
) -> Result<Value, ConfigError> {
    let binder = types
        .binder(type_id)
        .ok_or_else(|| ConfigError::UnknownType(type_id.to_string()))?;
    binder
        .canonicalize(value)
        .map_err(|source| ConfigError::Decode {
            item: format!("type {type_id}"),
            source,
        })
}
