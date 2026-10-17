//! The in-memory store: the reference `Store` for tests and the scenario
//! DSL. One mutex around everything; nothing is held across an await.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::error::StoreError;
use crate::journal::{Journal, Line};
use crate::store::{BatchRow, Commit, CommitOutcome, LineBatch, ProcessRead, Store};
use crate::types::{BatchId, BatchKey, BatchStatus, ProcessId, UnitKey};

#[derive(Default)]
struct Inner {
    journals: BTreeMap<ProcessId, Vec<Journal>>,
    batches: Vec<BatchRow>,
    versions: HashMap<(ProcessId, String), u32>,
}

/// The in-memory store. Cheap to clone: clones share the state.
#[derive(Clone, Default)]
pub struct InMemoryStore {
    inner: Arc<Mutex<Inner>>,
}

impl InMemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every journal of a process, for assertions.
    pub fn journals(&self, process: ProcessId) -> Vec<Journal> {
        let g = self.inner.lock().expect("store lock");
        g.journals.get(&process).cloned().unwrap_or_default()
    }

    /// Every batch row, for assertions.
    pub fn batches(&self) -> Vec<BatchRow> {
        self.inner.lock().expect("store lock").batches.clone()
    }

    /// Sets a batch's status, as a claimant would.
    pub fn set_status(&self, key: BatchKey, status: BatchStatus) {
        let mut g = self.inner.lock().expect("store lock");
        if let Some(row) = g.batches.iter_mut().find(|b| b.key == key) {
            row.status = status;
        }
    }

    /// The recorded rules version of a type in a process.
    pub fn version(&self, process: ProcessId, type_name: &str) -> Option<u32> {
        let g = self.inner.lock().expect("store lock");
        g.versions.get(&(process, type_name.to_owned())).copied()
    }
}

impl Store for InMemoryStore {
    async fn read(&self, process: ProcessId) -> Result<ProcessRead, StoreError> {
        let g = self.inner.lock().expect("store lock");
        let journals = g.journals.get(&process).cloned().unwrap_or_default();
        let mut batches: HashMap<BatchKey, BatchRow> = HashMap::new();
        for line in journals.iter().flat_map(|j| j.lines.iter()) {
            if let std::collections::hash_map::Entry::Vacant(slot) = batches.entry(line.batch_key) {
                let row = g
                    .batches
                    .iter()
                    .find(|b| b.key == line.batch_key)
                    .cloned()
                    .ok_or_else(|| {
                        StoreError::Corrupt(format!("batch key {} has no row", line.batch_key.0))
                    })?;
                slot.insert(row);
            }
        }
        let versions = g
            .versions
            .iter()
            .filter(|((p, _), _)| *p == process)
            .map(|((_, t), v)| (t.clone(), *v))
            .collect();
        Ok(ProcessRead {
            journals,
            versions,
            batches,
        })
    }

    async fn resolve(&self, ids: &[BatchId]) -> Result<Vec<Option<BatchRow>>, StoreError> {
        let g = self.inner.lock().expect("store lock");
        Ok(ids
            .iter()
            .map(|id| {
                g.batches
                    .iter()
                    .filter(|b| b.id == *id)
                    .max_by_key(|b| b.epoch)
                    .cloned()
            })
            .collect())
    }

    async fn commit(&self, commit: Commit) -> Result<CommitOutcome, StoreError> {
        let mut g = self.inner.lock().expect("store lock");
        let inner = &mut *g;
        let journals = inner.journals.entry(commit.process_id).or_default();
        let last = journals.last().map(|j| j.sequence).unwrap_or(0);
        if last != commit.after_sequence {
            return Ok(CommitOutcome::SequenceTaken);
        }
        let mut new_keys = Vec::with_capacity(commit.new_batches.len());
        for nb in &commit.new_batches {
            let existing = inner
                .batches
                .iter()
                .find(|b| b.id == nb.id && b.epoch == nb.epoch)
                .map(|b| b.key);
            let key = match existing {
                Some(key) => key,
                None => {
                    let key = BatchKey(inner.batches.len() as i64 + 1);
                    inner.batches.push(BatchRow {
                        key,
                        id: nb.id.clone(),
                        epoch: nb.epoch,
                        ledger: nb.ledger,
                        batch_type: nb.batch_type,
                        status: BatchStatus::Open,
                    });
                    key
                }
            };
            new_keys.push(key);
        }
        let lines = commit
            .lines
            .iter()
            .map(|l| Line {
                batch_key: match l.batch {
                    LineBatch::Key(k) => k,
                    LineBatch::New(i) => new_keys[i],
                },
                object_id: l.object_id,
                unit: l.unit,
                amount: l.amount,
            })
            .collect();
        journals.push(Journal {
            process_id: commit.process_id,
            sequence: last + 1,
            event_time: commit.event_time,
            booked_at: chrono::Utc::now(),
            lines,
            snapshot_diffs: commit.snapshot_diffs,
        });
        Ok(CommitOutcome::Committed)
    }

    async fn record_version(
        &self,
        process: ProcessId,
        type_name: &str,
        version: u32,
    ) -> Result<(), StoreError> {
        let mut g = self.inner.lock().expect("store lock");
        let slot = g
            .versions
            .entry((process, type_name.to_owned()))
            .or_insert(version);
        if *slot < version {
            *slot = version;
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
            .filter(|l| l.batch_key == batch)
        {
            *sums.entry(line.unit).or_insert(0) += line.amount;
        }
        Ok(sums.into_iter().collect())
    }
}
