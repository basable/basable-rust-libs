//! The base row: `basable_config.configuration_object` and its history,
//! shared by the loader and the repository. History is written here, by the
//! same transaction that changes the live row: one row per superseded
//! version, and one for a deleted object's last version. An unchanged
//! write is a no-op — no history row, no version bump.

use basable_core::labels::Labels;
use serde_json::Value;
use sqlx::{PgConnection, Row as _};
use uuid::Uuid;

use crate::error::ConfigError;
use crate::types::NAMESPACE_TYPE;

/// The label every applied object carries, naming who manages it.
pub const MANAGED_BY_LABEL: &str = "basable.com/managed-by";
/// The loader's marker: the object is declared in the seed files.
pub const MANAGED_BY_CONFIG: &str = "config";
/// The repository's marker: the object was written at runtime. The loader
/// never prunes, so runtime-owned objects survive a load; the label lets
/// tooling tell the two origins apart.
pub const MANAGED_BY_RUNTIME: &str = "runtime";

/// The live base row.
#[derive(Debug, Clone)]
pub(crate) struct BaseRow {
    pub id: Uuid,
    pub type_id: i16,
    pub namespace_id: Option<Uuid>,
    pub name: String,
    pub labels: Value,
    pub spec: Value,
    pub version: i64,
}

fn scan(row: &sqlx::postgres::PgRow) -> Result<BaseRow, sqlx::Error> {
    Ok(BaseRow {
        id: row.try_get("id")?,
        type_id: row.try_get("type_id")?,
        namespace_id: row.try_get("namespace_id")?,
        name: row.try_get("name")?,
        labels: row.try_get("labels")?,
        spec: row.try_get("spec")?,
        version: row.try_get("version")?,
    })
}

const COLUMNS: &str = "id, type_id, namespace_id, name, labels, spec, version";

/// The labels an applied object stores: the declared ones with the
/// managed-by marker stamped over any user-supplied value — it is a
/// system-owned label.
pub(crate) fn stamped(labels: &Labels, managed_by: &str) -> Value {
    let mut all = labels.clone();
    all.insert(MANAGED_BY_LABEL.to_owned(), managed_by.to_owned());
    serde_json::to_value(all).expect("a string map is JSON")
}

/// The id of the live object with the natural key. Namespaces are
/// root-scoped and match on `(type, name)` with a NULL namespace; every
/// other type requires a namespace id, since the `(type, name)` match is
/// only unique for namespaces.
pub(crate) async fn lookup_id<'e, E>(
    exec: E,
    type_id: i16,
    namespace_id: Option<Uuid>,
    name: &str,
) -> Result<Option<Uuid>, ConfigError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let op = format!("lookup {name} (type {type_id})");
    let row = if type_id == NAMESPACE_TYPE.id {
        sqlx::query(
            "SELECT id FROM basable_config.configuration_object
             WHERE type_id = $1 AND name = $2 AND namespace_id IS NULL",
        )
        .bind(type_id)
        .bind(name)
        .fetch_optional(exec)
        .await
    } else {
        let Some(ns) = namespace_id else {
            return Err(ConfigError::Sql {
                op,
                source: sqlx::Error::Protocol("a namespaced type requires a namespace id".into()),
            });
        };
        sqlx::query(
            "SELECT id FROM basable_config.configuration_object
             WHERE type_id = $1 AND namespace_id = $2 AND name = $3",
        )
        .bind(type_id)
        .bind(ns)
        .bind(name)
        .fetch_optional(exec)
        .await
    }
    .map_err(|e| ConfigError::sql(op.clone(), e))?;
    row.map(|r| r.try_get("id"))
        .transpose()
        .map_err(|e| ConfigError::sql(op, e))
}

/// The live row for `id`, locked for update.
pub(crate) async fn read_for_update(
    tx: &mut PgConnection,
    id: Uuid,
) -> Result<Option<BaseRow>, ConfigError> {
    let op = format!("read configuration_object {id}");
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM basable_config.configuration_object WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(tx)
    .await
    .map_err(|e| ConfigError::sql(op.clone(), e))?
    .map(|r| scan(&r))
    .transpose()
    .map_err(|e| ConfigError::sql(op, e))
}

/// Reads the live row for `id`.
pub(crate) async fn read<'e, E>(exec: E, id: Uuid) -> Result<Option<BaseRow>, ConfigError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let op = format!("read configuration_object {id}");
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM basable_config.configuration_object WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(exec)
    .await
    .map_err(|e| ConfigError::sql(op.clone(), e))?
    .map(|r| scan(&r))
    .transpose()
    .map_err(|e| ConfigError::sql(op, e))
}

/// Every live row of a type, optionally within one namespace, by name.
pub(crate) async fn list<'e, E>(
    exec: E,
    type_id: i16,
    namespace_id: Option<Uuid>,
) -> Result<Vec<BaseRow>, ConfigError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let op = format!("list configuration_object type {type_id}");
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM basable_config.configuration_object
         WHERE type_id = $1 AND ($2::uuid IS NULL OR namespace_id = $2)
         ORDER BY name"
    ))
    .bind(type_id)
    .bind(namespace_id)
    .fetch_all(exec)
    .await
    .map_err(|e| ConfigError::sql(op.clone(), e))?;
    rows.iter()
        .map(scan)
        .collect::<Result<_, _>>()
        .map_err(|e| ConfigError::sql(op, e))
}

/// Inserts a new live row at version 1.
pub(crate) async fn insert(
    tx: &mut PgConnection,
    id: Uuid,
    type_id: i16,
    namespace_id: Option<Uuid>,
    name: &str,
    labels: &Value,
    spec: &Value,
) -> Result<(), ConfigError> {
    sqlx::query(
        "INSERT INTO basable_config.configuration_object
             (id, type_id, namespace_id, name, labels, spec)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(type_id)
    .bind(namespace_id)
    .bind(name)
    .bind(labels)
    .bind(spec)
    .execute(tx)
    .await
    .map(|_| ())
    .map_err(|e| ConfigError::sql(format!("insert configuration_object {name}"), e))
}

/// Updates a live row's labels and spec when either differs: the current
/// version goes to history and the row advances. Returns whether anything
/// changed. The row's version is the optimistic guard: a concurrent change
/// between the read and the write is a [`ConfigError::Conflict`].
pub(crate) async fn update(
    tx: &mut PgConnection,
    current: &BaseRow,
    labels: &Value,
    spec: &Value,
) -> Result<bool, ConfigError> {
    if &current.labels == labels && &current.spec == spec {
        return Ok(false);
    }
    record_history(tx, current).await?;
    let done = sqlx::query(
        "UPDATE basable_config.configuration_object
         SET labels = $2, spec = $3, version = version + 1, updated_at = clock_timestamp()
         WHERE id = $1 AND version = $4",
    )
    .bind(current.id)
    .bind(labels)
    .bind(spec)
    .bind(current.version)
    .execute(tx)
    .await
    .map_err(|e| ConfigError::sql(format!("update configuration_object {}", current.name), e))?;
    if done.rows_affected() == 0 {
        return Err(ConfigError::Conflict(current.name.clone()));
    }
    Ok(true)
}

/// Deletes a live row, recording its last version in history first.
pub(crate) async fn delete(tx: &mut PgConnection, current: &BaseRow) -> Result<(), ConfigError> {
    record_history(tx, current).await?;
    let done = sqlx::query(
        "DELETE FROM basable_config.configuration_object WHERE id = $1 AND version = $2",
    )
    .bind(current.id)
    .bind(current.version)
    .execute(tx)
    .await
    .map_err(|e| ConfigError::sql(format!("delete configuration_object {}", current.name), e))?;
    if done.rows_affected() == 0 {
        return Err(ConfigError::Conflict(current.name.clone()));
    }
    Ok(())
}

async fn record_history(tx: &mut PgConnection, current: &BaseRow) -> Result<(), ConfigError> {
    sqlx::query(
        "INSERT INTO basable_config.configuration_object_history (id, version, spec)
         VALUES ($1, $2, $3)",
    )
    .bind(current.id)
    .bind(current.version)
    .bind(&current.spec)
    .execute(tx)
    .await
    .map(|_| ())
    .map_err(|e| ConfigError::sql(format!("record history of {}", current.name), e))
}
