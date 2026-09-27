//! The runtime access layer over the config schema: typed reads of any
//! registered type, and the rare programmatic write, stamped
//! `managed-by=runtime` where the loader stamps `config`.

use std::sync::Arc;

use basable_core::labels::Labels;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ConfigError;
use crate::store::{self, BaseRow, MANAGED_BY_RUNTIME};
use crate::types::{ConfigMessage, ConfigTypes, NAMESPACE_TYPE, TypeInfo};

/// A configuration object, hydrated: the base identity plus its spec
/// decoded as the type's message.
#[derive(Debug, Clone, PartialEq)]
pub struct Object<M> {
    /// The id.
    pub id: Uuid,
    /// The public id (`<prefix>_…`).
    pub public_id: String,
    /// The type.
    pub type_info: TypeInfo,
    /// The namespace the object is filed under; `None` for a namespace.
    pub namespace_id: Option<Uuid>,
    /// The name.
    pub name: String,
    /// The labels, including the managed-by marker.
    pub labels: Labels,
    /// The version, 1 at creation and advanced by every change.
    pub version: i64,
    /// The spec.
    pub spec: M,
}

/// Reads and writes over the config schema. Reads work over any pool that
/// may `SELECT` the schema (every nanoservice role); writes need the `app`
/// login's pool.
#[derive(Clone)]
pub struct Repository {
    pool: PgPool,
    types: Arc<ConfigTypes>,
}

impl Repository {
    /// A repository over `pool`.
    pub fn new(pool: PgPool, types: Arc<ConfigTypes>) -> Repository {
        Repository { pool, types }
    }

    /// The registry.
    pub fn types(&self) -> &ConfigTypes {
        &self.types
    }

    /// The id of the object with the natural key, if it exists.
    pub async fn lookup_id(
        &self,
        info: TypeInfo,
        namespace_id: Option<Uuid>,
        name: &str,
    ) -> Result<Option<Uuid>, ConfigError> {
        store::lookup_id(&self.pool, info.id, namespace_id, name).await
    }

    /// The id of the namespace named `name`, if it exists.
    pub async fn namespace_id(&self, name: &str) -> Result<Option<Uuid>, ConfigError> {
        store::lookup_id(&self.pool, NAMESPACE_TYPE.id, None, name).await
    }

    /// The object with the natural key, decoded as `M`.
    pub async fn get<M: ConfigMessage>(
        &self,
        info: TypeInfo,
        namespace_id: Option<Uuid>,
        name: &str,
    ) -> Result<Option<Object<M>>, ConfigError> {
        let Some(id) = self.lookup_id(info, namespace_id, name).await? else {
            return Ok(None);
        };
        self.get_by_id(info, id).await
    }

    /// The object with id `id`, decoded as `M`; `None` when absent or of
    /// another type.
    pub async fn get_by_id<M: ConfigMessage>(
        &self,
        info: TypeInfo,
        id: Uuid,
    ) -> Result<Option<Object<M>>, ConfigError> {
        match store::read(&self.pool, id).await? {
            Some(row) if row.type_id == info.id => hydrate(info, row).map(Some),
            _ => Ok(None),
        }
    }

    /// Every object of the type, within `namespace_id` when given, by name.
    pub async fn list<M: ConfigMessage>(
        &self,
        info: TypeInfo,
        namespace_id: Option<Uuid>,
    ) -> Result<Vec<Object<M>>, ConfigError> {
        store::list(&self.pool, info.id, namespace_id)
            .await?
            .into_iter()
            .map(|row| hydrate(info, row))
            .collect()
    }

    /// Creates or updates the object with the natural key from `msg` (its
    /// reference fields already ids) and returns its id. Stamped
    /// `managed-by=runtime`; an unchanged write is a no-op.
    pub async fn upsert<M: ConfigMessage>(
        &self,
        info: TypeInfo,
        namespace_id: Option<Uuid>,
        name: &str,
        labels: &Labels,
        msg: &M,
    ) -> Result<Uuid, ConfigError> {
        let binder = self
            .types
            .binder(info.id)
            .ok_or_else(|| ConfigError::UnknownType(info.name.to_owned()))?;
        let at = format!("{}:{name}", info.name);
        let spec = serde_json::to_value(msg)
            .map_err(|e| ConfigError::Decode {
                item: at.clone(),
                source: Box::new(e),
            })
            .and_then(|v| {
                binder
                    .canonicalize(v)
                    .map_err(|source| ConfigError::Decode {
                        item: at.clone(),
                        source,
                    })
            })?;
        let labels = store::stamped(labels, MANAGED_BY_RUNTIME);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ConfigError::sql("begin upsert", e))?;
        let existing = store::lookup_id(&mut *tx, info.id, namespace_id, name).await?;
        let id = match existing {
            Some(id) => {
                let row = store::read_for_update(&mut tx, id)
                    .await?
                    .ok_or_else(|| ConfigError::Conflict(at.clone()))?;
                store::update(&mut tx, &row, &labels, &spec).await?;
                id
            }
            None => {
                let id = Uuid::new_v4();
                store::insert(&mut tx, id, info.id, namespace_id, name, &labels, &spec).await?;
                id
            }
        };
        binder
            .apply(&mut tx, id, spec)
            .await
            .map_err(|source| ConfigError::Binder { item: at, source })?;
        tx.commit()
            .await
            .map_err(|e| ConfigError::sql("commit upsert", e))?;
        Ok(id)
    }

    /// Removes the object with the natural key (its subtype rows, then the
    /// base row, with its last version recorded). Returns whether it
    /// existed.
    pub async fn delete(
        &self,
        info: TypeInfo,
        namespace_id: Option<Uuid>,
        name: &str,
    ) -> Result<bool, ConfigError> {
        let binder = self
            .types
            .binder(info.id)
            .ok_or_else(|| ConfigError::UnknownType(info.name.to_owned()))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ConfigError::sql("begin delete", e))?;
        let Some(id) = store::lookup_id(&mut *tx, info.id, namespace_id, name).await? else {
            return Ok(false);
        };
        let Some(row) = store::read_for_update(&mut tx, id).await? else {
            return Ok(false);
        };
        binder
            .remove(&mut tx, id)
            .await
            .map_err(|source| ConfigError::Binder {
                item: format!("{}:{name}", info.name),
                source,
            })?;
        store::delete(&mut tx, &row).await?;
        tx.commit()
            .await
            .map_err(|e| ConfigError::sql("commit delete", e))?;
        Ok(true)
    }
}

fn hydrate<M: ConfigMessage>(info: TypeInfo, row: BaseRow) -> Result<Object<M>, ConfigError> {
    let at = format!("{}:{}", info.name, row.name);
    let spec: M = serde_json::from_value(row.spec).map_err(|e| ConfigError::Decode {
        item: at.clone(),
        source: Box::new(e),
    })?;
    let labels: Labels = match row.labels {
        Value::Null => Labels::default(),
        v => serde_json::from_value(v).map_err(|e| ConfigError::Decode {
            item: at,
            source: Box::new(e),
        })?,
    };
    Ok(Object {
        id: row.id,
        public_id: basable_publicid::encode(info.prefix, row.id),
        type_info: info,
        namespace_id: row.namespace_id,
        name: row.name,
        labels,
        version: row.version,
        spec,
    })
}
