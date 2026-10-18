//! The persistence interface. The engine is synchronous and pure (a
//! process's journals and the call's snapshots in, a commit out); only the
//! store is async. Four capabilities: read a process, append one journal at
//! the process's next sequence, record the rules version per process and
//! type, and sum one batch's lines over every process. The store knows
//! nothing about batches beyond the keys on the lines: there is no batch
//! table.

use std::collections::HashMap;
use std::future::Future;

use crate::error::StoreError;
use crate::journal::{Journal, Line, SnapshotDiff};
use crate::types::{BatchKey, ProcessId, Timestamp, UnitKey};

/// Everything the engine reads about one process: its journals in sequence
/// order and the rules version recorded per object type.
#[derive(Clone, Debug, Default)]
pub struct ProcessRead {
    /// The journals, sequence 1 first. Empty for a process never booked.
    pub journals: Vec<Journal>,
    /// The `desired_state` version that last evaluated each object type
    /// in this process, by type name.
    pub versions: HashMap<String, u32>,
}

impl ProcessRead {
    /// The process version: the highest sequence, 0 for none.
    pub fn sequence(&self) -> i64 {
        self.journals.last().map(|j| j.sequence).unwrap_or(0)
    }
}

/// What a call commits for one process: one journal at the next sequence.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Commit {
    /// The process.
    pub process_id: ProcessId,
    /// The highest sequence the engine read; the journal goes at plus one.
    pub after_sequence: i64,
    /// The journal's event time.
    pub event_time: Timestamp,
    /// The lines.
    pub lines: Vec<Line>,
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
}

/// The backend behind the engine. Every adapter passes the conformance suite
/// in `basable-ledger-testkit`.
pub trait Store: Send + Sync {
    /// Every journal of the process in sequence order and the recorded
    /// versions. A process never booked reads as empty.
    fn read(
        &self,
        process: ProcessId,
    ) -> impl Future<Output = Result<ProcessRead, StoreError>> + Send;

    /// Appends the journal at `after_sequence + 1`. The primary key on
    /// (process, sequence) is the compare-and-set: a violation is
    /// [`CommitOutcome::SequenceTaken`].
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

    /// The sum of one batch's lines per unit over every process: what a
    /// return, a payout or a report takes from a batch. A sum of zero is a
    /// closed batch. Lines keep landing while the sum is read; whatever the
    /// read missed is in the next one, which is why a claim books what it
    /// took as a journal of its own and the remainder is the next claim's.
    fn batch_sum(
        &self,
        batch: BatchKey,
    ) -> impl Future<Output = Result<Vec<(UnitKey, i128)>, StoreError>> + Send;

    /// Up to `limit` processes whose recorded version of `type_name` is below
    /// `version`: the startup restatement's worklist, in any stable order. A
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
