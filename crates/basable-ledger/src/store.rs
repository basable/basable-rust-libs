//! The persistence interface. The engine is synchronous and pure (a
//! process's journals and the call's snapshots in, a commit out); only the
//! store is async. Two tables per process: the journals, which a booking
//! reads and appends to, and the version index, one row per process and
//! object type holding the rules version the process was last evaluated
//! under, which the commit writes with the journal and nothing but the
//! startup restatement reads. A store also reaches the user's batch tables,
//! through the batch's value: `find` reads a row, and the commit creates the
//! rows of new batches together with the first journal that names them.

use std::collections::BTreeMap;
use std::future::Future;

use crate::accounting::LedgerBatch;
use crate::engine;
use crate::error::{Rejection, StoreError};
use crate::journal::{Journal, Line, SnapshotDiff};
use crate::types::{BatchKey, ProcessId, StoredBatch, UnitKey};

/// A batch the commit must create a row for: no row existed when the call
/// was planned, so its lines carry `placeholder`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NewBatch<B> {
    /// What its lines carry until the row gives them a key.
    pub placeholder: BatchKey,
    /// The value, with the columns a new row holds.
    pub batch: B,
}

/// What a call commits for one process: one journal at the next sequence.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Commit<B> {
    /// The process.
    pub process_id: ProcessId,
    /// The highest sequence the engine read; the journal goes at plus one.
    pub after_sequence: i64,
    /// The lines, carrying a placeholder for each of `new_batches`:
    /// [`resolved_lines`](Self::resolved_lines) is what a store writes.
    pub lines: Vec<Line>,
    /// The snapshot diffs.
    pub snapshot_diffs: Vec<SnapshotDiff>,
    /// The running rules version of every object type present in the
    /// process after this commit: upserted into the version index together
    /// with the journal, latest wins.
    pub versions: BTreeMap<String, u32>,
    /// The batches with no row yet, in primary key order, so that two first
    /// uses cannot deadlock. Their rows land with the journal or not at all.
    pub new_batches: Vec<NewBatch<B>>,
}

impl<B> Commit<B> {
    /// The lines with each placeholder replaced by the key its row got:
    /// `keys` is parallel to [`new_batches`](Self::new_batches). With no new
    /// batch these are the lines as planned.
    pub fn resolved_lines(&self, keys: &[BatchKey]) -> Vec<Line> {
        assert_eq!(keys.len(), self.new_batches.len(), "one key per new batch");
        if keys.is_empty() {
            return self.lines.clone();
        }
        let resolved: BTreeMap<BatchKey, BatchKey> = self
            .new_batches
            .iter()
            .map(|n| n.placeholder)
            .zip(keys.iter().copied())
            .collect();
        let lines: Vec<Line> = self
            .lines
            .iter()
            .map(|l| Line {
                batch: resolved.get(&l.batch).copied().unwrap_or(l.batch),
                ..*l
            })
            .collect();
        engine::net(&lines)
    }
}

/// What a commit came back with.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommitOutcome {
    /// The journal is in, with the rows of its new batches.
    Committed,
    /// Another commit to the process took the sequence first: read again,
    /// recompute. Nothing of this commit landed.
    SequenceTaken,
    /// A concurrent first use created a batch's row under another ledger or
    /// type than this commit's value says. Nothing of this commit landed.
    Rejected(Rejection),
}

/// The backend behind the engine. Every adapter passes the conformance suite
/// in `basable-ledger-testkit`.
pub trait Store: Send + Sync {
    /// The batches of this ledger: the one `Batch` enum of the owner.
    type Batch: LedgerBatch;

    /// Every journal of the process in sequence order. A process never
    /// booked reads as empty. A booking makes this read, and one
    /// [`find`](Self::find) per distinct batch its process names.
    fn read(
        &self,
        process: ProcessId,
    ) -> impl Future<Output = Result<Vec<Journal>, StoreError>> + Send;

    /// A batch's row, by the value's primary key, or `None` when no booking
    /// has created one, in which case nothing is booked on it anywhere. Only
    /// reads.
    fn find(
        &self,
        batch: &Self::Batch,
    ) -> impl Future<Output = Result<Option<StoredBatch>, StoreError>> + Send;

    /// In one transaction: a row for each of [`Commit::new_batches`] that
    /// still has none, in the order given, then the journal at
    /// `after_sequence + 1` carrying [`Commit::resolved_lines`], then the
    /// upserts of `versions` into the index of the version each process and
    /// type was last evaluated under (the latest commit wins). The primary
    /// key on (process, sequence) is the compare-and-set: a violation is
    /// [`CommitOutcome::SequenceTaken`], and nothing of the commit lands, the
    /// rows included. A commit with no new batch needs no more than one
    /// statement.
    fn commit(
        &self,
        commit: Commit<Self::Batch>,
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
