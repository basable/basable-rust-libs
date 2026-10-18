//! The persistence interface. The engine is synchronous and pure (a
//! process's journals and the call's snapshots in, a commit out); only the
//! store is async. Two tables per process: the journals, which a booking
//! reads and appends to, and the version index, one row per process and
//! object type holding the rules version the process was last evaluated
//! under, which the commit writes in the same statement and nothing but
//! the startup restatement reads. The store knows nothing about batches
//! beyond the keys on the lines: there is no batch table.

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
    /// The running rules version of every object type present in the
    /// process after this commit: upserted into the version index in the
    /// same statement as the journal, latest wins.
    pub versions: BTreeMap<String, u32>,
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
    /// statement, upserts `versions` into the index of the version each
    /// process and type was last evaluated under (the latest commit wins).
    /// The primary key on (process, sequence) is the compare-and-set: a
    /// violation is [`CommitOutcome::SequenceTaken`], and nothing else of the
    /// statement lands.
    fn commit(
        &self,
        commit: Commit,
    ) -> impl Future<Output = Result<CommitOutcome, StoreError>> + Send;

    /// Upserts `versions` into the index without a journal: the restatement's
    /// case of a process whose positions already match the running rules,
    /// so the worklist stops listing it. Never called by a booking.
    fn record_versions(
        &self,
        process: ProcessId,
        versions: &BTreeMap<String, u32>,
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

    /// Up to `limit` processes whose indexed version of `type_name` is below
    /// `version`: the startup restatement's worklist, in any stable order.
    fn processes_behind(
        &self,
        type_name: &str,
        version: u32,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<ProcessId>, StoreError>> + Send;
}
