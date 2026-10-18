//! The in-memory store: the reference `Store` for tests and the scenario
//! DSL. One mutex around everything; nothing is held across an await.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::error::StoreError;
use crate::journal::Journal;
use crate::store::{Commit, CommitOutcome, ProcessRead, Store};
use crate::types::{BatchKey, ProcessId, UnitKey};

#[derive(Default)]
struct Inner {
    journals: BTreeMap<ProcessId, Vec<Journal>>,
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
        let versions = g
            .versions
            .iter()
            .filter(|((p, _), _)| *p == process)
            .map(|((_, t), v)| (t.clone(), *v))
            .collect();
        Ok(ProcessRead { journals, versions })
    }

    async fn commit(&self, commit: Commit) -> Result<CommitOutcome, StoreError> {
        let mut g = self.inner.lock().expect("store lock");
        let journals = g.journals.entry(commit.process_id).or_default();
        let last = journals.last().map(|j| j.sequence).unwrap_or(0);
        if last != commit.after_sequence {
            return Ok(CommitOutcome::SequenceTaken);
        }
        journals.push(Journal {
            process_id: commit.process_id,
            sequence: last + 1,
            booked_at: chrono::Utc::now(),
            lines: commit.lines,
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
            .filter(|l| l.batch.key == batch)
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
