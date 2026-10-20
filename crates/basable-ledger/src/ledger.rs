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
use crate::types::{BatchKey, ProcessId, UnitKey};

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
    /// Objects whose positions moved under the running rules.
    pub objects: usize,
    /// Lines the restatement journal booked; 0 means no journal was written
    /// and only the version index moved.
    pub lines: usize,
}

/// A restatement's effect before it books: the net change per batch and unit.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RestatementPreview {
    /// Objects that would be re-evaluated.
    pub objects: usize,
    /// Net lines, per (batch key, unit).
    pub deltas: Vec<(BatchKey, UnitKey, i128)>,
}

/// What [`Ledger::restate_behind`] did at startup, over every process that
/// was behind.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RestatementReport {
    /// Processes restated.
    pub processes: usize,
    /// Objects whose positions moved.
    pub objects: usize,
    /// Lines booked.
    pub lines: usize,
}

/// How many processes behind [`Ledger::restate_behind`] fetches per page.
const RESTATE_PAGE: u32 = 256;

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
    /// The sequence was taken under the commit: read again, recompute.
    Retry,
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
        let journals = self.store.read(process).await?;
        let current = engine::fold(process, &journals, &self.registry)?;
        Ok(current.balances)
    }

    /// Restates one process under the running rules: every object is
    /// re-evaluated and the difference is appended as one journal, which
    /// also moves the version index. Nothing to do is a `Restated` with zero
    /// lines; the index is moved on its own then, so the startup worklist
    /// stops listing the process.
    pub async fn restate(&self, process: ProcessId) -> Result<Restated, LedgerError> {
        for _attempt in 0..self.max_attempts {
            let journals = self.store.read(process).await?;
            let current = engine::fold(process, &journals, &self.registry)?;
            let plan = engine::plan(&current, &[])
                .map_err(|rejection| LedgerError::Rejected { process, rejection })?;
            let mut restated = Restated {
                objects: plan.restated_objects,
                lines: 0,
            };
            if plan.is_empty() {
                if !plan.versions.is_empty() {
                    self.store.record_versions(process, &plan.versions).await?;
                }
                return Ok(restated);
            }
            match self.commit_plan(process, &current, plan).await? {
                Committed::Journal(lines) => restated.lines = lines,
                Committed::Retry => continue,
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
        let journals = self.store.read(process).await?;
        let current = engine::fold(process, &journals, &self.registry)?;
        let plan = engine::plan(&current, &[])
            .map_err(|rejection| LedgerError::Rejected { process, rejection })?;
        let mut deltas: BTreeMap<(BatchKey, UnitKey), i128> = BTreeMap::new();
        for l in &plan.lines {
            *deltas.entry((l.batch, l.unit)).or_insert(0) += l.amount;
        }
        Ok(RestatementPreview {
            objects: plan.restated_objects,
            deltas: deltas
                .into_iter()
                .filter(|(_, a)| *a != 0)
                .map(|((key, unit), a)| (key, unit, a))
                .collect(),
        })
    }

    /// Restates every process the version index lists behind the running
    /// [`Accounting::VERSION`] of a registered type, and returns once none
    /// is. A version changes only with a deployment, so this runs once, when
    /// the new code starts: before it serves, or alongside, since a process
    /// still behind is brought to the running rules by its next booking
    /// anyway. Any number of replicas may run it at the same time; the
    /// sequence makes a double restatement of one process land once. While a
    /// replica of the previous deployment is still booking, its bookings put
    /// processes back on the worklist and this pass takes them again; it
    /// settles when that replica is gone. A process whose desired states no
    /// longer balance under the new rules stops it with
    /// `LedgerError::Rejected`.
    pub async fn restate_behind(&self) -> Result<RestatementReport, LedgerError> {
        let mut report = RestatementReport::default();
        for (type_name, version) in self.registry.types() {
            loop {
                let behind = self
                    .store
                    .processes_behind(type_name, version, RESTATE_PAGE)
                    .await?;
                if behind.is_empty() {
                    break;
                }
                for process in behind {
                    let restated = self.restate(process).await?;
                    report.processes += 1;
                    report.objects += restated.objects;
                    report.lines += restated.lines;
                }
            }
        }
        Ok(report)
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
            let journals = self.store.read(process).await?;
            let current = engine::fold(process, &journals, &self.registry)?;
            let plan = match engine::plan(&current, submissions) {
                Ok(plan) => plan,
                Err(rejection) => return Ok(rejected(rejection)),
            };
            let outcomes = plan.outcomes.clone();
            if !plan.is_empty() {
                match self.commit_plan(process, &current, plan).await? {
                    Committed::Journal(_) => {}
                    Committed::Retry => continue,
                }
            }
            return Ok(outcomes
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

    /// Commits the plan as one journal at the process's next sequence.
    async fn commit_plan(
        &self,
        process: ProcessId,
        current: &engine::Current,
        plan: engine::Plan,
    ) -> Result<Committed, LedgerError> {
        let commit = plan.into_commit(process, current);
        let lines = commit.lines.len();
        Ok(match self.store.commit(commit).await? {
            CommitOutcome::Committed => Committed::Journal(lines),
            CommitOutcome::SequenceTaken => Committed::Retry,
        })
    }
}
