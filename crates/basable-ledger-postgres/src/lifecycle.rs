//! The batch lifecycle: open, closed, claimed, consumed. Money-moving work
//! claims whole batches instead of checking a balance. A claim closes the
//! batch, sums its lines, records the amounts with the status change, and
//! consumes the batch once the money moved. A consumed batch never changes;
//! the engine books a later line for the same batch id into the next epoch.
//!
//! Nothing can land in a batch after its close: a commit takes a `FOR
//! SHARE` lock on every batch it writes into and refuses one that is no
//! longer open, so the close's `UPDATE` waits for every in-flight writer and
//! every later writer sees the batch closed. The claim can therefore sum
//! the moment the close has committed. (A snapshot-based horizon was tried
//! first and dropped: a snapshot lists only the in-progress transactions
//! below its `xmax`, and a writer whose transaction id is assigned after the
//! close is not in it at all.)

use std::fmt;

use basable_core::BoxError;
use basable_ledger::{BatchKey, BatchStatus, UnitKey};
use sqlx::{PgPool, Row};

/// A lifecycle step failed.
#[derive(Debug)]
pub enum LifecycleError {
    /// The database.
    Backend(BoxError),
    /// No batch has the key.
    Unknown(BatchKey),
    /// The step needs another status: `close` an open batch, `claim` a
    /// closed one, `consume` a claimed one.
    WrongStatus {
        /// The batch.
        batch: BatchKey,
        /// Where it is.
        status: BatchStatus,
        /// What the step needed.
        needed: BatchStatus,
    },
    /// A stored value the crate cannot read.
    Corrupt(String),
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LifecycleError::Backend(e) => write!(f, "batch lifecycle: {e}"),
            LifecycleError::Unknown(b) => write!(f, "batch {} does not exist", b.0),
            LifecycleError::WrongStatus {
                batch,
                status,
                needed,
            } => write!(
                f,
                "batch {} is {status:?}, the step needs {needed:?}",
                batch.0
            ),
            LifecycleError::Corrupt(m) => write!(f, "batch lifecycle: corrupt: {m}"),
        }
    }
}

impl std::error::Error for LifecycleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LifecycleError::Backend(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

fn backend(e: sqlx::Error) -> LifecycleError {
    LifecycleError::Backend(Box::new(e))
}

/// What a claim found in a batch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Claim {
    /// The batch.
    pub batch: BatchKey,
    /// The batch's balance per unit at the claim, from its lines.
    pub amounts: Vec<(UnitKey, i128)>,
}

/// The lifecycle over the owning nanoservice's pool.
#[derive(Clone, Debug)]
pub struct BatchLifecycle {
    pool: PgPool,
}

impl BatchLifecycle {
    /// Over the same pool as the store.
    pub fn new(pool: PgPool) -> Self {
        BatchLifecycle { pool }
    }

    /// The batch's status.
    pub async fn status(&self, batch: BatchKey) -> Result<BatchStatus, LifecycleError> {
        let status: Option<i16> =
            sqlx::query_scalar("SELECT status FROM batch WHERE batch_key = $1")
                .bind(batch.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        let status = status.ok_or(LifecycleError::Unknown(batch))?;
        BatchStatus::from_i16(status)
            .ok_or_else(|| LifecycleError::Corrupt(format!("batch.status {status}")))
    }

    /// Closes an open batch. The update waits for every commit that holds
    /// the batch's row `FOR SHARE`, so when it returns no writer is in
    /// flight and none can follow. Closing a closed batch is a no-op; a
    /// claimed or consumed one is a `WrongStatus`.
    pub async fn close(&self, batch: BatchKey) -> Result<(), LifecycleError> {
        let closed = sqlx::query(
            "UPDATE batch SET status = 2, closed_at = clock_timestamp() \
             WHERE batch_key = $1 AND status = 1",
        )
        .bind(batch.0)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if closed.rows_affected() == 1 {
            return Ok(());
        }
        match self.status(batch).await? {
            BatchStatus::Closed => Ok(()),
            status => Err(LifecycleError::WrongStatus {
                batch,
                status,
                needed: BatchStatus::Open,
            }),
        }
    }

    /// Claims a closed batch: sums its lines per unit, records the amounts
    /// and moves it to claimed, in one transaction. Claiming a claimed or
    /// consumed batch returns the recorded amounts.
    pub async fn claim(&self, batch: BatchKey) -> Result<Claim, LifecycleError> {
        match self.status(batch).await? {
            BatchStatus::Claimed | BatchStatus::Consumed => return self.recorded(batch).await,
            BatchStatus::Closed => {}
            status => {
                return Err(LifecycleError::WrongStatus {
                    batch,
                    status,
                    needed: BatchStatus::Closed,
                });
            }
        }
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let moved = sqlx::query("UPDATE batch SET status = 3 WHERE batch_key = $1 AND status = 2")
            .bind(batch.0)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if moved.rows_affected() != 1 {
            // Another claimant got there first; read what it recorded.
            tx.rollback().await.map_err(backend)?;
            return self.recorded(batch).await;
        }
        let rows = sqlx::query(
            "SELECT (l->>2)::bigint AS unit_key, SUM((l->>3)::numeric)::text AS total \
             FROM journal j, jsonb_array_elements(j.lines) AS l \
             WHERE j.lines @> $1 AND (l->>0)::bigint = $2 \
             GROUP BY 1 ORDER BY 1",
        )
        .bind(serde_json::json!([[batch.0]]))
        .bind(batch.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        let mut amounts = Vec::with_capacity(rows.len());
        for row in &rows {
            let unit: i64 = row.try_get("unit_key").map_err(backend)?;
            let total: String = row.try_get("total").map_err(backend)?;
            let unit = u32::try_from(unit)
                .map_err(|_| LifecycleError::Corrupt(format!("unit key {unit}")))?;
            let amount: i128 = total
                .parse()
                .map_err(|e| LifecycleError::Corrupt(format!("sum {total}: {e}")))?;
            sqlx::query(
                "INSERT INTO batch_claim (batch_key, unit_key, amount) VALUES ($1, $2, $3::numeric)",
            )
            .bind(batch.0)
            .bind(i64::from(unit))
            .bind(amount.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            amounts.push((UnitKey(unit), amount));
        }
        tx.commit().await.map_err(backend)?;
        Ok(Claim { batch, amounts })
    }

    /// Marks a claimed batch consumed: the money moved, the batch never
    /// changes again. Consuming a consumed batch is a no-op.
    pub async fn consume(&self, batch: BatchKey) -> Result<(), LifecycleError> {
        let moved = sqlx::query("UPDATE batch SET status = 4 WHERE batch_key = $1 AND status = 3")
            .bind(batch.0)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if moved.rows_affected() == 1 {
            return Ok(());
        }
        match self.status(batch).await? {
            BatchStatus::Consumed => Ok(()),
            status => Err(LifecycleError::WrongStatus {
                batch,
                status,
                needed: BatchStatus::Claimed,
            }),
        }
    }

    async fn recorded(&self, batch: BatchKey) -> Result<Claim, LifecycleError> {
        let rows = sqlx::query(
            "SELECT unit_key, amount::text AS amount FROM batch_claim \
             WHERE batch_key = $1 ORDER BY unit_key",
        )
        .bind(batch.0)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let mut amounts = Vec::with_capacity(rows.len());
        for row in &rows {
            let unit: i64 = row.try_get("unit_key").map_err(backend)?;
            let amount: String = row.try_get("amount").map_err(backend)?;
            amounts.push((
                UnitKey(
                    u32::try_from(unit)
                        .map_err(|_| LifecycleError::Corrupt(format!("unit key {unit}")))?,
                ),
                amount
                    .parse()
                    .map_err(|e| LifecycleError::Corrupt(format!("claim {amount}: {e}")))?,
            ));
        }
        Ok(Claim { batch, amounts })
    }
}
