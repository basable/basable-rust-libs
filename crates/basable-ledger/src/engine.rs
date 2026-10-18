//! The pure engine: fold a process's journals into its current objects and
//! positions, plan what a call books, and turn the plan into a commit.
//! Nothing here is async and nothing reads a clock; the store stamps the
//! commit time, and the runtime in `ledger.rs` drives it against a store.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::balances::Balances;
use crate::desired::Desired;
use crate::erased::{ErasedObject, Registry};
use crate::error::{LedgerError, Rejection, StoreError};
use crate::journal::{Journal, Line, SnapshotDiff};
use crate::jsonpatch;
use crate::store::Commit;
use crate::types::{Batch, BatchKey, BatchType, LedgerKey, ObjectId, ProcessId, UnitKey};

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
    pub versions: BTreeMap<String, u32>,
}

/// Folds a process's journals into its current objects, positions and
/// rules versions.
pub(crate) fn fold(
    process: ProcessId,
    journals: &[Journal],
    registry: &Registry,
) -> Result<Current, LedgerError> {
    let mut snapshots: BTreeMap<ObjectId, (String, Value)> = BTreeMap::new();
    let mut balances = Balances::new();
    for journal in journals {
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
            balances
                .add(line.object_id, line.batch, line.unit, line.amount)
                .map_err(|(ledger, batch_type)| {
                    StoreError::Corrupt(format!(
                        "journal {} of process {}: batch {} is ledger {} {} but an earlier line has it as ledger {} {}",
                        journal.sequence,
                        process.0,
                        line.batch.key.0,
                        line.batch.ledger.0,
                        line.batch.batch_type,
                        ledger.0,
                        batch_type
                    ))
                })?;
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
        sequence: journals.last().map(|j| j.sequence).unwrap_or(0),
        objects,
        balances,
        versions: Journal::versions_of(journals),
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

/// What a call books for one process.
pub(crate) struct Plan {
    pub outcomes: Vec<(usize, PreOutcome)>,
    pub lines: Vec<Line>,
    pub diffs: Vec<SnapshotDiff>,
    /// Types whose recorded version becomes the running one with this
    /// journal: written in its `rules_versions`, and the index rows with it.
    pub rules_versions: BTreeMap<String, u32>,
    /// How many objects were restated ahead of the submissions.
    pub restated_objects: usize,
}

impl Plan {
    /// Nothing to commit: every submission was a duplicate or stale, no
    /// restatement was due and no version moves.
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.diffs.is_empty() && self.rules_versions.is_empty()
    }

    /// The commit: one journal at the process's next sequence.
    pub(crate) fn into_commit(self, process: ProcessId, current: &Current) -> Commit {
        Commit {
            process_id: process,
            after_sequence: current.sequence,
            lines: self.lines,
            snapshot_diffs: self.diffs,
            rules_versions: self.rules_versions,
        }
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
        lines: Vec::new(),
        diffs: Vec::new(),
        rules_versions: BTreeMap::new(),
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
                let desired = evaluate(cur.object.as_ref(), &mut working)?;
                let delta = delta_for(*id, &desired, &working);
                apply_delta(&mut working, &delta);
                plan.lines.extend(delta);
                plan.restated_objects += 1;
            }
        }
        if recorded.is_none_or(|r| r < running) {
            plan.rules_versions.insert(type_name.to_owned(), running);
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
        let desired = evaluate(s.object.as_ref(), &mut working)?;
        match cur {
            Some(cur) if s.object.state_eq(cur) => {
                let delta = delta_for(id, &desired, &working);
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
        let delta = delta_for(id, &desired, &working);
        apply_delta(&mut working, &delta);
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
        live.insert(id, s.object.as_ref());
        plan.outcomes.push((s.index, PreOutcome::Booked));
    }

    plan.lines = net(&plan.lines);
    check_journal_balances(&plan.lines)?;
    Ok(plan)
}

/// Evaluates an object's desired state, notes every batch it names against
/// the ones the process has booked (one key is one ledger and one type, for
/// good) and checks it balances per ledger and unit.
fn evaluate(object: &dyn ErasedObject, working: &mut Balances) -> Result<Desired, Rejection> {
    let desired = object.desired();
    for p in desired.positions() {
        working.note(p.batch).map_err(|(ledger, batch_type)| {
            Rejection::new(format!(
                "object {} in state {}: batch {} is booked in ledger {} as {} but the desired state names it in ledger {} as {}",
                object.object_id().0,
                object.describe_state(),
                p.batch.key.0,
                ledger.0,
                batch_type,
                p.batch.ledger.0,
                p.batch.batch_type
            ))
        })?;
    }
    check_desired_balances(object, &desired)?;
    Ok(desired)
}

/// Left equals right per (ledger, unit) in one desired state; memo outside.
fn check_desired_balances(object: &dyn ErasedObject, desired: &Desired) -> Result<(), Rejection> {
    let mut sums: BTreeMap<(LedgerKey, UnitKey), i128> = BTreeMap::new();
    for p in desired.positions() {
        let signed = match p.batch.batch_type {
            BatchType::Debit => p.amount,
            BatchType::Credit => p
                .amount
                .checked_neg()
                .ok_or_else(|| Rejection::new("amount overflow"))?,
            BatchType::Memo => continue,
        };
        let slot = sums.entry((p.batch.ledger, p.unit)).or_insert(0);
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
/// per batch and unit. A booked position the desired state no longer names
/// closes through the same batch, whose ledger and type the booked lines
/// carry.
fn delta_for(object: ObjectId, desired: &Desired, working: &Balances) -> Vec<Line> {
    let mut wanted: BTreeMap<(BatchKey, UnitKey), (Batch, i128)> = BTreeMap::new();
    for p in desired.positions() {
        let slot = wanted.entry((p.batch.key, p.unit)).or_insert((p.batch, 0));
        slot.1 = slot.1.checked_add(p.amount).expect("amount overflow");
    }
    let booked = working.positions_of(object);
    let keys: BTreeSet<(BatchKey, UnitKey)> = wanted.keys().chain(booked.keys()).copied().collect();
    let mut delta = Vec::new();
    for key in keys {
        let want = wanted.get(&key).map(|(_, a)| *a).unwrap_or(0);
        let have = booked.get(&key).map(|(_, a)| *a).unwrap_or(0);
        let amount = want.checked_sub(have).expect("amount overflow");
        if amount == 0 {
            continue;
        }
        let batch = wanted
            .get(&key)
            .or_else(|| booked.get(&key))
            .map(|(b, _)| *b)
            .expect("a wanted or booked position has its batch");
        delta.push(Line {
            batch,
            object_id: object,
            unit: key.1,
            amount,
        });
    }
    delta
}

fn apply_delta(working: &mut Balances, delta: &[Line]) {
    for l in delta {
        working
            .add(l.object_id, l.batch, l.unit, l.amount)
            .expect("every batch in a delta was noted first");
    }
}

/// Left equals right per (ledger, unit) over the whole journal.
fn check_journal_balances(lines: &[Line]) -> Result<(), Rejection> {
    let mut sums: BTreeMap<(LedgerKey, UnitKey), i128> = BTreeMap::new();
    for l in lines {
        let signed = match l.batch.batch_type {
            BatchType::Debit => l.amount,
            BatchType::Credit => -l.amount,
            BatchType::Memo => continue,
        };
        *sums.entry((l.batch.ledger, l.unit)).or_insert(0) += signed;
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
fn net(lines: &[Line]) -> Vec<Line> {
    let mut sums: BTreeMap<(ObjectId, BatchKey, UnitKey), (Batch, i128)> = BTreeMap::new();
    for l in lines {
        let slot = sums
            .entry((l.object_id, l.batch.key, l.unit))
            .or_insert((l.batch, 0));
        slot.1 += l.amount;
    }
    sums.into_iter()
        .filter(|(_, (_, a))| *a != 0)
        .map(|((object_id, _, unit), (batch, amount))| Line {
            batch,
            object_id,
            unit,
            amount,
        })
        .collect()
}
