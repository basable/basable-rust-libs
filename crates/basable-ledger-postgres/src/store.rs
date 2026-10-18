//! `PgStore`: the `Store` over a nanoservice's pool.

use std::collections::HashMap;

use basable_db::sqlstate;
use basable_ledger::{
    BatchKey, Commit, CommitOutcome, Journal, Line, ProcessId, ProcessRead, SnapshotDiff, Store,
    StoreError, Timestamp, UnitKey,
};
use serde_json::Value;
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
        for row in &rows {
            let lines: Value = row.try_get("lines").map_err(backend)?;
            let diffs: Value = row.try_get("snapshot_diffs").map_err(backend)?;
            let lines: Vec<Line> =
                serde_json::from_value(lines).map_err(|e| corrupt("journal.lines", e))?;
            let snapshot_diffs: Vec<SnapshotDiff> =
                serde_json::from_value(diffs).map_err(|e| corrupt("journal.snapshot_diffs", e))?;
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
        Ok(ProcessRead { journals, versions })
    }

    async fn commit(&self, commit: Commit) -> Result<CommitOutcome, StoreError> {
        let lines_json = serde_json::to_value(&commit.lines).map_err(|e| corrupt("lines", e))?;
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
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(_) => Ok(CommitOutcome::Committed),
            Err(e) if sqlstate::is_unique_violation(&e) => Ok(CommitOutcome::SequenceTaken),
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

    /// Over `idx_journal_batch_keys`: the probe is the index's expression,
    /// the batch keys of a journal's lines, so only journals with a line in
    /// the batch are read, and of those only the lines in it are summed.
    async fn batch_sum(&self, batch: BatchKey) -> Result<Vec<(UnitKey, i128)>, StoreError> {
        let probe = serde_json::json!([batch.0]);
        let rows = sqlx::query(
            "SELECT (l->>4)::bigint AS unit_key, SUM((l->>5)::numeric)::text AS total \
             FROM journal j, jsonb_array_elements(j.lines) AS l \
             WHERE jsonb_path_query_array(j.lines, '$[*][0]') @> $1 \
               AND (l->>0)::bigint = $2 \
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

    async fn processes_behind(
        &self,
        type_name: &str,
        version: u32,
        limit: u32,
    ) -> Result<Vec<ProcessId>, StoreError> {
        let rows: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT process_id FROM process_accounting_object_version \
             WHERE type_name = $1 AND version < $2 ORDER BY process_id LIMIT $3",
        )
        .bind(type_name)
        .bind(i32::try_from(version).map_err(|_| corrupt("version", version))?)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(rows.into_iter().map(ProcessId).collect())
    }
}
