//! The optional projector: a background loop that maintains
//! `batch_balance (batch_key, unit_key, amount)` from the journals, with a
//! per-process sequence check. The engine never reads it; a claim sums its
//! batch from the journals. An implementer starts it or not; the only
//! writer of `batch_balance` is this loop, so bookings never contend on it.
//!
//! Every run reads the journals booked since the previous run's start
//! minus an overlap, in booked order, and applies each process's journals
//! only as the stored sequence plus one. A sequence at or below the stored
//! one is the overlap showing a journal again, and is skipped. A sequence
//! two or more above means a journal was missed: within a process the
//! sequence is gapless by construction and booked in order, so the only
//! ways to miss one are a booking transaction that outlived the overlap or
//! a clock step. The run then stops with [`ProjectorError::Gap`] and
//! applies nothing; recovery is [`Projector::run_from`] with a wider window,
//! since applying is idempotent. The requirement this rests on: a booking
//! transaction finishes inside the overlap, which `statement_timeout`
//! enforces.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use basable_core::BoxError;
use basable_ledger::{BatchKey, BatchType, LedgerKey, Line, ProcessId, Timestamp, UnitKey};
use serde_json::Value;
use sqlx::{PgPool, Row};

/// A run failed.
#[derive(Debug)]
pub enum ProjectorError {
    /// The database.
    Backend(BoxError),
    /// A stored value the projector cannot read.
    Corrupt(String),
    /// A process's journal arrived two or more sequences ahead of the last
    /// one applied: a journal was missed. Nothing of the run was applied.
    Gap {
        /// The process.
        process: ProcessId,
        /// The sequence the projector expected next.
        expected: i64,
        /// The sequence it found.
        found: i64,
    },
}

impl fmt::Display for ProjectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProjectorError::Backend(e) => write!(f, "projector: {e}"),
            ProjectorError::Corrupt(m) => write!(f, "projector: corrupt: {m}"),
            ProjectorError::Gap {
                process,
                expected,
                found,
            } => write!(
                f,
                "projector: process {} skipped from sequence {} to {found}: a journal was missed, \
                 widen the window and run again",
                process.0,
                expected - 1
            ),
        }
    }
}

impl std::error::Error for ProjectorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProjectorError::Backend(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

fn backend(e: sqlx::Error) -> ProjectorError {
    ProjectorError::Backend(Box::new(e))
}

/// What one run did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Applied {
    /// Journals applied.
    pub applied: usize,
    /// Journals seen again through the overlap and skipped.
    pub skipped: usize,
}

/// One row of a trial balance: a batch's balance in one unit, with the
/// batch's ledger and type so the two columns can be drawn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BalanceRow {
    /// The batch.
    pub batch: BatchKey,
    /// The user's batch id.
    pub batch_id: String,
    /// The epoch.
    pub epoch: i32,
    /// The ledger.
    pub ledger: LedgerKey,
    /// Debit, credit or memo: the column.
    pub batch_type: BatchType,
    /// The unit.
    pub unit: UnitKey,
    /// The balance.
    pub amount: i128,
}

/// The projector over the owning nanoservice's pool.
#[derive(Clone, Debug)]
pub struct Projector {
    pool: PgPool,
    name: String,
    overlap: Duration,
}

impl Projector {
    /// A projector named `default` with the given overlap: how far before
    /// the previous run's start each run reads. Longer than any booking
    /// transaction may run.
    pub fn new(pool: PgPool, overlap: Duration) -> Self {
        Projector {
            pool,
            name: "default".to_owned(),
            overlap,
        }
    }

    /// A second projector with its own watermark.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// One run from the previous run's start minus the overlap (from the
    /// beginning of time on the first run).
    pub async fn run_once(&self) -> Result<Applied, ProjectorError> {
        let since: Option<Timestamp> =
            sqlx::query_scalar("SELECT through FROM projector_watermark WHERE name = $1")
                .bind(&self.name)
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        let since = since
            .map(|t| t - chrono::Duration::from_std(self.overlap).unwrap_or_default())
            .unwrap_or(Timestamp::UNIX_EPOCH);
        self.run_from(since).await
    }

    /// One run over every journal booked at or after `since`. The recovery
    /// after a [`ProjectorError::Gap`]: widen the window and run again.
    pub async fn run_from(&self, since: Timestamp) -> Result<Applied, ProjectorError> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let start: Timestamp = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(backend)?;
        let rows = sqlx::query(
            "SELECT process_id, sequence, lines FROM journal \
             WHERE booked_at >= $1 ORDER BY booked_at, process_id, sequence",
        )
        .bind(since)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        let mut stored: HashMap<ProcessId, i64> = HashMap::new();
        let mut applied = Applied::default();
        for row in &rows {
            let process = ProcessId(row.try_get("process_id").map_err(backend)?);
            let sequence: i64 = row.try_get("sequence").map_err(backend)?;
            let last = match stored.get(&process) {
                Some(s) => *s,
                None => {
                    let s: Option<i64> = sqlx::query_scalar(
                        "SELECT sequence FROM projected_process WHERE process_id = $1",
                    )
                    .bind(process.0)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(backend)?;
                    let s = s.unwrap_or(0);
                    stored.insert(process, s);
                    s
                }
            };
            if sequence <= last {
                applied.skipped += 1;
                continue;
            }
            if sequence != last + 1 {
                return Err(ProjectorError::Gap {
                    process,
                    expected: last + 1,
                    found: sequence,
                });
            }
            let lines: Value = row.try_get("lines").map_err(backend)?;
            let lines: Vec<Line> = serde_json::from_value(lines)
                .map_err(|e| ProjectorError::Corrupt(format!("journal.lines: {e}")))?;
            for line in &lines {
                sqlx::query(
                    "INSERT INTO batch_balance (batch_key, unit_key, amount) \
                     VALUES ($1, $2, $3::numeric) \
                     ON CONFLICT (batch_key, unit_key) DO UPDATE \
                     SET amount = batch_balance.amount + EXCLUDED.amount",
                )
                .bind(line.batch_key.0)
                .bind(i64::from(line.unit.0))
                .bind(line.amount.to_string())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
            sqlx::query(
                "INSERT INTO projected_process (process_id, sequence) VALUES ($1, $2) \
                 ON CONFLICT (process_id) DO UPDATE SET sequence = EXCLUDED.sequence",
            )
            .bind(process.0)
            .bind(sequence)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            stored.insert(process, sequence);
            applied.applied += 1;
        }
        sqlx::query(
            "INSERT INTO projector_watermark (name, through) VALUES ($1, $2) \
             ON CONFLICT (name) DO UPDATE SET through = EXCLUDED.through",
        )
        .bind(&self.name)
        .bind(start)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(applied)
    }

    /// Runs every `interval` until `cancel` completes or a run fails. A
    /// [`ProjectorError::Gap`] stops the loop on purpose: it is an operator's
    /// decision to widen the window.
    pub async fn run(
        &self,
        interval: Duration,
        cancel: impl Future<Output = ()>,
    ) -> Result<(), ProjectorError> {
        let mut cancel = std::pin::pin!(cancel);
        let mut ticker = tokio::time::interval(interval);
        loop {
            tokio::select! {
                _ = &mut cancel => return Ok(()),
                _ = ticker.tick() => {
                    let applied = self.run_once().await?;
                    tracing::debug!(
                        projector = %self.name,
                        applied = applied.applied,
                        skipped = applied.skipped,
                        "projector run"
                    );
                }
            }
        }
    }

    /// The projected balance of one batch in one unit; 0 when none.
    pub async fn balance(&self, batch: BatchKey, unit: UnitKey) -> Result<i128, ProjectorError> {
        let amount: Option<String> = sqlx::query_scalar(
            "SELECT amount::text FROM batch_balance WHERE batch_key = $1 AND unit_key = $2",
        )
        .bind(batch.0)
        .bind(i64::from(unit.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        match amount {
            None => Ok(0),
            Some(a) => a
                .parse()
                .map_err(|e| ProjectorError::Corrupt(format!("batch_balance {a}: {e}"))),
        }
    }

    /// Every projected balance of one ledger, with each batch's type: the
    /// trial balance, two columns by type, left equal to right per unit.
    pub async fn trial_balance(
        &self,
        ledger: LedgerKey,
    ) -> Result<Vec<BalanceRow>, ProjectorError> {
        let rows = sqlx::query(
            "SELECT bb.batch_key, b.batch_id, b.epoch, b.ledger, b.batch_type, bb.unit_key, \
                    bb.amount::text AS amount \
             FROM batch_balance bb JOIN batch b ON b.batch_key = bb.batch_key \
             WHERE b.ledger = $1 ORDER BY b.batch_id, b.epoch, bb.unit_key",
        )
        .bind(i64::from(ledger.0))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        rows.iter().map(balance_row).collect()
    }

    /// Debit and credit batches whose balance is below zero in their column:
    /// normal while an open item clears across objects, a bug when it ages.
    pub async fn negative_balances(&self) -> Result<Vec<BalanceRow>, ProjectorError> {
        let rows = sqlx::query(
            "SELECT bb.batch_key, b.batch_id, b.epoch, b.ledger, b.batch_type, bb.unit_key, \
                    bb.amount::text AS amount \
             FROM batch_balance bb JOIN batch b ON b.batch_key = bb.batch_key \
             WHERE bb.amount < 0 AND b.batch_type IN (1, 2) \
             ORDER BY b.batch_id, b.epoch, bb.unit_key",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        rows.iter().map(balance_row).collect()
    }
}

fn balance_row(row: &sqlx::postgres::PgRow) -> Result<BalanceRow, ProjectorError> {
    let ledger: i64 = row.try_get("ledger").map_err(backend)?;
    let batch_type: i16 = row.try_get("batch_type").map_err(backend)?;
    let unit: i64 = row.try_get("unit_key").map_err(backend)?;
    let amount: String = row.try_get("amount").map_err(backend)?;
    Ok(BalanceRow {
        batch: BatchKey(row.try_get("batch_key").map_err(backend)?),
        batch_id: row.try_get("batch_id").map_err(backend)?,
        epoch: row.try_get("epoch").map_err(backend)?,
        ledger: LedgerKey(
            u32::try_from(ledger)
                .map_err(|_| ProjectorError::Corrupt(format!("ledger {ledger}")))?,
        ),
        batch_type: BatchType::from_i16(batch_type)
            .ok_or_else(|| ProjectorError::Corrupt(format!("batch_type {batch_type}")))?,
        unit: UnitKey(
            u32::try_from(unit).map_err(|_| ProjectorError::Corrupt(format!("unit {unit}")))?,
        ),
        amount: amount
            .parse()
            .map_err(|e| ProjectorError::Corrupt(format!("amount {amount}: {e}")))?,
    })
}
