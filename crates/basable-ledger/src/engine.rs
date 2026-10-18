//! The pure engine: fold a process's journals into its current objects and
//! positions, plan what a call books, and turn the plan into a commit once
//! the batches are resolved. Nothing here is async and nothing reads a
//! clock; the runtime in `ledger.rs` drives it against a store.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;

use crate::accounting::BatchAccount;
use crate::balances::Balances;
use crate::batches::{BatchMode, BatchRef, BatchRequest, Batches};
use crate::desired::{Desired, Position};
use crate::erased::{ErasedObject, Registry};
use crate::error::{LedgerError, Rejection, StoreError};
use crate::journal::SnapshotDiff;
use crate::jsonpatch;
use crate::store::{BatchRow, Commit, CommitLine, LineBatch, NewBatch, ProcessRead};
use crate::types::{
    BatchId, BatchStatus, BatchType, LedgerKey, ObjectId, ProcessId, Timestamp, UnitKey,
};

/// One object as the process currently books it: its folded snapshot and
/// the decoded object.
pub(crate) struct CurrentObject {
    pub json: Value,
    pub object: Box<dyn ErasedObject>,
}

/// The process as read and folded.
pub(crate) struct Current {
    pub sequence: i64,
    pub objects: BTreeMap<ObjectId, CurrentObject>,
    pub balances: Balances,
    pub versions: HashMap<String, u32>,
}

/// Folds a process read into its current objects and positions.
pub(crate) fn fold(
    process: ProcessId,
    read: &ProcessRead,
    registry: &Registry,
) -> Result<Current, LedgerError> {
    let mut snapshots: BTreeMap<ObjectId, (String, Value)> = BTreeMap::new();
    let mut balances = Balances::new();
    for journal in &read.journals {
        for diff in &journal.snapshot_diffs {
            match (&diff.full, &diff.patch) {
                (Some(full), _) => {
                    snapshots.insert(diff.object_id, (diff.type_name.clone(), full.clone()));
                }
                (None, Some(patch)) => match snapshots.get_mut(&diff.object_id) {
                    Some((_, json)) => jsonpatch::apply(json, patch),
                    None => {
                        return Err(StoreError::Corrupt(format!(
                            "journal {} of process {}: patch for object {} before its first snapshot",
                            journal.sequence, process.0, diff.object_id.0
                        ))
                        .into());
                    }
                },
                (None, None) => {
                    return Err(StoreError::Corrupt(format!(
                        "journal {} of process {}: diff for object {} with neither full nor patch",
                        journal.sequence, process.0, diff.object_id.0
                    ))
                    .into());
                }
            }
        }
        for line in &journal.lines {
            let row = read.batches.get(&line.batch_key).ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "journal {} of process {}: line names batch key {} the read did not return",
                    journal.sequence, process.0, line.batch_key.0
                ))
            })?;
            balances.add(
                line.object_id,
                &row.id,
                (row.ledger, row.batch_type),
                line.unit,
                line.amount,
            );
        }
    }
    let mut objects = BTreeMap::new();
    for (id, (type_name, json)) in snapshots {
        let handler = registry
            .get(&type_name)
            .ok_or_else(|| LedgerError::UnknownType(type_name.clone()))?;
        let object = handler
            .decode(&json)
            .map_err(|reason| LedgerError::Undecodable {
                process,
                object: id,
                reason,
            })?;
        objects.insert(id, CurrentObject { json, object });
    }
    Ok(Current {
        sequence: read.sequence(),
        objects,
        balances,
        versions: read.versions.clone(),
    })
}

/// A submitted object, with its position in the call.
pub(crate) struct Submission {
    pub index: usize,
    pub object: Box<dyn ErasedObject>,
}

/// What the engine decided for one submission before the commit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PreOutcome {
    Booked,
    AlreadyBooked,
    Stale,
}

/// A planned delta line, before batches are resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PlannedLine {
    pub batch: BatchRef,
    pub object: ObjectId,
    pub unit: UnitKey,
    pub amount: i128,
}

/// What a call books for one process.
pub(crate) struct Plan {
    pub outcomes: Vec<(usize, PreOutcome)>,
    pub batches: Batches,
    pub lines: Vec<PlannedLine>,
    pub diffs: Vec<SnapshotDiff>,
    pub event_time: Option<Timestamp>,
    /// Types whose recorded version must become the running one.
    pub versions_to_record: Vec<(String, u32)>,
    /// How many objects were restated ahead of the submissions.
    pub restated_objects: usize,
}

impl Plan {
    /// Nothing to commit: every submission was a duplicate or stale and no
    /// restatement was due.
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.diffs.is_empty()
    }
}

/// Plans one process's part of a call. `restate_types` are the registered
/// types to check for a pending restatement beyond the submitted ones (the
/// startup restatement passes every registered type).
pub(crate) fn plan(
    current: &Current,
    submissions: &[Submission],
    restate_types: &[(&'static str, u32)],
) -> Result<Plan, Rejection> {
    let mut plan = Plan {
        outcomes: Vec::with_capacity(submissions.len()),
        batches: Batches::new(),
        lines: Vec::new(),
        diffs: Vec::new(),
        event_time: None,
        versions_to_record: Vec::new(),
        restated_objects: 0,
    };
    let mut working = current.balances.clone();
    // The objects as the call leaves them, for later objects of the same call.
    let mut live: BTreeMap<ObjectId, &dyn ErasedObject> = current
        .objects
        .iter()
        .map(|(id, o)| (*id, o.object.as_ref()))
        .collect();

    // Restatement first: every type in question that is behind in this process.
    let mut types: BTreeMap<&'static str, u32> = restate_types.iter().copied().collect();
    let mut submitted: BTreeSet<&'static str> = BTreeSet::new();
    for s in submissions {
        types.insert(s.object.type_name(), s.object.rules_version());
        submitted.insert(s.object.type_name());
    }
    for (type_name, running) in types {
        let recorded = current.versions.get(type_name).copied();
        let has_objects = current
            .objects
            .values()
            .any(|o| o.object.type_name() == type_name);
        let behind = match recorded {
            Some(r) => r < running,
            None => has_objects,
        };
        // A type this process has no objects of and that is not being booked
        // gets no version row: the row marks a type the process booked.
        if !behind && !submitted.contains(type_name) {
            continue;
        }
        if behind {
            for (id, cur) in &current.objects {
                if cur.object.type_name() != type_name {
                    continue;
                }
                let desired = evaluate(cur.object.as_ref(), &mut plan.batches)?;
                let delta = delta_for(*id, &desired, &mut plan.batches, &working);
                apply_delta(&mut working, &plan.batches, &delta);
                plan.lines.extend(delta);
                plan.restated_objects += 1;
            }
        }
        if recorded.is_none_or(|r| r < running) {
            plan.versions_to_record
                .push((type_name.to_owned(), running));
        }
    }

    // Then the submissions, in order.
    for s in submissions {
        let id = s.object.object_id();
        let cur = live.get(&id).copied();
        if let Some(cur) = cur {
            if cur.type_name() != s.object.type_name() {
                return Err(Rejection::new(format!(
                    "object {} is a {} but was submitted as a {}",
                    id.0,
                    cur.type_name(),
                    s.object.type_name()
                )));
            }
            if s.object.version() == cur.version() {
                plan.outcomes.push((s.index, PreOutcome::AlreadyBooked));
                continue;
            }
            if s.object.version() < cur.version() {
                plan.outcomes.push((s.index, PreOutcome::Stale));
                continue;
            }
        }
        let desired = evaluate(s.object.as_ref(), &mut plan.batches)?;
        match cur {
            Some(cur) if s.object.state_eq(cur) => {
                let delta = delta_for(id, &desired, &mut plan.batches, &working);
                if !delta.is_empty() && !cur.amount_change_allowed() {
                    return Err(Rejection::new(format!(
                        "object {} changed its amounts in state {}, which allows no amount change",
                        id.0,
                        cur.describe_state()
                    )));
                }
            }
            _ => {
                if !s.object.change_allowed(cur) {
                    return Err(Rejection::new(format!(
                        "object {} may not move from {} to {}",
                        id.0,
                        cur.map(|c| c.describe_state())
                            .unwrap_or_else(|| "nothing".to_owned()),
                        s.object.describe_state()
                    )));
                }
            }
        }
        s.object
            .validate_balances(&working, &desired)
            .map_err(|r| Rejection::new(format!("object {}: {}", id.0, r.message())))?;
        let delta = delta_for(id, &desired, &mut plan.batches, &working);
        apply_delta(&mut working, &plan.batches, &delta);
        plan.lines.extend(delta);

        let json = s.object.to_json()?;
        let diff = match current.objects.get(&id) {
            None => SnapshotDiff {
                object_id: id,
                type_name: s.object.type_name().to_owned(),
                full: Some(json),
                patch: None,
            },
            Some(cur) => SnapshotDiff {
                object_id: id,
                type_name: s.object.type_name().to_owned(),
                full: None,
                patch: Some(jsonpatch::diff(&cur.json, &json)),
            },
        };
        plan.diffs.push(diff);
        let entered = s.object.entered_at();
        plan.event_time = Some(plan.event_time.map_or(entered, |t| t.max(entered)));
        live.insert(id, s.object.as_ref());
        plan.outcomes.push((s.index, PreOutcome::Booked));
    }

    if let Some(c) = plan.batches.conflicts().first() {
        return Err(Rejection::new(format!(
            "batch {} named with two accounts: ledger {} {} and ledger {} {}",
            c.id, c.first.0.0, c.first.1, c.second.0.0, c.second.1
        )));
    }
    plan.lines = net(&plan.lines);
    check_journal_balances(&plan)?;
    Ok(plan)
}

/// Evaluates an object's desired state into the shared recorder and checks
/// it balances per ledger and unit.
fn evaluate(object: &dyn ErasedObject, shared: &mut Batches) -> Result<Desired, Rejection> {
    let mut local = Batches::new();
    let desired = object.desired(&mut local);
    if let Some(c) = local.conflicts().first() {
        return Err(Rejection::new(format!(
            "object {}: batch {} named with two accounts: ledger {} {} and ledger {} {}",
            object.object_id().0,
            c.id,
            c.first.0.0,
            c.first.1,
            c.second.0.0,
            c.second.1
        )));
    }
    let map = shared.absorb(&local);
    let desired = Desired::new(
        desired
            .positions()
            .iter()
            .map(|p| Position {
                batch: map[p.batch.0],
                unit: p.unit,
                amount: p.amount,
            })
            .collect(),
    );
    check_desired_balances(object, &desired, shared)?;
    Ok(desired)
}

/// Left equals right per (ledger, unit) in one desired state; memo outside.
fn check_desired_balances(
    object: &dyn ErasedObject,
    desired: &Desired,
    batches: &Batches,
) -> Result<(), Rejection> {
    let mut sums: BTreeMap<(LedgerKey, UnitKey), i128> = BTreeMap::new();
    for p in desired.positions() {
        let req = batches.get(p.batch);
        let signed = match req.batch_type {
            BatchType::Debit => p.amount,
            BatchType::Credit => p
                .amount
                .checked_neg()
                .ok_or_else(|| Rejection::new("amount overflow"))?,
            BatchType::Memo => continue,
        };
        let slot = sums.entry((req.ledger, p.unit)).or_insert(0);
        *slot = slot
            .checked_add(signed)
            .ok_or_else(|| Rejection::new("amount overflow"))?;
    }
    if let Some(((ledger, unit), sum)) = sums.iter().find(|(_, s)| **s != 0) {
        return Err(Rejection::new(format!(
            "object {} in state {}: ledger {} unit {} does not balance, left minus right is {sum}",
            object.object_id().0,
            object.describe_state(),
            ledger.0,
            unit.0
        )));
    }
    Ok(())
}

/// The delta that brings an object's booked positions to a desired state,
/// per logical batch and unit. A booked position the desired state no
/// longer names closes through the same batch id, so that batch is recorded
/// too.
fn delta_for(
    object: ObjectId,
    desired: &Desired,
    batches: &mut Batches,
    working: &Balances,
) -> Vec<PlannedLine> {
    let mut wanted: BTreeMap<(BatchId, UnitKey), i128> = BTreeMap::new();
    for p in desired.positions() {
        let id = batches.get(p.batch).id.clone();
        let slot = wanted.entry((id, p.unit)).or_insert(0);
        *slot = slot.checked_add(p.amount).expect("amount overflow");
    }
    let booked = working.positions_of(object);
    let keys: BTreeSet<(BatchId, UnitKey)> = wanted.keys().chain(booked.keys()).cloned().collect();
    let mut delta = Vec::new();
    for key in keys {
        let want = wanted.get(&key).copied().unwrap_or(0);
        let have = booked.get(&key).copied().unwrap_or(0);
        let amount = want.checked_sub(have).expect("amount overflow");
        if amount == 0 {
            continue;
        }
        let (ledger, batch_type) = working
            .batch_account(&key.0)
            .or_else(|| {
                batches
                    .requests()
                    .iter()
                    .find(|r| r.id == key.0)
                    .map(|r| (r.ledger, r.batch_type))
            })
            .expect("a named or booked batch has an account");
        let batch = batches.open(Flat(ledger, batch_type), key.0.clone());
        delta.push(PlannedLine {
            batch,
            object,
            unit: key.1,
            amount,
        });
    }
    delta
}

struct Flat(LedgerKey, BatchType);

impl BatchAccount for Flat {
    fn ledger(&self) -> LedgerKey {
        self.0
    }

    fn batch_type(&self) -> BatchType {
        self.1
    }
}

fn apply_delta(working: &mut Balances, batches: &Batches, delta: &[PlannedLine]) {
    for l in delta {
        let req = batches.get(l.batch);
        working.add(
            l.object,
            &req.id,
            (req.ledger, req.batch_type),
            l.unit,
            l.amount,
        );
    }
}

/// Left equals right per (ledger, unit) over the whole journal.
fn check_journal_balances(plan: &Plan) -> Result<(), Rejection> {
    let mut sums: BTreeMap<(LedgerKey, UnitKey), i128> = BTreeMap::new();
    for l in &plan.lines {
        let req = plan.batches.get(l.batch);
        let signed = match req.batch_type {
            BatchType::Debit => l.amount,
            BatchType::Credit => -l.amount,
            BatchType::Memo => continue,
        };
        *sums.entry((req.ledger, l.unit)).or_insert(0) += signed;
    }
    if let Some(((ledger, unit), sum)) = sums.iter().find(|(_, s)| **s != 0) {
        return Err(Rejection::new(format!(
            "journal does not balance: ledger {} unit {} left minus right is {sum}",
            ledger.0, unit.0
        )));
    }
    Ok(())
}

/// Sums the lines per (object, batch, unit) and drops the zeros.
fn net(lines: &[PlannedLine]) -> Vec<PlannedLine> {
    let mut sums: BTreeMap<(ObjectId, usize, UnitKey), i128> = BTreeMap::new();
    for l in lines {
        *sums.entry((l.object, l.batch.0, l.unit)).or_insert(0) += l.amount;
    }
    sums.into_iter()
        .filter(|(_, a)| *a != 0)
        .map(|((object, batch, unit), amount)| PlannedLine {
            batch: BatchRef(batch),
            object,
            unit,
            amount,
        })
        .collect()
}

/// Turns a plan into a commit once the store has resolved the batches: an
/// open batch is used, a consumed or closed one rolls to the next epoch, a
/// missing one is created (or rejected under `existing`), and a batch that
/// exists under another ledger or type rejects. `now` is the event time of a
/// journal with no submissions (a restatement books prospectively).
pub(crate) fn finalize(
    process: ProcessId,
    current: &Current,
    plan: &Plan,
    resolved: &[Option<BatchRow>],
    now: Timestamp,
) -> Result<Commit, Rejection> {
    let requests = plan.batches.requests();
    assert_eq!(requests.len(), resolved.len(), "one resolution per request");
    let mut new_batches = Vec::new();
    let mut targets: Vec<LineBatch> = Vec::with_capacity(requests.len());
    for (req, row) in requests.iter().zip(resolved) {
        let target = match row {
            None => match req.mode {
                BatchMode::Open => LineBatch::New(push_new(&mut new_batches, req, 1)),
                BatchMode::Existing => {
                    return Err(Rejection::new(format!(
                        "batch {} does not exist and the desired state asks for an existing one",
                        req.id
                    )));
                }
            },
            Some(row) => {
                if row.ledger != req.ledger || row.batch_type != req.batch_type {
                    return Err(Rejection::new(format!(
                        "batch {} exists in ledger {} as {} but was named in ledger {} as {}",
                        req.id, row.ledger.0, row.batch_type, req.ledger.0, req.batch_type
                    )));
                }
                match (row.status, req.mode) {
                    (BatchStatus::Open, _) => LineBatch::Key(row.key),
                    (_, BatchMode::Open) => {
                        LineBatch::New(push_new(&mut new_batches, req, row.epoch + 1))
                    }
                    (_, BatchMode::Existing) => {
                        return Err(Rejection::new(format!(
                            "batch {} is {:?} and the desired state asks for an existing open one",
                            req.id, row.status
                        )));
                    }
                }
            }
        };
        targets.push(target);
    }
    let lines = plan
        .lines
        .iter()
        .map(|l| CommitLine {
            batch: targets[l.batch.0],
            object_id: l.object,
            unit: l.unit,
            amount: l.amount,
        })
        .collect();
    Ok(Commit {
        process_id: process,
        after_sequence: current.sequence,
        new_batches,
        event_time: plan.event_time.unwrap_or(now),
        lines,
        snapshot_diffs: plan.diffs.clone(),
    })
}

fn push_new(new_batches: &mut Vec<NewBatch>, req: &BatchRequest, epoch: i32) -> usize {
    if let Some(i) = new_batches.iter().position(|b| b.id == req.id) {
        return i;
    }
    new_batches.push(NewBatch {
        id: req.id.clone(),
        epoch,
        ledger: req.ledger,
        batch_type: req.batch_type,
    });
    new_batches.len() - 1
}
