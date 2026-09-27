//! The base row: `basable_config.configuration_object`, shared by the
//! loader's reconciler and the repository's writes. History is
//! trigger-only; these touch the live table.

use basable_core::labels::Labels;
use serde_json::Value;
use sqlx::{PgConnection, Row as _};
use uuid::Uuid;

use crate::error::ConfigError;
use crate::types::NAMESPACE_TYPE;

/// The label every applied object carries, naming who manages it.
pub const MANAGED_BY_LABEL: &str = "basable.com/managed-by";
/// The loader's marker: the object is declared in the seed files, and the
/// loader prunes it when they stop declaring it.
pub const MANAGED_BY_CONFIG: &str = "config";
/// The repository's marker: the object was written at runtime. The loader
/// prunes only its own objects, so runtime-owned ones survive every load.
pub const MANAGED_BY_RUNTIME: &str = "runtime";

/// The live base row.
#[derive(Debug, Clone)]
pub(crate) struct BaseRow {
    pub id: Uuid,
    pub external_id: String,
    pub type_id: i16,
    pub name: String,
    /// Equals `id` for a namespace (the root scope self-references).
    pub namespace_id: Uuid,
    pub labels: Value,
}

fn scan(row: &sqlx::postgres::PgRow) -> Result<BaseRow, sqlx::Error> {
    Ok(BaseRow {
        id: row.try_get("id")?,
        external_id: row.try_get("external_id")?,
        type_id: row.try_get("configuration_object_type_id")?,
        name: row.try_get("name")?,
        namespace_id: row.try_get("namespace_id")?,
        labels: row
            .try_get::<Option<Value>, _>("labels")?
            .unwrap_or(Value::Null),
    })
}

const COLUMNS: &str = "id, external_id, configuration_object_type_id, name, namespace_id, labels";

/// The labels an applied object stores: the header's with the managed-by
/// marker stamped over any user-supplied value — it is a system-owned
/// label. The canonical JSON of a sorted map.
pub(crate) fn stamped(labels: &Labels, managed_by: &str) -> Value {
    let mut all = labels.clone();
    all.insert(MANAGED_BY_LABEL.to_owned(), managed_by.to_owned());
    serde_json::to_value(all).expect("a string map is JSON")
}

/// The id of the current object with the natural key. Namespaces are
/// root-scoped: their id is unknown here yet they self-reference, so they
/// match on `(type, name)` alone, globally unique by the partial index.
/// Every other type requires a namespace id: a `None` is a caller-side
/// signal that only ever accompanies the namespace type, and the
/// `(type, name)` match is only unique for that type, so it is rejected
/// instead of returning an arbitrary row.
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
             WHERE configuration_object_type_id = $1 AND name = $2",
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
             WHERE configuration_object_type_id = $1 AND name = $2 AND namespace_id = $3",
        )
        .bind(type_id)
        .bind(name)
        .bind(ns)
        .fetch_optional(exec)
        .await
    }
    .map_err(|e| ConfigError::sql(op.clone(), e))?;
    row.map(|r| r.try_get("id"))
        .transpose()
        .map_err(|e| ConfigError::sql(op, e))
}

/// The live row for `id`.
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
         WHERE configuration_object_type_id = $1 AND ($2::uuid IS NULL OR namespace_id = $2)
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

/// Every loader-managed object: `(id, type id, name)`.
pub(crate) async fn list_managed(
    tx: &mut PgConnection,
) -> Result<Vec<(Uuid, i16, String)>, ConfigError> {
    let selector = serde_json::json!({ MANAGED_BY_LABEL: MANAGED_BY_CONFIG });
    let rows = sqlx::query(
        "SELECT id, configuration_object_type_id, name
         FROM basable_config.configuration_object
         WHERE labels @> $1::jsonb",
    )
    .bind(selector)
    .fetch_all(tx)
    .await
    .map_err(|e| ConfigError::sql("list loader-managed objects", e))?;
    rows.iter()
        .map(|r| {
            Ok((
                r.try_get("id")?,
                r.try_get("configuration_object_type_id")?,
                r.try_get("name")?,
            ))
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(|e| ConfigError::sql("scan loader-managed object", e))
}

/// Inserts a new base row. `namespace_id` is NOT NULL: a namespaced object
/// carries its namespace's id, a namespace its own.
pub(crate) async fn insert(tx: &mut PgConnection, row: &BaseRow) -> Result<(), ConfigError> {
    sqlx::query(
        "INSERT INTO basable_config.configuration_object
             (id, external_id, configuration_object_type_id, name, namespace_id, labels)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(row.id)
    .bind(&row.external_id)
    .bind(row.type_id)
    .bind(&row.name)
    .bind(row.namespace_id)
    .bind(&row.labels)
    .execute(tx)
    .await
    .map(|_| ())
    .map_err(|e| ConfigError::sql(format!("insert configuration_object {}", row.name), e))
}

/// Refreshes an existing object's mutable base state. The loader always
/// includes the managed-by marker, so writing labels here is what adopts
/// an object first created at runtime into config management. Name,
/// namespace, type and external id are identity and never change; the
/// versioning trigger no-ops when nothing actually changed.
pub(crate) async fn update_labels(
    tx: &mut PgConnection,
    id: Uuid,
    labels: &Value,
) -> Result<(), ConfigError> {
    sqlx::query("UPDATE basable_config.configuration_object SET labels = $2 WHERE id = $1")
        .bind(id)
        .bind(labels)
        .execute(tx)
        .await
        .map(|_| ())
        .map_err(|e| ConfigError::sql(format!("update configuration_object {id}"), e))
}

/// Removes the base row (after its subtype rows); the trigger keeps its
/// final state in history.
pub(crate) async fn delete(tx: &mut PgConnection, id: Uuid) -> Result<(), ConfigError> {
    sqlx::query("DELETE FROM basable_config.configuration_object WHERE id = $1")
        .bind(id)
        .execute(tx)
        .await
        .map(|_| ())
        .map_err(|e| ConfigError::sql(format!("delete configuration_object {id}"), e))
}
