//! The pure engine: fold a process's journals into its current objects and
//! positions, plan what a call books, and turn the plan into a commit.
//! Nothing here is async and nothing reads a clock; the store stamps the
//! commit time, and the runtime in `ledger.rs` drives it against a store.
//! The engine knows no recorded rules version: every plan re-evaluates every
//! object of the process under the running rules and books the difference,
//! so a rules change is booked like any other difference, at the process's
//! next touch. The version each type was evaluated under goes to the store
//! with the commit, for the startup restatement's worklist, and is never
//! read back by a booking.

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
}

/// Folds a process's journals into its current objects and positions.
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
    /// The running rules version of every object type present in the
    /// process after the call: what the commit writes to the version index.
    pub versions: BTreeMap<String, u32>,
    /// Current objects whose booked positions no longer matched their desired
    /// state under the running rules, moved ahead of the submissions.
    pub restated_objects: usize,
}

impl Plan {
    /// Nothing to commit: every submission was a duplicate or stale and every
    /// current object already matches the running rules.
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.diffs.is_empty()
    }

    /// The commit: one journal at the process's next sequence.
    pub(crate) fn into_commit(self, process: ProcessId, current: &Current) -> Commit {
        Commit {
            process_id: process,
            after_sequence: current.sequence,
            lines: self.lines,
            snapshot_diffs: self.diffs,
            versions: self.versions,
        }
    }
}

/// Plans one process's part of a call: the restatement of every current
/// object first, then the submissions in order.
pub(crate) fn plan(current: &Current, submissions: &[Submission]) -> Result<Plan, Rejection> {
    let mut plan = Plan {
        outcomes: Vec::with_capacity(submissions.len()),
        lines: Vec::new(),
        diffs: Vec::new(),
        versions: BTreeMap::new(),
        restated_objects: 0,
    };
    let mut working = current.balances.clone();
    // The objects as the call leaves them, for later objects of the same call.
    let mut live: BTreeMap<ObjectId, &dyn ErasedObject> = current
        .objects
        .iter()
        .map(|(id, o)| (*id, o.object.as_ref()))
        .collect();

    // Every current object first: under the running rules its booked
    // positions must equal the desired state of its folded snapshot. A
    // difference means the rules changed since it was last booked, and it is
    // booked here, ahead of the submissions, so that a submission's own
    // delta below is the object's own change and nothing else.
    for (id, cur) in &current.objects {
        let desired = evaluate(cur.object.as_ref(), &mut working)?;
        let delta = delta_for(*id, &desired, &working);
        if !delta.is_empty() {
            apply_delta(&mut working, &delta);
            plan.lines.extend(delta);
            plan.restated_objects += 1;
        }
        plan.versions.insert(
            cur.object.type_name().to_owned(),
            cur.object.rules_version(),
        );
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
        plan.versions
            .insert(s.object.type_name().to_owned(), s.object.rules_version());
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
