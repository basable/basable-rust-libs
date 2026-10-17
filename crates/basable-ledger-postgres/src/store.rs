//! `PgStore`: the `Store` over a nanoservice's pool.

use std::collections::HashMap;

use basable_db::sqlstate;
use basable_ledger::{
    BatchId, BatchKey, BatchRow, BatchStatus, BatchType, Commit, CommitOutcome, Journal, LedgerKey,
    Line, LineBatch, ProcessId, ProcessRead, SnapshotDiff, Store, StoreError, Timestamp, UnitKey,
};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

/// The Postgres store. Holds the owning nanoservice's pool (`SET ROLE` and
/// `search_path` pinned by `NanoPool`), so every table name is unqualified.
#[derive(Clone, Debug)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// Over a pool whose `search_path` names the schema the ledger's
    /// migration was applied in.
    pub fn new(pool: PgPool) -> Self {
        PgStore { pool }
    }

    /// The pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn backend(e: sqlx::Error) -> StoreError {
    StoreError::Backend(Box::new(e))
}

fn corrupt(what: &str, e: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(format!("{what}: {e}"))
}

fn batch_row(row: &PgRow) -> Result<BatchRow, StoreError> {
    let batch_type: i16 = row.try_get("batch_type").map_err(backend)?;
    let status: i16 = row.try_get("status").map_err(backend)?;
    let ledger: i64 = row.try_get("ledger").map_err(backend)?;
    Ok(BatchRow {
        key: BatchKey(row.try_get("batch_key").map_err(backend)?),
        id: BatchId::new(row.try_get::<String, _>("batch_id").map_err(backend)?),
        epoch: row.try_get("epoch").map_err(backend)?,
        ledger: LedgerKey(u32::try_from(ledger).map_err(|_| corrupt("batch.ledger", ledger))?),
        batch_type: BatchType::from_i16(batch_type)
            .ok_or_else(|| corrupt("batch.batch_type", batch_type))?,
        status: BatchStatus::from_i16(status).ok_or_else(|| corrupt("batch.status", status))?,
    })
}

const SELECT_BATCH: &str =
    "SELECT batch_key, batch_id, epoch, ledger, batch_type, status FROM batch";

impl Store for PgStore {
    async fn read(&self, process: ProcessId) -> Result<ProcessRead, StoreError> {
        let rows = sqlx::query(
            "SELECT sequence, event_time, booked_at, lines, snapshot_diffs \
             FROM journal WHERE process_id = $1 ORDER BY sequence",
        )
        .bind(process.0)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let mut journals = Vec::with_capacity(rows.len());
        let mut keys: Vec<i64> = Vec::new();
        for row in &rows {
            let lines: Value = row.try_get("lines").map_err(backend)?;
            let diffs: Value = row.try_get("snapshot_diffs").map_err(backend)?;
            let lines: Vec<Line> =
                serde_json::from_value(lines).map_err(|e| corrupt("journal.lines", e))?;
            let snapshot_diffs: Vec<SnapshotDiff> =
                serde_json::from_value(diffs).map_err(|e| corrupt("journal.snapshot_diffs", e))?;
            for l in &lines {
                if !keys.contains(&l.batch_key.0) {
                    keys.push(l.batch_key.0);
                }
            }
            let sequence: i64 = row.try_get("sequence").map_err(backend)?;
            let event_time: Timestamp = row.try_get("event_time").map_err(backend)?;
            let booked_at: Timestamp = row.try_get("booked_at").map_err(backend)?;
            journals.push(Journal {
                process_id: process,
                sequence,
                event_time,
                booked_at,
                lines,
                snapshot_diffs,
            });
        }
        let mut batches = HashMap::new();
        if !keys.is_empty() {
            let rows = sqlx::query(&format!("{SELECT_BATCH} WHERE batch_key = ANY($1)"))
                .bind(&keys)
                .fetch_all(&self.pool)
                .await
                .map_err(backend)?;
            for row in &rows {
                let b = batch_row(row)?;
                batches.insert(b.key, b);
            }
            if batches.len() != keys.len() {
                return Err(StoreError::Corrupt(format!(
                    "process {}: lines name {} batches, {} rows exist",
                    process.0,
                    keys.len(),
                    batches.len()
                )));
            }
        }
        let rows = sqlx::query(
            "SELECT type_name, version FROM process_accounting_object_version \
             WHERE process_id = $1",
        )
        .bind(process.0)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let mut versions = HashMap::new();
        for row in &rows {
            let name: String = row.try_get("type_name").map_err(backend)?;
            let version: i32 = row.try_get("version").map_err(backend)?;
            versions.insert(
                name,
                u32::try_from(version).map_err(|_| corrupt("version", version))?,
            );
        }
        Ok(ProcessRead {
            journals,
            versions,
            batches,
        })
    }

    async fn resolve(&self, ids: &[BatchId]) -> Result<Vec<Option<BatchRow>>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let names: Vec<&str> = ids.iter().map(|i| i.as_str()).collect();
        let rows = sqlx::query(
            "SELECT DISTINCT ON (batch_id) batch_key, batch_id, epoch, ledger, batch_type, status \
             FROM batch WHERE batch_id = ANY($1) ORDER BY batch_id, epoch DESC",
        )
        .bind(&names)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let mut by_id: HashMap<String, BatchRow> = HashMap::new();
        for row in &rows {
            let b = batch_row(row)?;
            by_id.insert(b.id.as_str().to_owned(), b);
        }
        Ok(ids.iter().map(|id| by_id.remove(id.as_str())).collect())
    }

    async fn commit(&self, commit: Commit) -> Result<CommitOutcome, StoreError> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let mut new_keys: Vec<i64> = Vec::with_capacity(commit.new_batches.len());
        for nb in &commit.new_batches {
            let inserted: Option<i64> = sqlx::query_scalar(
                "INSERT INTO batch (batch_id, epoch, ledger, batch_type, status) \
                 VALUES ($1, $2, $3, $4, 1) \
                 ON CONFLICT (batch_id, epoch) DO NOTHING RETURNING batch_key",
            )
            .bind(nb.id.as_str())
            .bind(nb.epoch)
            .bind(i64::from(nb.ledger.0))
            .bind(nb.batch_type.as_i16())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
            let key = match inserted {
                Some(key) => key,
                None => sqlx::query_scalar(
                    "SELECT batch_key FROM batch WHERE batch_id = $1 AND epoch = $2",
                )
                .bind(nb.id.as_str())
                .bind(nb.epoch)
                .fetch_one(&mut *tx)
                .await
                .map_err(backend)?,
            };
            new_keys.push(key);
        }
        let lines: Vec<Line> = commit
            .lines
            .iter()
            .map(|l| Line {
                batch_key: match l.batch {
                    LineBatch::Key(k) => k,
                    LineBatch::New(i) => BatchKey(new_keys[i]),
                },
                object_id: l.object_id,
                unit: l.unit,
                amount: l.amount,
            })
            .collect();
        let lines_json = serde_json::to_value(&lines).map_err(|e| corrupt("lines", e))?;
        let diffs_json =
            serde_json::to_value(&commit.snapshot_diffs).map_err(|e| corrupt("diffs", e))?;
        let inserted = sqlx::query(
            "INSERT INTO journal (process_id, sequence, event_time, lines, snapshot_diffs) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(commit.process_id.0)
        .bind(commit.after_sequence + 1)
        .bind(commit.event_time)
        .bind(&lines_json)
        .bind(&diffs_json)
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => {
                tx.commit().await.map_err(backend)?;
                Ok(CommitOutcome::Committed)
            }
            Err(e) if sqlstate::is_unique_violation(&e) => {
                tx.rollback().await.map_err(backend)?;
                Ok(CommitOutcome::SequenceTaken)
            }
            Err(e) => Err(backend(e)),
        }
    }

    async fn record_version(
        &self,
        process: ProcessId,
        type_name: &str,
        version: u32,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO process_accounting_object_version (process_id, type_name, version) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (process_id, type_name) DO UPDATE \
             SET version = GREATEST(process_accounting_object_version.version, EXCLUDED.version)",
        )
        .bind(process.0)
        .bind(type_name)
        .bind(i32::try_from(version).map_err(|_| corrupt("version", version))?)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn batch_sum(&self, batch: BatchKey) -> Result<Vec<(UnitKey, i128)>, StoreError> {
        let probe = serde_json::json!([[batch.0]]);
        let rows = sqlx::query(
            "SELECT (l->>2)::bigint AS unit_key, SUM((l->>3)::numeric)::text AS total \
             FROM journal j, jsonb_array_elements(j.lines) AS l \
             WHERE j.lines @> $1 AND (l->>0)::bigint = $2 \
             GROUP BY 1 ORDER BY 1",
        )
        .bind(&probe)
        .bind(batch.0)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let mut sums = Vec::with_capacity(rows.len());
        for row in &rows {
            let unit: i64 = row.try_get("unit_key").map_err(backend)?;
            let total: String = row.try_get("total").map_err(backend)?;
            let unit = u32::try_from(unit).map_err(|_| corrupt("unit_key", unit))?;
            let amount: i128 = total.parse().map_err(|e| corrupt("sum", e))?;
            sums.push((UnitKey(unit), amount));
        }
        Ok(sums)
    }
}
