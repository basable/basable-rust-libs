//! The loader: a seed directory for one environment, applied in one
//! transaction. The files are the complete declaration of what the loader
//! manages: an object carrying `managed-by=config` that the files no longer
//! declare is deleted in the same transaction (prune). Objects written by
//! anything else — the repository, application code — carry no such label
//! and are never touched; the loader matches by natural key and adopts such
//! an object the first time it appears in the files, after which it is
//! managed declaratively. Removing an item from the files is therefore how
//! an object is deleted; there is no delete marker.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::ConfigError;
use crate::item::{Environment, Item, ItemName, read_items};
use crate::reference::{resolve_refs, topo_sort, validate_deps};
use crate::store::{self, BaseRow, MANAGED_BY_CONFIG};
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
    /// Existing objects re-applied (whether or not anything changed: the
    /// trigger decides what history records).
    pub updated: usize,
    /// Loader-managed objects the files no longer declared.
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
    /// lock that serialises concurrent loads (replicas booting together):
    /// apply every item, then prune every loader-managed object the run did
    /// not apply.
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
        // re-applies everything unchanged.
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
        run.prune(&mut tx).await?;
        tx.commit()
            .await
            .map_err(|e| ConfigError::sql("commit load", e))?;
        tracing::info!(
            dir = %dir.display(),
            env = %env,
            created = run.result.created,
            updated = run.result.updated,
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

        // Namespace membership: a namespace is its own; everything else
        // lives in one applied earlier this run.
        let namespace_id = if item.type_info.id == NAMESPACE_TYPE.id {
            None
        } else {
            Some(self.resolve(&ItemName::namespace(item.name.namespace.clone()))?)
        };
        let existing =
            store::lookup_id(&mut *tx, item.type_info.id, namespace_id, &item.name.name).await?;

        // Resolve #{…} references into id strings (header.namespace among
        // them), then decode: the header's labels go on the base row.
        let body = resolve_refs(&item.body, &mut |r| {
            self.resolve(r).map(|id| id.to_string())
        })?;
        let header = binder
            .decode_header(&body)
            .map_err(|source| ConfigError::Decode {
                item: at.clone(),
                source,
            })?;
        let labels = store::stamped(&header.labels, MANAGED_BY_CONFIG);

        let id = match existing {
            Some(id) => {
                store::update_labels(tx, id, &labels).await?;
                self.result.updated += 1;
                id
            }
            None => {
                let id = Uuid::new_v4();
                store::insert(
                    tx,
                    &BaseRow {
                        id,
                        external_id: basable_publicid::encode(item.type_info.prefix, id),
                        type_id: item.type_info.id,
                        name: item.name.name.clone(),
                        // A namespace object is its own namespace.
                        namespace_id: namespace_id.unwrap_or(id),
                        labels,
                    },
                )
                .await?;
                self.result.created += 1;
                id
            }
        };
        binder
            .apply(tx, id, body)
            .await
            .map_err(|source| ConfigError::Binder { item: at, source })?;
        self.resolved.insert(item.name.clone(), id);
        Ok(())
    }

    /// Deletes every loader-managed object this run did not apply: its item
    /// left the files. Only `managed-by=config` rows are candidates, so
    /// runtime-owned objects survive. Every foreign key into
    /// `configuration_object` cascades or nulls, so the deletions need no
    /// order; the subtype's delete runs first as it does for any removal,
    /// and history keeps the final state.
    async fn prune(&mut self, tx: &mut PgConnection) -> Result<(), ConfigError> {
        let applied: HashSet<Uuid> = self.resolved.values().copied().collect();
        let mut gone: Vec<(Uuid, i16, String)> = store::list_managed(tx)
            .await?
            .into_iter()
            .filter(|(id, _, _)| !applied.contains(id))
            .collect();
        // Namespaces last: deleting one cascades to its members, which must
        // go through their own binders first.
        gone.sort_by_key(|(_, type_id, _)| *type_id == NAMESPACE_TYPE.id);

        // A binder may refuse while another object still references the
        // row (a contact an organisation names). The refusal is a read, not
        // a failed statement, so the transaction stays usable: delete what
        // can go, retry the refused ones, and fail only when a pass frees
        // nothing — then something the files keep, or a runtime object,
        // still points at it.
        while !gone.is_empty() {
            let mut refused = Vec::new();
            let mut first_refusal: Option<ConfigError> = None;
            let deleted_before = self.result.deleted;
            for (id, type_id, name) in std::mem::take(&mut gone) {
                let type_info = self.types.type_by_id(type_id).ok_or_else(|| {
                    ConfigError::UnknownType(format!("id {type_id} (prune {name})"))
                })?;
                let binder = self
                    .types
                    .binder(type_id)
                    .ok_or_else(|| ConfigError::UnknownType(type_info.name.to_owned()))?;
                let at = format!("prune {}:{name}", type_info.name);
                match binder.remove(tx, id).await {
                    Ok(()) => {}
                    Err(source) => {
                        let err = ConfigError::Binder { item: at, source };
                        if err.is_still_referenced() {
                            refused.push((id, type_id, name));
                            first_refusal.get_or_insert(err);
                            continue;
                        }
                        return Err(err);
                    }
                }
                store::delete(tx, id).await?;
                self.result.deleted += 1;
            }
            if let Some(err) = first_refusal
                && self.result.deleted == deleted_before
            {
                return Err(err);
            }
            gone = refused;
        }
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
