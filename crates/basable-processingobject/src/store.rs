//! The store core and the creation path: [`TypedStore`] (the per-type
//! handle every caller works through), `bind` (declaration validation plus
//! a fail-fast schema check), and `create` — the only INSERT path in the
//! framework. Envelope, typed spec and typed status are born in one
//! transaction (invariant 1), which is what lets every other path treat
//! "envelope exists but typed row missing" as a violation instead of a case
//! to handle.
//!
//! There is deliberately no process-wide store and no in-process type
//! registry: type keys and names are static data declared in code and
//! checked against the database's registry table at bind. A nanoservice
//! constructs a handle for each type it owns over its own pool.
//!
//! Scheduling note: `create` never touches `next_reconcile_at`. `due_at` is
//! generated from `observed_generation < generation`, so a new object
//! (generation 1, observed 0) is due by construction.

use basable_core::labels::{Labels, validate_labels};
use basable_db::sqlstate;
use basable_db::{NanoPool, Stateful};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool, Row as _};
use uuid::Uuid;

use crate::decl::{Adapter, ProcessingObjectType};
use crate::error::Error;
use crate::model::{NamespacedName, Ref};
use crate::tx::Tx;

/// The `pg_notify` channel carrying wake hints. The payload is the type
/// name. Notifications are a latency hint only — polling remains the
/// correctness path — and they fire on commit, never for rolled-back work.
pub const WAKE_CHANNEL: &str = "processing_object_wake";

/// The handle for one processing-object type, bound at boot by the owning
/// nanoservice over its [`NanoPool`].
pub struct TypedStore<S, T, A: Adapter<S, T>> {
    pub(crate) pool: PgPool,
    pub(crate) decl: ProcessingObjectType<S, T, A>,
    /// `processing_object_<name>`, the partition every statement targets.
    pub(crate) partition: String,
}

impl<S, T, A: Adapter<S, T>> std::fmt::Debug for TypedStore<S, T, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedStore")
            .field("type", &self.decl.name)
            .field("key", &self.decl.type_key)
            .finish()
    }
}

impl<S, T, A> TypedStore<S, T, A>
where
    S: Send + Sync + 'static,
    T: Send + Sync + 'static,
    A: Adapter<S, T>,
{
    /// Validates the declaration, verifies the database knows this type —
    /// the registry row under the declared key and name, the partition
    /// reachable — and returns the handle. It fails at boot rather than
    /// letting the first create discover a missing migration.
    pub async fn bind<N: Stateful>(
        pool: &NanoPool<N>,
        decl: ProcessingObjectType<S, T, A>,
    ) -> Result<TypedStore<S, T, A>, Error> {
        decl.validate()?;
        let partition = decl.partition();
        verify_type_schema(
            pool.pool(),
            decl.type_key,
            decl.name,
            decl.public_id_prefix,
            &partition,
        )
        .await?;
        Ok(TypedStore {
            pool: pool.pool().clone(),
            decl,
            partition,
        })
    }

    /// The registry name of the handle's type.
    pub fn name(&self) -> &'static str {
        self.decl.name
    }

    /// The registry key of the handle's type.
    pub fn type_key(&self) -> i16 {
        self.decl.type_key
    }

    /// A reference to an object of the handle's type.
    pub fn r#ref(&self, id: Uuid) -> Ref {
        Ref::new(self.decl.name, id)
    }

    /// The public id an object of this type carries as its `external_id`:
    /// deterministic, usable before, during and after the object's lifetime.
    pub fn public_id(&self, id: Uuid) -> String {
        basable_publicid::encode(self.decl.public_id_prefix, id)
    }

    /// Validates a caller-supplied ref against the handle's type.
    pub(crate) fn check_ref(&self, r: &Ref) -> Result<(), Error> {
        r.validate()?;
        if r.processing_object_type != self.decl.name {
            return Err(Error::invalid(format!(
                "ref {r} used with the {:?} typed store",
                self.decl.name
            )));
        }
        Ok(())
    }

    /// Inserts envelope, typed spec and typed status in one transaction
    /// through the adapter, and publishes a wake. The id is client-minted:
    /// it is the adoption handle that makes creation safe to retry across an
    /// ambiguous commit (invariant 5). The `external_id` is always derived
    /// from it, never supplied.
    ///
    /// A create that finds the `(namespace, name)` pair held by another live
    /// object of the type fails with [`Error::NameTaken`]; a deleting object
    /// is no longer a live holder. If the id already exists, the earlier
    /// attempt's commit landed and this call adopts it, writing nothing —
    /// provided it presents the identity the object was born with, name AND
    /// labels (a mismatch is [`Error::InvalidConfig`]: an ambiguous-commit
    /// retry must never believe labels landed that did not). An existing
    /// object that is already deleting is [`Error::Deleting`].
    ///
    /// Domain natural keys are constraints on the typed spec table, so a
    /// concurrent-create race for the same natural key surfaces as the
    /// adapter's unique violation ([`Error::Sql`]) for the owning
    /// nanoservice to classify.
    pub async fn create(
        &self,
        id: Uuid,
        name: NamespacedName,
        spec: &S,
        status: &T,
        opts: CreateOptions,
    ) -> Result<Ref, Error> {
        if id.is_nil() {
            return Err(Error::invalid("create requires a client-minted id"));
        }
        name.validate()?;
        let r = self.r#ref(id);
        validate_labels(&opts.labels)
            .map_err(|e| Error::invalid(format!("create {r}: labels: {e}")))?;
        let op = |what: &str| format!("create {r}: {what}");

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Error::sql(op("begin"), e))?;
        // An empty map stores NULL, preserving the labels CHECK's semantics
        // and the GIN index's partial predicate.
        let labels: Option<Json<&Labels>> = if opts.labels.is_empty() {
            None
        } else {
            Some(Json(&opts.labels))
        };
        let inserted = sqlx::query(&format!(
            "INSERT INTO {} (processing_object_type_key, id, external_id, name, namespace, labels)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (processing_object_type_key, id) DO NOTHING",
            self.partition
        ))
        .bind(self.decl.type_key)
        .bind(id)
        .bind(self.public_id(id))
        .bind(&name.name)
        .bind(name.namespace)
        .bind(labels)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if sqlstate::is_unique_violation(&e) {
                // The envelope INSERT can raise no other unique violation:
                // the primary key is the ON CONFLICT arbiter and external_id
                // is a bijection of the id.
                Error::NameTaken {
                    r: r.clone(),
                    name: name.clone(),
                }
            } else {
                Error::sql(op("insert envelope"), e)
            }
        })?;

        if inserted.rows_affected() == 0 {
            // The id exists: our own earlier create. Adopt it — unless it is
            // already being torn down or was born under a different identity.
            let row = sqlx::query(&format!(
                "SELECT deleted_at, name, namespace, labels FROM {} WHERE id = $1",
                self.partition
            ))
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| Error::sql(op("inspect existing"), e))?;
            let deleted_at: Option<chrono::DateTime<chrono::Utc>> = row
                .try_get("deleted_at")
                .map_err(|e| Error::sql(op("inspect existing"), e))?;
            let stored = NamespacedName {
                namespace: row
                    .try_get("namespace")
                    .map_err(|e| Error::sql(op("inspect existing"), e))?,
                name: row
                    .try_get("name")
                    .map_err(|e| Error::sql(op("inspect existing"), e))?,
            };
            let stored_labels: Option<Json<Labels>> = row
                .try_get("labels")
                .map_err(|e| Error::sql(op("inspect existing"), e))?;
            let stored_labels = stored_labels.map(|j| j.0).unwrap_or_default();
            if deleted_at.is_some() {
                return Err(Error::Deleting(r));
            }
            if stored != name {
                return Err(Error::invalid(format!(
                    "create {r}: existing object carries identity {stored}, not {name}"
                )));
            }
            if stored_labels != opts.labels {
                return Err(Error::invalid(format!(
                    "create {r}: existing object carries labels {stored_labels:?}, not {:?}",
                    opts.labels
                )));
            }
            return Ok(r);
        }

        {
            let conn: &mut PgConnection = &mut tx;
            let mut rtx = Tx::new(conn);
            self.decl
                .adapter
                .insert_spec(&mut rtx, &r, spec)
                .await
                .map_err(|e| Error::sql(op("insert typed spec"), e))?;
            self.decl
                .adapter
                .insert_status(&mut rtx, &r, status)
                .await
                .map_err(|e| Error::sql(op("insert typed status"), e))?;
        }
        publish_wake(&mut tx, self.decl.name).await?;
        tx.commit().await.map_err(|e| {
            Error::commit(format!("create {r} (retry with the same id to adopt)"), e)
        })?;
        Ok(r)
    }
}

/// Optional create behaviour.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateOptions {
    /// The envelope's labels, immutable for the object's lifetime: claim
    /// routing partitions a type's row space by them, and a label flip would
    /// move a possibly-claimed object between logical workers with
    /// different lease policies. Empty stores NULL.
    pub labels: Labels,
}

impl CreateOptions {
    /// No labels.
    pub fn none() -> CreateOptions {
        CreateOptions::default()
    }

    /// With these labels.
    pub fn labels(labels: Labels) -> CreateOptions {
        CreateOptions { labels }
    }
}

/// Checks the registry row and probes the partition with an insert that is
/// always rolled back: the exact failure a real create would hit (no
/// registry row, no partition, the wrong owner) without leaving a row.
async fn verify_type_schema(
    pool: &PgPool,
    key: i16,
    name: &str,
    prefix: &str,
    partition: &str,
) -> Result<(), Error> {
    let registered: Option<(String,)> =
        sqlx::query_as("SELECT name FROM basable.processing_object_type WHERE key = $1")
            .bind(key)
            .fetch_optional(pool)
            .await
            .map_err(|e| Error::sql(format!("verify type {name:?} registration"), e))?;
    match registered {
        None => {
            return Err(Error::invalid(format!(
                "type {name:?}: no processing_object_type row for key {key} — the type's migration has not run"
            )));
        }
        Some((db_name,)) if db_name != name => {
            return Err(Error::invalid(format!(
                "type key {key} is registered in the database as {db_name:?}, not {name:?}"
            )));
        }
        Some(_) => {}
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| Error::sql(format!("verify type {name:?} partition: begin"), e))?;
    // The probe's own fresh uuid doubles as the namespace, so the identity
    // columns are satisfied without colliding with a real pair.
    let probe = Uuid::new_v4();
    sqlx::query(&format!(
        "INSERT INTO {partition} (processing_object_type_key, id, external_id, name, namespace)
         VALUES ($1, $2, $3, 'schema-probe', $2)"
    ))
    .bind(key)
    .bind(probe)
    .bind(basable_publicid::encode(prefix, probe))
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        Error::invalid(format!(
            "type {name:?}: envelope partition {partition} for key {key} is missing or unusable: {e}"
        ))
    })?;
    tx.rollback()
        .await
        .map_err(|e| Error::sql(format!("verify type {name:?} partition: rollback"), e))?;
    Ok(())
}

/// Emits the wake hint inside the transaction, so it reaches listeners
/// exactly when the transaction's work becomes visible. Rule: every
/// transaction that makes an object due publishes a wake.
pub(crate) async fn publish_wake(conn: &mut PgConnection, type_name: &str) -> Result<(), Error> {
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(WAKE_CHANNEL)
        .bind(type_name)
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .map_err(|e| Error::sql(format!("publish wake for {type_name}"), e))
}
