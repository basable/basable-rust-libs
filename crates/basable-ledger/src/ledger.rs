//! The runtime: a ledger built with every object type it books, over one
//! store. A call carries any mix of registered types across processes; the
//! objects of one process book atomically in one journal, in the order
//! submitted, or not at all, and the outcomes come back per object.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::accounting::Accounting;
use crate::balances::Balances;
use crate::engine::{self, PreOutcome, Submission};
use crate::erased::Registry;
use crate::error::{LedgerError, Rejection};
use crate::object::LedgerObject;
use crate::store::{CommitOutcome, Store};
use crate::types::{BatchId, ProcessId, Timestamp, UnitKey};

/// How many times a process's commit is retried when another writer takes
/// its sequence first. A sibling's commit is pure mechanics, so the retry is
/// the library's; the caller never sees a race.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 8;

/// What happened to one submitted object.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Booked in this call.
    Booked,
    /// A snapshot at the version already booked: the journal is already in.
    AlreadyBooked,
    /// A snapshot older than the version booked.
    Stale,
    /// Rejected, with the reason. One rejected object rejects every object
    /// of its process in the call, and the reason names the culprit.
    Rejected(Rejection),
}

/// The objects of one call.
#[derive(Default)]
pub struct Request {
    items: Vec<(&'static str, Box<dyn Any + Send>)>,
}

impl Request {
    /// An empty call.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an object of any registered type.
    pub fn add<O: LedgerObject>(&mut self, object: O) -> &mut Self {
        self.items.push((O::TYPE, Box::new(object)));
        self
    }

    /// `add`, by value.
    pub fn with<O: LedgerObject>(mut self, object: O) -> Self {
        self.add(object);
        self
    }

    /// How many objects the call carries.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the call carries nothing.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// What a restatement of one process did.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Restated {
    /// Objects re-evaluated under the running rules.
    pub objects: usize,
    /// Lines the restatement journal booked; 0 means no journal was written.
    pub lines: usize,
    /// Types whose recorded version moved.
    pub versions_recorded: usize,
}

/// A restatement's effect before it books: the net change per batch and unit.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RestatementPreview {
    /// Objects that would be re-evaluated.
    pub objects: usize,
    /// Net lines, per (batch id, unit).
    pub deltas: Vec<(BatchId, UnitKey, i128)>,
}

/// Builds a [`Ledger`]: the store and every object type it books.
pub struct Builder<S> {
    store: S,
    registry: Registry,
    max_attempts: u32,
}

impl<S: Store> Builder<S> {
    /// Registers an object type with its accounting. Registering one type
    /// name twice is a programming error and panics here, at startup.
    pub fn register<O: LedgerObject, A: Accounting<O>>(mut self) -> Self {
        if let Err(e) = self.registry.register::<O, A>() {
            panic!("{e}");
        }
        self
    }

    /// How often a process's commit is retried on a lost sequence race.
    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts.max(1);
        self
    }

    /// The ledger.
    pub fn build(self) -> Ledger<S> {
        Ledger {
            store: self.store,
            registry: Arc::new(self.registry),
            max_attempts: self.max_attempts,
        }
    }
}

/// The ledger over one store.
pub struct Ledger<S> {
    store: S,
    registry: Arc<Registry>,
    max_attempts: u32,
}

enum Committed {
    Journal(usize),
    SequenceTaken,
    Rejected(Rejection),
}

impl<S: Store> Ledger<S> {
    /// Starts building a ledger over `store`.
    pub fn builder(store: S) -> Builder<S> {
        Builder {
            store,
            registry: Registry::default(),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }

    /// The store underneath.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Books a call: one outcome per object, in the request's order. The
    /// processes in the call are handled one after another; within one
    /// process the objects book atomically or not at all.
    pub async fn book(&self, request: Request) -> Result<Vec<Outcome>, LedgerError> {
        let mut outcomes: Vec<Option<Outcome>> = vec![None; request.items.len()];
        let mut by_process: BTreeMap<ProcessId, Vec<Submission>> = BTreeMap::new();
        let mut order: Vec<ProcessId> = Vec::new();
        for (index, (type_name, any)) in request.items.into_iter().enumerate() {
            let handler = self
                .registry
                .get(type_name)
                .ok_or_else(|| LedgerError::UnknownType(type_name.to_owned()))?;
            let object = handler
                .wrap(any)
                .ok_or_else(|| LedgerError::UnknownType(type_name.to_owned()))?;
            let process = object.process_id();
            let entry = by_process.entry(process).or_insert_with(|| {
                order.push(process);
                Vec::new()
            });
            if entry
                .iter()
                .any(|s| s.object.object_id() == object.object_id())
            {
                return Err(LedgerError::DuplicateObject(process, object.object_id()));
            }
            entry.push(Submission { index, object });
        }
        for process in order {
            let submissions = by_process.remove(&process).unwrap_or_default();
            let decided = self.book_process(process, &submissions).await?;
            for (index, outcome) in decided {
                outcomes[index] = Some(outcome);
            }
        }
        Ok(outcomes
            .into_iter()
            .map(|o| o.expect("every submission decided"))
            .collect())
    }

    /// The booked positions of every object of a process, as the engine
    /// reads them: the sum of each object's lines per batch id and unit.
    pub async fn balances(&self, process: ProcessId) -> Result<Balances, LedgerError> {
        let read = self.store.read(process).await?;
        let current = engine::fold(process, &read, &self.registry)?;
        Ok(current.balances)
    }

    /// Restates one process under the running rules of every registered
    /// type: objects of a type whose recorded version is behind are
    /// re-evaluated and the difference is appended as one journal. Nothing
    /// to do is a `Restated` with zero lines.
    pub async fn restate(&self, process: ProcessId) -> Result<Restated, LedgerError> {
        let types: Vec<(&'static str, u32)> = self.registry.types().collect();
        for _attempt in 0..self.max_attempts {
            let read = self.store.read(process).await?;
            let current = engine::fold(process, &read, &self.registry)?;
            let plan = engine::plan(&current, &[], &types)
                .map_err(|rejection| LedgerError::Rejected { process, rejection })?;
            let mut restated = Restated {
                objects: plan.restated_objects,
                lines: 0,
                versions_recorded: 0,
            };
            if !plan.is_empty() {
                match self.commit_plan(process, &current, &plan).await? {
                    Committed::Journal(lines) => restated.lines = lines,
                    Committed::SequenceTaken => continue,
                    Committed::Rejected(rejection) => {
                        return Err(LedgerError::Rejected { process, rejection });
                    }
                }
            }
            for (type_name, version) in &plan.versions_to_record {
                self.store
                    .record_version(process, type_name, *version)
                    .await?;
                restated.versions_recorded += 1;
            }
            return Ok(restated);
        }
        Err(LedgerError::Contention {
            process,
            attempts: self.max_attempts,
        })
    }

    /// What restating one process would book, without booking it.
    pub async fn preview_restatement(
        &self,
        process: ProcessId,
    ) -> Result<RestatementPreview, LedgerError> {
        let types: Vec<(&'static str, u32)> = self.registry.types().collect();
        let read = self.store.read(process).await?;
        let current = engine::fold(process, &read, &self.registry)?;
        let plan = engine::plan(&current, &[], &types)
            .map_err(|rejection| LedgerError::Rejected { process, rejection })?;
        let mut deltas: BTreeMap<(BatchId, UnitKey), i128> = BTreeMap::new();
        for l in &plan.lines {
            let id = plan.batches.get(l.batch).id.clone();
            *deltas.entry((id, l.unit)).or_insert(0) += l.amount;
        }
        Ok(RestatementPreview {
            objects: plan.restated_objects,
            deltas: deltas
                .into_iter()
                .filter(|(_, a)| *a != 0)
                .map(|((id, unit), a)| (id, unit, a))
                .collect(),
        })
    }

    /// Books one process's submissions, retrying a lost sequence race.
    async fn book_process(
        &self,
        process: ProcessId,
        submissions: &[Submission],
    ) -> Result<Vec<(usize, Outcome)>, LedgerError> {
        let rejected = |rejection: Rejection| {
            submissions
                .iter()
                .map(|s| (s.index, Outcome::Rejected(rejection.clone())))
                .collect::<Vec<_>>()
        };
        for _attempt in 0..self.max_attempts {
            let read = self.store.read(process).await?;
            let current = engine::fold(process, &read, &self.registry)?;
            let plan = match engine::plan(&current, submissions, &[]) {
                Ok(plan) => plan,
                Err(rejection) => return Ok(rejected(rejection)),
            };
            if !plan.is_empty() {
                match self.commit_plan(process, &current, &plan).await? {
                    Committed::Journal(_) => {}
                    Committed::SequenceTaken => continue,
                    Committed::Rejected(rejection) => return Ok(rejected(rejection)),
                }
            }
            for (type_name, version) in &plan.versions_to_record {
                self.store
                    .record_version(process, type_name, *version)
                    .await?;
            }
            return Ok(plan
                .outcomes
                .iter()
                .map(|(index, pre)| {
                    let outcome = match pre {
                        PreOutcome::Booked => Outcome::Booked,
                        PreOutcome::AlreadyBooked => Outcome::AlreadyBooked,
                        PreOutcome::Stale => Outcome::Stale,
                    };
                    (*index, outcome)
                })
                .collect());
        }
        Err(LedgerError::Contention {
            process,
            attempts: self.max_attempts,
        })
    }

    /// Resolves the plan's batches, finalizes and commits.
    async fn commit_plan(
        &self,
        process: ProcessId,
        current: &engine::Current,
        plan: &engine::Plan,
    ) -> Result<Committed, LedgerError> {
        let ids: Vec<BatchId> = plan
            .batches
            .requests()
            .iter()
            .map(|r| r.id.clone())
            .collect();
        let resolved = self.store.resolve(&ids).await?;
        let commit = match engine::finalize(process, current, plan, &resolved, now()) {
            Ok(commit) => commit,
            Err(rejection) => return Ok(Committed::Rejected(rejection)),
        };
        let lines = commit.lines.len();
        Ok(match self.store.commit(commit).await? {
            CommitOutcome::Committed => Committed::Journal(lines),
            CommitOutcome::SequenceTaken => Committed::SequenceTaken,
        })
    }
}

fn now() -> Timestamp {
    chrono::Utc::now()
}
