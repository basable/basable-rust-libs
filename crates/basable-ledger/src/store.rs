//! The persistence interface. The engine is synchronous and pure (a
//! process's journals and the call's snapshots in, a commit out); only the
//! store is async. Five capabilities: read a process, resolve batch ids to
//! their open epoch, append one journal at the process's next sequence
//! together with the batch rows it needs, record the rules version per
//! process and type, and sum one batch's lines over every process.

use std::collections::HashMap;
use std::future::Future;

use crate::error::StoreError;
use crate::journal::{Journal, SnapshotDiff};
use crate::types::{
    BatchId, BatchKey, BatchStatus, BatchType, LedgerKey, ObjectId, ProcessId, Timestamp, UnitKey,
};

/// One physical batch: an epoch of a batch id.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BatchRow {
    /// The store's key, what a line carries.
    pub key: BatchKey,
    /// The user's id.
    pub id: BatchId,
    /// The epoch under that id, from 1.
    pub epoch: i32,
    /// The ledger it balances in.
    pub ledger: LedgerKey,
    /// Its column.
    pub batch_type: BatchType,
    /// Where it is in its life.
    pub status: BatchStatus,
}

/// Everything the engine reads about one process: its journals in sequence
/// order, the rules version recorded per object type, and the batch rows
/// its lines refer to.
#[derive(Clone, Debug, Default)]
pub struct ProcessRead {
    /// The journals, sequence 1 first. Empty for a process never booked.
    pub journals: Vec<Journal>,
    /// The `desired_state` version that last evaluated each object type
    /// in this process, by type name.
    pub versions: HashMap<String, u32>,
    /// The batch rows the journals' lines name, by key.
    pub batches: HashMap<BatchKey, BatchRow>,
}

impl ProcessRead {
    /// The process version: the highest sequence, 0 for none.
    pub fn sequence(&self) -> i64 {
        self.journals.last().map(|j| j.sequence).unwrap_or(0)
    }
}

/// A batch the commit creates: the next epoch of a batch id, with its ledger
/// and type. The store assigns the key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NewBatch {
    /// The user's id.
    pub id: BatchId,
    /// The epoch to create.
    pub epoch: i32,
    /// Its ledger.
    pub ledger: LedgerKey,
    /// Its column.
    pub batch_type: BatchType,
}

/// A line's batch in a commit: a key the store already has, or an index into
/// the commit's new batches, whose key the store learns when it creates them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineBatch {
    /// An existing batch.
    Key(BatchKey),
    /// `new_batches[i]`.
    New(usize),
}

/// One line as the engine hands it to the store.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CommitLine {
    /// The batch.
    pub batch: LineBatch,
    /// The object.
    pub object_id: ObjectId,
    /// The unit.
    pub unit: UnitKey,
    /// Signed amount.
    pub amount: i128,
}

/// What a call commits for one process: one journal at the next sequence and
/// the batches it needs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Commit {
    /// The process.
    pub process_id: ProcessId,
    /// The highest sequence the engine read; the journal goes at plus one.
    pub after_sequence: i64,
    /// Batches to create, `ON CONFLICT DO NOTHING` on (id, epoch).
    pub new_batches: Vec<NewBatch>,
    /// The journal's event time.
    pub event_time: Timestamp,
    /// The lines.
    pub lines: Vec<CommitLine>,
    /// The snapshot diffs.
    pub snapshot_diffs: Vec<SnapshotDiff>,
}

/// What a commit came back with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommitOutcome {
    /// The journal is in.
    Committed,
    /// Another commit to the process took the sequence first: read again,
    /// recompute.
    SequenceTaken,
    /// A batch the lines write into is no longer open: it was closed between
    /// the resolve and the commit. Read again, recompute; the next epoch is
    /// resolved then. The store checks this under a `FOR SHARE` lock on the
    /// batch row, so a close waits for every in-flight commit and nothing
    /// lands in a batch after its close.
    BatchNotOpen(BatchKey),
}

/// The backend behind the engine. Every adapter passes the conformance suite
/// in `basable-ledger-testkit`.
pub trait Store: Send + Sync {
    /// Every journal of the process in sequence order, the recorded versions
    /// and the batch rows the lines name. A process never booked reads as
    /// empty.
    fn read(
        &self,
        process: ProcessId,
    ) -> impl Future<Output = Result<ProcessRead, StoreError>> + Send;

    /// For each batch id, its highest epoch's row, whatever its status, or
    /// `None` when no batch has the id. Same order as `ids`.
    fn resolve(
        &self,
        ids: &[BatchId],
    ) -> impl Future<Output = Result<Vec<Option<BatchRow>>, StoreError>> + Send;

    /// Atomically creates the new batches (a lost race on (id, epoch) reuses
    /// the winner's row), checks under a shared row lock that every existing
    /// batch written into is still open ([`CommitOutcome::BatchNotOpen`]
    /// otherwise), and appends the journal at `after_sequence + 1`. The
    /// primary key on (process, sequence) is the compare-and-set: a
    /// violation is [`CommitOutcome::SequenceTaken`].
    fn commit(
        &self,
        commit: Commit,
    ) -> impl Future<Output = Result<CommitOutcome, StoreError>> + Send;

    /// Records the `desired_state` version that last evaluated an object
    /// type in a process. An upsert, called after the commit; a crash between
    /// the two is repaired by the next pass, which finds nothing to book.
    fn record_version(
        &self,
        process: ProcessId,
        type_name: &str,
        version: u32,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The sum of one batch's lines per unit over every process, for a claim.
    fn batch_sum(
        &self,
        batch: BatchKey,
    ) -> impl Future<Output = Result<Vec<(UnitKey, i128)>, StoreError>> + Send;

    /// Up to `limit` processes whose recorded version of `type_name` is below
    /// `version`: the restatement sweep's worklist, in any stable order. A
    /// process that booked the type but never had its version recorded (a
    /// crash between commit and `record_version`) is not listed; its next
    /// booking restates it.
    fn processes_behind(
        &self,
        type_name: &str,
        version: u32,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<ProcessId>, StoreError>> + Send;
}
