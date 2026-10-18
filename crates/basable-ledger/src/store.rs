//! The persistence interface. The engine is synchronous and pure (a
//! process's journals and the call's snapshots in, a commit out); only the
//! store is async. Four capabilities: read a process's journals, append one
//! journal at the process's next sequence, sum one batch's lines over every
//! process, and list the processes behind a rules version. The store knows
//! nothing about batches beyond the keys on the lines: there is no batch
//! table. The rules version per process and type rides in the journal row;
//! the store keeps an index of it, maintained by the commit, that only the
//! startup restatement reads.

use std::collections::BTreeMap;
use std::future::Future;

use crate::error::StoreError;
use crate::journal::{Journal, Line, SnapshotDiff};
use crate::types::{BatchKey, ProcessId, UnitKey};

/// What a call commits for one process: one journal at the next sequence.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Commit {
    /// The process.
    pub process_id: ProcessId,
    /// The highest sequence the engine read; the journal goes at plus one.
    pub after_sequence: i64,
    /// The lines.
    pub lines: Vec<Line>,
    /// The snapshot diffs.
    pub snapshot_diffs: Vec<SnapshotDiff>,
    /// The types whose rules version this journal moves, with the version:
    /// written in the journal row and upserted into the version index in
    /// the same statement. Usually empty.
    pub rules_versions: BTreeMap<String, u32>,
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
    /// Every journal of the process in sequence order. A process never
    /// booked reads as empty. This is the one read a booking makes.
    fn read(
        &self,
        process: ProcessId,
    ) -> impl Future<Output = Result<Vec<Journal>, StoreError>> + Send;

    /// Appends the journal at `after_sequence + 1` and, in the same
    /// statement, upserts its `rules_versions` into the index of the
    /// recorded version per process and type (never moving one backwards).
    /// The primary key on (process, sequence) is the compare-and-set: a
    /// violation is [`CommitOutcome::SequenceTaken`], and nothing else of the
    /// statement lands.
    fn commit(
        &self,
        commit: Commit,
    ) -> impl Future<Output = Result<CommitOutcome, StoreError>> + Send;

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
    /// `version`, from the index the commit maintains: the startup
    /// restatement's worklist, in any stable order.
    fn processes_behind(
        &self,
        type_name: &str,
        version: u32,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<ProcessId>, StoreError>> + Send;
}
