//! The runtime access layer over the config schema: reads of any registered
//! type through its binder, and the programmatic write, stamped
//! `managed-by=runtime` where the loader stamps `config`.

use std::any::Any;
use std::sync::Arc;

use basable_core::labels::Labels;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ConfigError;
use crate::store::{self, BaseRow, MANAGED_BY_RUNTIME};
use crate::types::{ConfigMessage, ConfigTypes, NAMESPACE_TYPE, TypeInfo};

/// A configuration object, hydrated: the base identity beside the message
/// its binder read back.
#[derive(Debug, Clone, PartialEq)]
pub struct Object<M> {
    /// The id.
    pub id: Uuid,
    /// The public id (`<prefix>_…`).
    pub external_id: String,
    /// The type.
    pub type_info: TypeInfo,
    /// The name.
    pub name: String,
    /// The namespace the object is filed under; the object's own id for a
    /// namespace.
    pub namespace_id: Uuid,
    /// The labels, including the managed-by marker.
    pub labels: Labels,
    /// The message, as the binder read it (its header is not filled: the
    /// identity is beside it).
    pub message: M,
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

    /// The object with the natural key, as its binder's message.
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

    /// The object with id `id`; `None` when absent or of another type.
    pub async fn get_by_id<M: ConfigMessage>(
        &self,
        info: TypeInfo,
        id: Uuid,
    ) -> Result<Option<Object<M>>, ConfigError> {
        match store::read(&self.pool, id).await? {
            Some(row) if row.type_id == info.id => self.hydrate(info, row).await.map(Some),
            _ => Ok(None),
        }
    }

    /// Every object of the type, within `namespace_id` when given, by name.
    pub async fn list<M: ConfigMessage>(
        &self,
        info: TypeInfo,
        namespace_id: Option<Uuid>,
    ) -> Result<Vec<Object<M>>, ConfigError> {
        let mut out = Vec::new();
        for row in store::list(&self.pool, info.id, namespace_id).await? {
            out.push(self.hydrate(info, row).await?);
        }
        Ok(out)
    }

    /// Creates or updates the object with the natural key from `msg` (its
    /// reference fields already ids; its header's labels the object's), and
    /// returns its id. Stamped `managed-by=runtime`. `namespace_id` is
    /// `None` for a namespace.
    pub async fn upsert<M: ConfigMessage>(
        &self,
        info: TypeInfo,
        namespace_id: Option<Uuid>,
        name: &str,
        msg: &M,
    ) -> Result<Uuid, ConfigError> {
        let binder = self
            .types
            .binder(info.id)
            .ok_or_else(|| ConfigError::UnknownType(info.name.to_owned()))?;
        let at = format!("{}:{name}", info.name);
        let body: Value = serde_json::to_value(msg).map_err(|e| ConfigError::Decode {
            item: at.clone(),
            source: Box::new(e),
        })?;
        let header = binder
            .decode_header(&body)
            .map_err(|e| ConfigError::Decode {
                item: at.clone(),
                source: e,
            })?;
        let labels = store::stamped(&header.labels, MANAGED_BY_RUNTIME);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ConfigError::sql("begin upsert", e))?;
        let existing = store::lookup_id(&mut *tx, info.id, namespace_id, name).await?;
        let id = match existing {
            Some(id) => {
                store::update_labels(&mut tx, id, &labels).await?;
                id
            }
            None => {
                let id = Uuid::new_v4();
                store::insert(
                    &mut tx,
                    &BaseRow {
                        id,
                        external_id: basable_publicid::encode(info.prefix, id),
                        type_id: info.id,
                        name: name.to_owned(),
                        namespace_id: namespace_id.unwrap_or(id),
                        labels,
                    },
                )
                .await?;
                id
            }
        };
        binder
            .apply(&mut tx, id, body)
            .await
            .map_err(|source| ConfigError::Binder { item: at, source })?;
        tx.commit()
            .await
            .map_err(|e| ConfigError::sql("commit upsert", e))?;
        Ok(id)
    }

    /// Removes the object with the natural key (its subtype rows through
    /// the binder, then the base row; the trigger keeps the final state in
    /// history). Returns whether it existed; a binder's refusal
    /// (`is_still_referenced`) is the error.
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
        binder
            .remove(&mut tx, id)
            .await
            .map_err(|source| ConfigError::Binder {
                item: format!("{}:{name}", info.name),
                source,
            })?;
        store::delete(&mut tx, id).await?;
        tx.commit()
            .await
            .map_err(|e| ConfigError::sql("commit delete", e))?;
        Ok(true)
    }

    async fn hydrate<M: ConfigMessage>(
        &self,
        info: TypeInfo,
        row: BaseRow,
    ) -> Result<Object<M>, ConfigError> {
        let binder = self
            .types
            .binder(info.id)
            .ok_or_else(|| ConfigError::UnknownType(info.name.to_owned()))?;
        let at = format!("{}:{}", info.name, row.name);
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| ConfigError::sql(format!("read {at}"), e))?;
        let erased: Box<dyn Any + Send> = binder
            .read(&mut conn, row.id)
            .await
            .map_err(|source| ConfigError::Binder {
                item: at.clone(),
                source,
            })?
            .ok_or_else(|| ConfigError::Sql {
                op: format!("read {at}"),
                source: sqlx::Error::RowNotFound,
            })?;
        let message = *erased.downcast::<M>().map_err(|_| ConfigError::Decode {
            item: at.clone(),
            source: format!(
                "the registered binder for {} reads a different message type than {}",
                info.name,
                std::any::type_name::<M>()
            )
            .into(),
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
            external_id: row.external_id,
            type_info: info,
            name: row.name,
            namespace_id: row.namespace_id,
            labels,
            message,
        })
    }
}
