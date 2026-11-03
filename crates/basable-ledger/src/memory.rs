//! The in-memory store: the reference `Store` for tests and the scenario
//! DSL. One mutex around everything; nothing is held across an await. It
//! keeps one key per primary key from one counter, as the user's batch tables
//! and their one sequence do.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::accounting::LedgerBatch;
use crate::error::StoreError;
use crate::journal::Journal;
use crate::store::{Commit, CommitOutcome, Store};
use crate::types::{BatchKey, ProcessId, StoredBatch, UnitKey};

struct Inner<B: LedgerBatch> {
    journals: BTreeMap<ProcessId, Vec<Journal>>,
    /// The version index the commit maintains, as Postgres keeps it.
    versions: HashMap<(ProcessId, String), u32>,
    /// The batch rows, by primary key.
    batches: BTreeMap<B::PrimaryKey, StoredBatch>,
    /// The last key allocated.
    last_key: i64,
}

impl<B: LedgerBatch> Default for Inner<B> {
    fn default() -> Self {
        Inner {
            journals: BTreeMap::new(),
            versions: HashMap::new(),
            batches: BTreeMap::new(),
            last_key: 0,
        }
    }
}

/// The in-memory store. Cheap to clone: clones share the state.
pub struct InMemoryStore<B: LedgerBatch> {
    inner: Arc<Mutex<Inner<B>>>,
}

impl<B: LedgerBatch> Clone for InMemoryStore<B> {
    fn clone(&self) -> Self {
        InMemoryStore {
            inner: self.inner.clone(),
        }
    }
}

impl<B: LedgerBatch> Default for InMemoryStore<B> {
    fn default() -> Self {
        InMemoryStore {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }
}

impl<B: LedgerBatch> InMemoryStore<B> {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every journal of a process, for assertions.
    pub fn journals(&self, process: ProcessId) -> Vec<Journal> {
        let g = self.inner.lock().expect("store lock");
        g.journals.get(&process).cloned().unwrap_or_default()
    }

    /// The indexed rules version a type in a process was last evaluated under.
    pub fn version(&self, process: ProcessId, type_name: &str) -> Option<u32> {
        let g = self.inner.lock().expect("store lock");
        g.versions.get(&(process, type_name.to_owned())).copied()
    }

    /// How many batch rows exist, for assertions.
    pub fn batch_count(&self) -> usize {
        let g = self.inner.lock().expect("store lock");
        g.batches.len()
    }
}

impl<B: LedgerBatch> Store for InMemoryStore<B> {
    type Batch = B;

    async fn read(&self, process: ProcessId) -> Result<Vec<Journal>, StoreError> {
        let g = self.inner.lock().expect("store lock");
        Ok(g.journals.get(&process).cloned().unwrap_or_default())
    }

    async fn find(&self, batch: &B) -> Result<Option<StoredBatch>, StoreError> {
        let g = self.inner.lock().expect("store lock");
        Ok(g.batches.get(&batch.primary_key()).copied())
    }

    async fn commit(&self, commit: Commit<B>) -> Result<CommitOutcome, StoreError> {
        let mut g = self.inner.lock().expect("store lock");
        let inner = &mut *g;
        let last = inner
            .journals
            .get(&commit.process_id)
            .and_then(|js| js.last())
            .map(|j| j.sequence)
            .unwrap_or(0);
        if last != commit.after_sequence {
            return Ok(CommitOutcome::SequenceTaken);
        }
        // Checked before anything changes: nothing of a refused commit lands.
        for new in &commit.new_batches {
            if let Some(row) = inner.batches.get(&new.batch.primary_key())
                && let Err(rejection) = row.check(&new.batch)
            {
                return Ok(CommitOutcome::Rejected(rejection));
            }
        }
        let mut keys = Vec::with_capacity(commit.new_batches.len());
        for new in &commit.new_batches {
            let row = inner
                .batches
                .entry(new.batch.primary_key())
                .or_insert_with(|| {
                    inner.last_key += 1;
                    StoredBatch {
                        key: BatchKey(inner.last_key),
                        ledger: new.batch.ledger(),
                        batch_type: new.batch.batch_type(),
                    }
                });
            keys.push(row.key);
        }
        let lines = commit.resolved_lines(&keys);
        for (type_name, version) in &commit.versions {
            inner
                .versions
                .insert((commit.process_id, type_name.clone()), *version);
        }
        inner
            .journals
            .entry(commit.process_id)
            .or_default()
            .push(Journal {
                process_id: commit.process_id,
                sequence: last + 1,
                booked_at: chrono::Utc::now(),
                lines,
                snapshot_diffs: commit.snapshot_diffs,
            });
        Ok(CommitOutcome::Committed)
    }

    async fn record_versions(
        &self,
        process: ProcessId,
        versions: &BTreeMap<String, u32>,
    ) -> Result<(), StoreError> {
        let mut g = self.inner.lock().expect("store lock");
        for (type_name, version) in versions {
            g.versions.insert((process, type_name.clone()), *version);
        }
        Ok(())
    }

    async fn batch_sum(&self, batch: BatchKey) -> Result<Vec<(UnitKey, i128)>, StoreError> {
        let g = self.inner.lock().expect("store lock");
        let mut sums: BTreeMap<UnitKey, i128> = BTreeMap::new();
        for line in g
            .journals
            .values()
            .flat_map(|js| js.iter())
            .flat_map(|j| j.lines.iter())
            .filter(|l| l.batch == batch)
        {
            *sums.entry(line.unit).or_insert(0) += line.amount;
        }
        Ok(sums.into_iter().collect())
    }

    async fn processes_behind(
        &self,
        type_name: &str,
        version: u32,
        limit: u32,
    ) -> Result<Vec<ProcessId>, StoreError> {
        let g = self.inner.lock().expect("store lock");
        let mut behind: Vec<ProcessId> = g
            .versions
            .iter()
            .filter(|((_, t), v)| t == type_name && **v < version)
            .map(|((p, _), _)| *p)
            .collect();
        behind.sort();
        behind.truncate(limit as usize);
        Ok(behind)
    }
}
