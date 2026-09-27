//! The claim path (invariant 4): `claim_batch` takes an exclusive, leased
//! claim on a batch of due objects, and [`Claim`] is the attempt's authority
//! handle that heartbeats extend and completion fences against.
//!
//! Authority rests on two clocks, and both are needed:
//!
//! - The DATABASE lease (`lease_expires_at`) exists for successors: once it
//!   expires, another replica adopts the row by replacing the token through
//!   ordinary claiming. It cannot protect against the holder itself — a
//!   paused process resumes and writes without consulting the database.
//! - The LOCAL ownership proof ([`Deadline`]) exists for the holder: it is
//!   measured from a reading taken BEFORE the claim was sent, so it always
//!   expires no later than the database lease, and it is held against both
//!   the monotonic and the wall clock. A holder that cannot prove its lease
//!   is still running self-fences without any database round trip.
//!
//! Only the token is authority. Lease expiry never fences the holder by
//! itself (the successor's token replacement does), and the local proof
//! never grants anything (it only revokes).
//!
//! The scan takes disjoint batches across replicas with `FOR UPDATE SKIP
//! LOCKED`, ordered by how overdue work is with `last_reconciled_at` as the
//! fairness key. Adopting an expired claim bumps that key in the same
//! statement: a poison object that repeatedly kills its holder rotates to
//! the back of the queue.
//!
//! There is deliberately no release: a claim ends by completion or by lease
//! expiry. Crash, shutdown mid-attempt and lost completion all converge on
//! the same path, so there is one abandonment mechanism, not two.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use basable_core::Deadline;
use basable_core::labels::Labels;
use sqlx::{PgConnection, PgPool, Row as _};
use uuid::Uuid;

use crate::decl::{Adapter, WorkerConfig};
use crate::error::Error;
use crate::model::{Meta, Object, Ref, Row, SCHEDULE_PARKED_SQL};
use crate::store::TypedStore;
use crate::store_read::{META_COLUMNS, scan_meta};
use crate::tx::Tx;

/// How far the database lease outlives the attempt timeout. The margin
/// absorbs completion latency: an attempt that used its full budget still
/// gets its completion transaction in before successors may adopt.
pub const LEASE_SLACK: Duration = Duration::from_secs(30);

/// Renders a validated, non-empty label selector as the claim scan's
/// containment predicate — a SQL LITERAL, so a per-partition partial index
/// on `labels @> '{…}'` would be usable from this text. Injection safety is
/// by construction: the label charset excludes `'` and `\`, and the ordered
/// map renders sorted keys, so equal selectors render byte-identically.
pub(crate) fn label_selector_sql(selector: &Labels) -> Result<String, Error> {
    let rendered = serde_json::to_string(selector)
        .map_err(|e| Error::invalid(format!("render label selector: {e}")))?;
    Ok(format!("  AND labels @> '{rendered}'::jsonb\n"))
}

/// The attempt's lease: the token, the local proof, and the sticky closed
/// flag. Shared between the claim and the [`LeaseHandle`] a worker
/// heartbeats with while the reconciler holds the claim.
pub(crate) struct Lease {
    pub(crate) token: Uuid,
    pub(crate) duration: Duration,
    proof: Mutex<Option<Deadline>>,
    closed: AtomicBool,
}

impl Lease {
    fn new(token: Uuid, duration: Duration, proof: Deadline) -> Lease {
        Lease {
            token,
            duration,
            proof: Mutex::new(Some(proof)),
            closed: AtomicBool::new(false),
        }
    }

    /// The local self-fence (invariant 4): every authoritative write calls
    /// it before touching the database. Once the proof lapses or the claim
    /// is closed, the claim is dead for good — [`Error::Fenced`],
    /// permanently. Closing is sticky: a concurrent heartbeat landing
    /// afterwards cannot resurrect a closed claim.
    pub(crate) fn require_proof(&self) -> Result<(), Error> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::Fenced);
        }
        let live = self.proof().is_some_and(|d| d.is_live());
        if !live {
            self.close();
            return Err(Error::Fenced);
        }
        Ok(())
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub(crate) fn proof(&self) -> Option<Deadline> {
        *self.proof.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Extends the proof to `next`. The sticky closed flag keeps a closed
    /// claim dead regardless of this store.
    pub(crate) fn extend(&self, next: Deadline) {
        *self.proof.lock().unwrap_or_else(|p| p.into_inner()) = Some(next);
    }
}

/// One attempt's authority over one object: the claim-time snapshot plus
/// the token minted for this attempt. Created only by
/// [`TypedStore::claim_batch`] and consumed by [`Claim::complete`].
pub struct Claim<S, T, A: Adapter<S, T>> {
    /// The claim-time snapshot: the envelope (including the generation and
    /// wake sequence this attempt is fenced to) plus typed spec and status,
    /// all read under the claim transaction's locks. `write_status`
    /// advances `object.status`.
    pub object: Object<S, T>,
    pub(crate) store: TypedStore<S, T, A>,
    pub(crate) lease: Arc<Lease>,
    pub(crate) cfg: WorkerConfig,
    pub(crate) adopted: bool,
}

/// The heartbeat half of a claim, cloneable so a worker can extend the
/// lease on a ticker while the reconciler holds the claim itself.
#[derive(Clone)]
pub struct LeaseHandle {
    lease: Arc<Lease>,
    pool: PgPool,
    partition: String,
    id: Uuid,
    r: Ref,
}

impl LeaseHandle {
    /// Extends the claim's database lease and local ownership proof. A
    /// worker calls it on a fixed interval while the attempt runs; a third
    /// of the lease is comfortable.
    ///
    /// `Ok`: the lease was extended and the proof now ends one lease after
    /// the instant just before the request was sent. [`Error::Fenced`]:
    /// permanent — the token no longer matches (a successor adopted the
    /// object) or the proof already lapsed; the claim is closed for good.
    /// Anything else is transient: the claim stays open and the worker tries
    /// again next tick; the proof keeps counting down through the outage,
    /// so persistent failure converges on self-fencing by itself.
    pub async fn heartbeat(&self) -> Result<(), Error> {
        self.lease.require_proof()?;
        let pre_send = Deadline::after(self.lease.duration);
        let done = sqlx::query(&format!(
            "UPDATE {} SET lease_expires_at = clock_timestamp() + make_interval(secs => $1)
             WHERE id = $2 AND claim_token = $3",
            self.partition
        ))
        .bind(self.lease.duration.as_secs_f64())
        .bind(self.id)
        .bind(self.lease.token)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::sql(format!("heartbeat {}", self.r), e))?;
        if done.rows_affected() == 0 {
            self.lease.close();
            return Err(Error::Fenced);
        }
        self.lease.extend(pre_send);
        Ok(())
    }

    /// The local ownership proof's horizon, `None` once fenced.
    pub fn ownership_deadline(&self) -> Option<Deadline> {
        if self.lease.closed.load(Ordering::SeqCst) {
            return None;
        }
        self.lease.proof()
    }
}

impl<S, T, A> Claim<S, T, A>
where
    S: Send + Sync + 'static,
    T: Send + Sync + 'static,
    A: Adapter<S, T>,
{
    /// The claimed object's reference.
    pub fn r#ref(&self) -> Ref {
        self.store.r#ref(self.object.id)
    }

    /// Whether this claim replaced a predecessor's expired lease (a
    /// takeover) rather than claiming an unclaimed object. Successors use
    /// it to avoid charging a predecessor's stall against wall-clock budgets
    /// they anchor in typed status.
    pub fn adopted(&self) -> bool {
        self.adopted
    }

    /// The local ownership proof's horizon: the instant after which this
    /// process can no longer prove its database lease is still running.
    /// Heartbeats extend it; `None` once fenced.
    pub fn ownership_deadline(&self) -> Option<Deadline> {
        self.lease_handle().ownership_deadline()
    }

    /// The heartbeat handle, for a worker to extend the lease concurrently
    /// with the attempt.
    pub fn lease_handle(&self) -> LeaseHandle {
        LeaseHandle {
            lease: Arc::clone(&self.lease),
            pool: self.store.inner.pool.clone(),
            partition: self.store.inner.partition.clone(),
            id: self.object.id,
            r: self.r#ref(),
        }
    }

    /// [`LeaseHandle::heartbeat`].
    pub async fn heartbeat(&self) -> Result<(), Error> {
        self.lease_handle().heartbeat().await
    }

    /// The worker policy this claim was taken under.
    pub fn config(&self) -> &WorkerConfig {
        &self.cfg
    }
}

impl<S, T, A> TypedStore<S, T, A>
where
    S: Send + Sync + 'static,
    T: Send + Sync + 'static,
    A: Adapter<S, T>,
{
    /// Claims up to `cfg.batch_size` due objects and returns their claim
    /// handles with claim-time snapshots. Objects are due when `due_at` has
    /// passed and they are unclaimed or their previous holder's lease has
    /// expired (adoption). Parked objects are not due by construction;
    /// deleting objects claim like any other — teardown is ordinary work.
    ///
    /// The whole batch is one transaction: one statement mints tokens,
    /// stamps leases and returns the post-claim envelopes; the typed rows
    /// are then read under the same row locks, so the snapshot cannot be
    /// torn. Competing replicas skip locked rows and take disjoint batches.
    /// A batch whose COMMIT acknowledgement is lost needs no repair: if it
    /// landed, the claims expire unheartbeaten and are adopted.
    pub async fn claim_batch(&self, cfg: WorkerConfig) -> Result<Vec<Claim<S, T, A>>, Error> {
        let cfg = cfg.validated()?;
        let lease = cfg.attempt_timeout + LEASE_SLACK;
        let name = self.inner.decl.name;
        let op = format!("claim {name} batch");

        // The local proof is measured from BEFORE any database work, so it
        // can only underestimate the database lease, never outlive it.
        let proof = Deadline::after(lease);

        let selector = if cfg.label_selector.is_empty() {
            String::new()
        } else {
            label_selector_sql(&cfg.label_selector)?
        };

        let mut tx = self
            .inner
            .pool
            .begin()
            .await
            .map_err(|e| Error::sql(format!("{op}: begin"), e))?;

        // One statement claims the batch and returns the post-update
        // envelopes — which ARE the claim-time snapshots. The redundant
        // literal `due_at < <parked>` comparison exists for the partial scan
        // index: the planner cannot prove the index predicate from
        // `due_at <= now()`, only from this literal.
        let partition = &self.inner.partition;
        let meta_o = META_COLUMNS
            .split(", ")
            .map(|c| format!("o.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = sqlx::query(&format!(
            "UPDATE {partition} o
             SET claim_token = gen_random_uuid(),
                 claimed_at = clock_timestamp(),
                 lease_expires_at = clock_timestamp() + make_interval(secs => $2),
                 last_reconciled_at = CASE
                     WHEN o.claim_token IS NOT NULL THEN clock_timestamp()
                     ELSE o.last_reconciled_at
                 END
             FROM (
                 SELECT id, claim_token IS NOT NULL AS adopted
                 FROM {partition}
                 WHERE due_at < {SCHEDULE_PARKED_SQL}
                   AND due_at <= now()
                   AND (claim_token IS NULL OR lease_expires_at <= now())
                 {selector}
                 ORDER BY due_at ASC, last_reconciled_at ASC NULLS FIRST, id ASC
                 LIMIT $1
                 FOR UPDATE SKIP LOCKED
             ) due
             WHERE o.id = due.id
             RETURNING {meta_o}, o.claim_token, due.adopted"
        ))
        .bind(i64::from(cfg.batch_size))
        .bind(lease.as_secs_f64())
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| Error::sql(format!("{op}: scan"), e))?;

        let mut tokens: HashMap<Uuid, Uuid> = HashMap::with_capacity(rows.len());
        let mut metas: HashMap<Uuid, Meta> = HashMap::with_capacity(rows.len());
        let mut ids: Vec<Uuid> = Vec::with_capacity(rows.len());
        let mut adopted_ids: Vec<Uuid> = Vec::new();
        for row in &rows {
            let (id, meta) =
                scan_meta(row).map_err(|e| Error::sql(format!("{op}: scan claimed row"), e))?;
            let token: Uuid = row
                .try_get("claim_token")
                .map_err(|e| Error::sql(format!("{op}: scan claimed row"), e))?;
            let adopted: bool = row
                .try_get("adopted")
                .map_err(|e| Error::sql(format!("{op}: scan claimed row"), e))?;
            tokens.insert(id, token);
            metas.insert(id, meta);
            ids.push(id);
            if adopted {
                adopted_ids.push(id);
            }
        }
        if ids.is_empty() {
            if !cfg.label_selector.is_empty() {
                self.warn_unroutable_due_rows(&mut tx).await;
            }
            return Ok(Vec::new());
        }

        let typed = {
            let conn: &mut PgConnection = &mut tx;
            let mut rtx = Tx::new(conn);
            self.inner
                .decl
                .adapter
                .read_rows(&mut rtx, &ids)
                .await
                .map_err(|e| Error::sql(format!("{op}: typed rows"), e))?
        };
        let mut by_id: HashMap<Uuid, Row<S, T>> = HashMap::with_capacity(typed.len());
        for row in typed {
            by_id.insert(row.id, row);
        }

        let lease_shared_cfg = cfg.clone();
        let mut claims = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(row) = by_id.remove(&id) else {
                // Corruption (invariant 1), but failing the whole batch
                // would let one broken row halt the entire type on every
                // claim scan. Claim it and drop it: loud here, then the
                // lease expires and the adoption bump rotates it behind
                // healthy work.
                tracing::error!(
                    processing_object_type = name,
                    %id,
                    invariant = 1,
                    "processing object envelope has no typed row — dropping from claim batch"
                );
                continue;
            };
            let meta = metas.remove(&id).expect("every claimed id has a meta");
            claims.push(Claim {
                object: Object {
                    meta,
                    id,
                    spec: row.spec,
                    status: row.status,
                },
                store: self.clone(),
                lease: Arc::new(Lease::new(tokens[&id], lease, proof)),
                cfg: lease_shared_cfg.clone(),
                adopted: adopted_ids.contains(&id),
            });
        }
        tx.commit()
            .await
            .map_err(|e| Error::commit(op.clone(), e))?;
        // Logged only after the commit made the adoption real. An adoption
        // means a predecessor crashed, stalled past its lease, or lost its
        // completion — the single most diagnostic event when reconstructing
        // repeated work.
        for id in &adopted_ids {
            tracing::warn!(
                processing_object_type = name,
                %id,
                "adopted expired processing object claim — the previous holder never completed"
            );
        }
        Ok(claims)
    }

    /// The liveness canary for label-filtered claiming — a diagnostic, never
    /// enforcement. A row created with NO labels matches no equality
    /// selector and is never claimed by ANY filtered worker; this probe runs
    /// only when a filtered scan came back empty.
    async fn warn_unroutable_due_rows(&self, conn: &mut PgConnection) {
        let unroutable: Result<(bool,), _> = sqlx::query_as(&format!(
            "SELECT EXISTS (
                 SELECT 1 FROM {}
                 WHERE due_at < {SCHEDULE_PARKED_SQL}
                   AND due_at <= now()
                   AND (claim_token IS NULL OR lease_expires_at <= now())
                   AND labels IS NULL
             )",
            self.inner.partition
        ))
        .fetch_one(&mut *conn)
        .await;
        // A failed canary probe must never fail the claim path.
        if let Ok((true,)) = unroutable {
            tracing::warn!(
                processing_object_type = self.inner.decl.name,
                "label-filtered claim scan found no work while unlabelled due rows exist — rows without labels are never claimed by filtered workers"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Pinned to its exact text, like the parked literal: a per-partition
    /// partial index on `labels @> '{…}'` would be usable only because the
    /// query carries this literal byte for byte.
    #[test]
    fn the_label_selector_renders_as_a_sorted_literal() {
        assert_eq!(
            label_selector_sql(&labels(&[("infra_type", "cloud")])).unwrap(),
            "  AND labels @> '{\"infra_type\":\"cloud\"}'::jsonb\n"
        );
        let a = label_selector_sql(&labels(&[("zone", "fsn1"), ("infra_type", "cloud")])).unwrap();
        let b = label_selector_sql(&labels(&[("infra_type", "cloud"), ("zone", "fsn1")])).unwrap();
        assert_eq!(a, b);
        assert!(a.contains("'{\"infra_type\":\"cloud\",\"zone\":\"fsn1\"}'::jsonb"));
    }

    #[test]
    fn a_lease_fences_once_and_stays_fenced() {
        let lease = Lease::new(
            Uuid::new_v4(),
            Duration::from_secs(1),
            Deadline::after(Duration::from_secs(60)),
        );
        assert!(lease.require_proof().is_ok());
        lease.close();
        assert!(matches!(lease.require_proof(), Err(Error::Fenced)));
        // A later extension cannot resurrect it.
        lease.extend(Deadline::after(Duration::from_secs(60)));
        assert!(matches!(lease.require_proof(), Err(Error::Fenced)));

        let lapsed = Lease::new(
            Uuid::new_v4(),
            Duration::from_secs(1),
            Deadline::after(Duration::ZERO),
        );
        std::thread::sleep(Duration::from_millis(2));
        assert!(matches!(lapsed.require_proof(), Err(Error::Fenced)));
    }
}
