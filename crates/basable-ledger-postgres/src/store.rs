//! `PgStore`: the `Store` over a nanoservice's pool, and `PgBatch`, how it
//! reaches the user's batch tables.

use std::collections::BTreeMap;
use std::future::Future;
use std::marker::PhantomData;

use basable_db::sqlstate;
use basable_ledger::{
    BatchKey, BatchType, Commit, CommitOutcome, Journal, LedgerBatch, LedgerKey, ProcessId,
    SnapshotDiff, Store, StoreError, StoredBatch, Timestamp, UnitKey, lines_from_json,
    lines_to_json,
};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};

/// A batch's way to its row in the user's table: each variant knows its table
/// and its key values, so neither method takes more than a connection. The
/// scaffold renders this from the kinds of batch the manifest lists.
pub trait PgBatch: LedgerBatch {
    /// Reads the batch's row by its primary key. Must only read.
    fn find(
        &self,
        conn: &mut PgConnection,
    ) -> impl Future<Output = Result<Option<StoredBatch>, sqlx::Error>> + Send;

    /// Creates the batch's row if it has none and returns it:
    /// `INSERT ... ON CONFLICT (<primary key>) DO NOTHING` as a statement of
    /// its own, so it is committed when it returns, then a read. A concurrent
    /// first use waits for that one statement and reads its row. Never opens a
    /// transaction, which would let a stalled client hold every first use of
    /// the batch, and never `DO UPDATE`: a row that exists stays as it is.
    fn ensure(
        &self,
        conn: &mut PgConnection,
    ) -> impl Future<Output = Result<StoredBatch, sqlx::Error>> + Send;
}

/// A batch's row from its `batch_key`, `ledger` and `batch_type` columns, for
/// a [`PgBatch`] impl.
pub fn stored_batch(row: &PgRow) -> Result<StoredBatch, sqlx::Error> {
    let decode = |column: &str, e: String| sqlx::Error::ColumnDecode {
        index: column.to_owned(),
        source: e.into(),
    };
    let ledger: i32 = row.try_get("ledger")?;
    let ledger = u32::try_from(ledger).map_err(|e| decode("ledger", e.to_string()))?;
    let batch_type: i16 = row.try_get("batch_type")?;
    let batch_type = BatchType::from_i16(batch_type)
        .ok_or_else(|| decode("batch_type", format!("{batch_type} is not 1, 2 or 3")))?;
    Ok(StoredBatch {
        key: BatchKey(row.try_get("batch_key")?),
        ledger: LedgerKey(ledger),
        batch_type,
    })
}

/// The Postgres store. Holds the owning nanoservice's pool (`SET ROLE` and
/// `search_path` pinned by `NanoPool`), so every table name is unqualified.
#[derive(Debug)]
pub struct PgStore<B> {
    pool: PgPool,
    _batch: PhantomData<fn() -> B>,
}

impl<B> Clone for PgStore<B> {
    fn clone(&self) -> Self {
        PgStore {
            pool: self.pool.clone(),
            _batch: PhantomData,
        }
    }
}

impl<B> PgStore<B> {
    /// Over a pool whose `search_path` names the schema the ledger's
    /// migration and the batch tables were applied in.
    pub fn new(pool: PgPool) -> Self {
        PgStore {
            pool,
            _batch: PhantomData,
        }
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

impl<B: PgBatch> Store for PgStore<B> {
    type Batch = B;

    async fn read(&self, process: ProcessId) -> Result<Vec<Journal>, StoreError> {
        let rows = sqlx::query(
            "SELECT sequence, booked_at, lines, snapshot_diffs \
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
            let lines = lines_from_json(lines).map_err(|e| corrupt("journal.lines", e))?;
            let snapshot_diffs: Vec<SnapshotDiff> =
                serde_json::from_value(diffs).map_err(|e| corrupt("journal.snapshot_diffs", e))?;
            let sequence: i64 = row.try_get("sequence").map_err(backend)?;
            let booked_at: Timestamp = row.try_get("booked_at").map_err(backend)?;
            journals.push(Journal {
                process_id: process,
                sequence,
                booked_at,
                lines,
                snapshot_diffs,
            });
        }
        Ok(journals)
    }

    async fn find(&self, batch: &B) -> Result<Option<StoredBatch>, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(backend)?;
        batch.find(&mut conn).await.map_err(backend)
    }

    async fn ensure(&self, batch: &B) -> Result<StoredBatch, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(backend)?;
        batch.ensure(&mut conn).await.map_err(backend)
    }

    /// One statement, with no transaction to open and no lock of any kind: the
    /// journal row, and the upserts into the version index for every type
    /// present in the process, latest wins. A primary-key violation on the
    /// journal fails the statement as a whole.
    async fn commit(&self, commit: Commit) -> Result<CommitOutcome, StoreError> {
        let lines = lines_to_json(&commit.lines);
        let diffs =
            serde_json::to_value(&commit.snapshot_diffs).map_err(|e| corrupt("diffs", e))?;
        let versions =
            serde_json::to_value(&commit.versions).map_err(|e| corrupt("versions", e))?;
        let inserted = sqlx::query(
            "WITH j AS ( \
                 INSERT INTO journal (process_id, sequence, lines, snapshot_diffs) \
                 VALUES ($1, $2, $3, $4) RETURNING process_id \
             ) \
             INSERT INTO process_ledger_object_version (process_id, type_name, version) \
             SELECT j.process_id, v.key, v.value::integer FROM j, jsonb_each_text($5) AS v \
             ON CONFLICT (process_id, type_name) DO UPDATE SET version = EXCLUDED.version \
             WHERE process_ledger_object_version.version <> EXCLUDED.version",
        )
        .bind(commit.process_id.0)
        .bind(commit.after_sequence + 1)
        .bind(&lines)
        .bind(&diffs)
        .bind(&versions)
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(_) => Ok(CommitOutcome::Committed),
            Err(e) if sqlstate::is_unique_violation(&e) => Ok(CommitOutcome::SequenceTaken),
            Err(e) => Err(backend(e)),
        }
    }

    async fn record_versions(
        &self,
        process: ProcessId,
        versions: &BTreeMap<String, u32>,
    ) -> Result<(), StoreError> {
        let versions_json = serde_json::to_value(versions).map_err(|e| corrupt("versions", e))?;
        sqlx::query(
            "INSERT INTO process_ledger_object_version (process_id, type_name, version) \
             SELECT $1, v.key, v.value::integer FROM jsonb_each_text($2) AS v \
             ON CONFLICT (process_id, type_name) DO UPDATE SET version = EXCLUDED.version \
             WHERE process_ledger_object_version.version <> EXCLUDED.version",
        )
        .bind(process.0)
        .bind(&versions_json)
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
            "SELECT (l->>1)::bigint AS unit_key, SUM((l->>2)::numeric)::text AS total \
             FROM journal j, jsonb_each(j.lines) AS o, jsonb_array_elements(o.value) AS l \
             WHERE jsonb_path_query_array(j.lines, '$.*[*][0]') @> $1 \
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
            "SELECT process_id FROM process_ledger_object_version \
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
