//! The read path: `read` (one object) and `read_many` (a batch). Reads are
//! pure — no locks, no writes, no scheduling effects — and each call is one
//! `REPEATABLE READ READ ONLY` transaction, so envelope and typed rows come
//! from a single snapshot: a [`Meta`] can never describe an older completion
//! than the status it is returned with.
//!
//! There is deliberately no framework list-all: a nanoservice lists through
//! domain queries on its own typed tables and batch-loads the results with
//! `read_many`, so filtering SQL stays where the columns live and the
//! framework read is O(1) queries per batch.

use std::collections::HashMap;

use basable_core::labels::Labels;
use basable_db::begin_snapshot;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{PgConnection, Row as _};
use uuid::Uuid;

use crate::decl::Adapter;
use crate::error::Error;
use crate::model::{Meta, NamespacedName, Object, Phase, Ref};
use crate::store::TypedStore;
use crate::tx::Tx;

/// The envelope read column set, kept next to its scanner ([`scan_meta`]) —
/// the two must stay in lockstep. `next_reconcile_at` scans as a plain
/// timestamp: the sentinels are ordinary values.
pub(crate) const META_COLUMNS: &str = "id, external_id, name, namespace, labels, generation, \
     generation_changed_at, wake_seq, observed_generation, phase, attempts, last_error, \
     deleted_at, next_reconcile_at, last_reconciled_at, claimed_at, lease_expires_at, created_at";

/// Scans one `META_COLUMNS` row into `(id, Meta)`, mapping SQL NULLs to
/// their Rust defaults.
pub(crate) fn scan_meta(row: &PgRow) -> Result<(Uuid, Meta), sqlx::Error> {
    let id: Uuid = row.try_get("id")?;
    let phase: String = row.try_get("phase")?;
    let phase = Phase::parse(&phase).ok_or_else(|| {
        sqlx::Error::Decode(format!("processing object {id}: unknown phase {phase:?}").into())
    })?;
    let labels: Option<Json<Labels>> = row.try_get("labels")?;
    let last_error: Option<String> = row.try_get("last_error")?;
    let meta = Meta {
        external_id: row.try_get("external_id")?,
        name: NamespacedName {
            namespace: row.try_get("namespace")?,
            name: row.try_get("name")?,
        },
        labels: labels.map(|j| j.0).unwrap_or_default(),
        generation: row.try_get("generation")?,
        generation_changed_at: row.try_get::<DateTime<Utc>, _>("generation_changed_at")?,
        wake_seq: row.try_get("wake_seq")?,
        observed_generation: row.try_get("observed_generation")?,
        phase,
        attempts: row.try_get("attempts")?,
        last_error: last_error.unwrap_or_default(),
        deleted_at: row.try_get("deleted_at")?,
        next_reconcile_at: row.try_get("next_reconcile_at")?,
        last_reconciled_at: row.try_get("last_reconciled_at")?,
        claimed_at: row.try_get("claimed_at")?,
        lease_expires_at: row.try_get("lease_expires_at")?,
        created_at: row.try_get("created_at")?,
    };
    Ok((id, meta))
}

impl<S, T, A> TypedStore<S, T, A>
where
    S: Send + Sync + 'static,
    T: Send + Sync + 'static,
    A: Adapter<S, T>,
{
    /// One object — envelope snapshot plus typed spec and status from a
    /// single snapshot — or [`Error::NotFound`].
    pub async fn read(&self, r: &Ref) -> Result<Object<S, T>, Error> {
        self.check_ref(r)?;
        let mut objs = self.read_many(std::slice::from_ref(&r.id)).await?;
        if objs.is_empty() {
            return Err(Error::NotFound(r.clone()));
        }
        Ok(objs.swap_remove(0))
    }

    /// The objects for the given ids, in input order, from one consistent
    /// snapshot. Ids that no longer exist are simply absent — a batch
    /// assembled from a domain query may race a deletion, and that is the
    /// caller's normal case. An envelope whose typed row is missing is
    /// [`Error::Invariant`].
    pub async fn read_many(&self, ids: &[Uuid]) -> Result<Vec<Object<S, T>>, Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let op = format!("read {} batch", self.decl.name);
        let mut tx = begin_snapshot(&self.pool)
            .await
            .map_err(|e| Error::sql(format!("{op}: begin"), e))?;
        let rows = sqlx::query(&format!(
            "SELECT {META_COLUMNS} FROM {} WHERE id = ANY($1)",
            self.partition
        ))
        .bind(ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| Error::sql(format!("{op}: envelopes"), e))?;
        let mut metas: HashMap<Uuid, Meta> = HashMap::with_capacity(rows.len());
        let mut found: Vec<Uuid> = Vec::with_capacity(rows.len());
        for row in &rows {
            let (id, meta) =
                scan_meta(row).map_err(|e| Error::sql(format!("{op}: scan envelope"), e))?;
            metas.insert(id, meta);
            found.push(id);
        }
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let typed = {
            let conn: &mut PgConnection = &mut tx;
            let mut rtx = Tx::new(conn);
            self.decl
                .adapter
                .read_rows(&mut rtx, &found)
                .await
                .map_err(|e| Error::sql(format!("{op}: typed rows"), e))?
        };
        let mut by_id: HashMap<Uuid, (S, T)> = HashMap::with_capacity(typed.len());
        for row in typed {
            by_id.insert(row.id, (row.spec, row.status));
        }
        let mut objs = Vec::with_capacity(found.len());
        for id in ids {
            let Some(meta) = metas.remove(id) else {
                continue; // gone; absence is the caller's normal case
            };
            let Some((spec, status)) = by_id.remove(id) else {
                return Err(Error::Invariant(format!(
                    "envelope {} has no typed row",
                    self.r#ref(*id)
                )));
            };
            objs.push(Object {
                meta,
                id: *id,
                spec,
                status,
            });
        }
        Ok(objs)
    }
}
