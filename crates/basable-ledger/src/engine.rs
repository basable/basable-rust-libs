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
//!
//! A desired state names batches by value and a line carries a key, so the
//! runtime resolves the values first: the row of each batch that has one
//! ([`named_batches`], [`Resolved`]), and a placeholder for each that has
//! none. A placeholder is sound because such a batch has nothing booked: a
//! line only ever carries the key of a row already committed. After the plan
//! the runtime creates the rows its lines land in, and the commit carries
//! their real keys.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::accounting::LedgerBatch;
use crate::balances::Balances;
use crate::desired::Desired;
use crate::erased::{ErasedObject, Registry};
use crate::error::{LedgerError, Rejection, StoreError};
use crate::journal::{Journal, Line, SnapshotDiff};
use crate::jsonpatch;
use crate::store::Commit;
use crate::types::{BatchKey, BatchType, LedgerKey, ObjectId, ProcessId, StoredBatch, UnitKey};

/// One object as the process currently books it: its folded snapshot and
/// the decoded object.
pub(crate) struct CurrentObject<B: LedgerBatch> {
    pub json: Value,
    pub object: Box<dyn ErasedObject<B>>,
}

/// The process as read and folded.
pub(crate) struct Current<B: LedgerBatch> {
    pub sequence: i64,
    pub objects: BTreeMap<ObjectId, CurrentObject<B>>,
    pub balances: Balances<B>,
}

/// Folds a process's journals into its current objects and positions.
pub(crate) fn fold<B: LedgerBatch>(
    process: ProcessId,
    journals: &[Journal],
    registry: &Registry<B>,
) -> Result<Current<B>, LedgerError> {
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
            balances.add(line.object_id, line.batch, line.unit, line.amount);
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
pub(crate) struct Submission<B: LedgerBatch> {
    pub index: usize,
    pub object: Box<dyn ErasedObject<B>>,
}

/// The batches a plan of this call may name, by primary key: those of every
/// current object and of every submission that will be evaluated, the first
/// value naming a primary key standing for it. A stale or duplicate
/// submission is never evaluated, so it names nothing.
pub(crate) fn named_batches<B: LedgerBatch>(
    current: &Current<B>,
    submissions: &[Submission<B>],
) -> BTreeMap<B::PrimaryKey, B> {
    let mut named = BTreeMap::new();
    let mut note = |desired: Desired<B>| {
        for p in desired.positions() {
            named
                .entry(p.batch.primary_key())
                .or_insert_with(|| p.batch.clone());
        }
    };
    for cur in current.objects.values() {
        note(cur.object.desired());
    }
    for s in submissions {
        let settled = current
            .objects
            .get(&s.object.object_id())
            .is_some_and(|cur| s.object.version() <= cur.object.version());
        if !settled {
            note(s.object.desired());
        }
    }
    named
}

/// What a call's batches resolved to: every row found, and a placeholder row
/// for each batch that has none.
pub(crate) struct Resolved<B: LedgerBatch> {
    rows: BTreeMap<B::PrimaryKey, StoredBatch>,
    new: BTreeMap<B::PrimaryKey, B>,
}

impl<B: LedgerBatch> Resolved<B> {
    pub(crate) fn new() -> Self {
        Resolved {
            rows: BTreeMap::new(),
            new: BTreeMap::new(),
        }
    }

    /// Adds what the store found for a batch.
    pub(crate) fn add(&mut self, batch: B, row: Option<StoredBatch>) -> Result<(), StoreError> {
        let primary_key = batch.primary_key();
        let row = match row {
            Some(row) if row.key.is_placeholder() => {
                return Err(StoreError::Corrupt(format!(
                    "batch {primary_key:?} has key {}, which no sequence hands out",
                    row.key.0
                )));
            }
            Some(row) => row,
            None => {
                let row = StoredBatch {
                    key: BatchKey::placeholder(self.new.len()),
                    ledger: batch.ledger(),
                    batch_type: batch.batch_type(),
                };
                self.new.insert(primary_key.clone(), batch);
                row
            }
        };
        self.rows.insert(primary_key, row);
        Ok(())
    }

    /// The value of a batch with no row, by its placeholder.
    pub(crate) fn new_batch(&self, placeholder: BatchKey) -> Option<&B> {
        self.new
            .iter()
            .find(|(pk, _)| self.rows.get(pk).is_some_and(|r| r.key == placeholder))
            .map(|(_, batch)| batch)
    }

    /// Every row, the placeholders' included.
    pub(crate) fn into_rows(self) -> BTreeMap<B::PrimaryKey, StoredBatch> {
        self.rows
    }
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

    /// The new batches a line actually lands in, each with its placeholder,
    /// in primary key order. A batch named with nothing to book gets no row.
    pub(crate) fn new_batches<'a, B: LedgerBatch>(
        &self,
        resolved: &'a Resolved<B>,
    ) -> Vec<(BatchKey, &'a B)> {
        let used: BTreeSet<BatchKey> = self.lines.iter().map(|l| l.batch).collect();
        resolved
            .new
            .iter()
            .filter_map(|(primary_key, batch)| {
                let placeholder = resolved.rows.get(primary_key)?.key;
                used.contains(&placeholder).then_some((placeholder, batch))
            })
            .collect()
    }

    /// The commit: one journal at the process's next sequence, with each
    /// placeholder replaced by the key its row got.
    pub(crate) fn into_commit<B: LedgerBatch>(
        self,
        process: ProcessId,
        current: &Current<B>,
        keys: &BTreeMap<BatchKey, BatchKey>,
    ) -> Commit {
        let lines: Vec<Line> = self
            .lines
            .iter()
            .map(|l| Line {
                batch: keys.get(&l.batch).copied().unwrap_or(l.batch),
                ..*l
            })
            .collect();
        Commit {
            process_id: process,
            after_sequence: current.sequence,
            lines: net(&lines),
            snapshot_diffs: self.diffs,
            versions: self.versions,
        }
    }
}

/// Plans one process's part of a call: the restatement of every current
/// object first, then the submissions in order.
pub(crate) fn plan<B: LedgerBatch>(
    current: &Current<B>,
    submissions: &[Submission<B>],
    resolved: &Resolved<B>,
) -> Result<Plan, Rejection> {
    let mut plan = Plan {
        outcomes: Vec::with_capacity(submissions.len()),
        lines: Vec::new(),
        diffs: Vec::new(),
        versions: BTreeMap::new(),
        restated_objects: 0,
    };
    let mut working = current.balances.clone();
    working.set_rows(resolved.rows.clone());
    // The objects as the call leaves them, for later objects of the same call.
    let mut live: BTreeMap<ObjectId, &dyn ErasedObject<B>> = current
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
        let desired = evaluate(cur.object.as_ref(), &working)?;
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
        let desired = evaluate(s.object.as_ref(), &working)?;
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

    // No check that the journal as a whole balances: the booked positions
    // are the sum of desired states that each balanced per ledger and unit,
    // the desired states above balance, so their difference does too, as
    // long as one key means one ledger and one type for good. A line stores
    // the key alone; that constancy is the row's, which a value that
    // disagrees with it cannot get past (`evaluate`).
    plan.lines = net(&plan.lines);
    Ok(plan)
}

/// Evaluates an object's desired state, checks every batch it names against
/// its row (one key is one ledger and one type) and checks it balances per
/// ledger and unit.
fn evaluate<B: LedgerBatch>(
    object: &dyn ErasedObject<B>,
    working: &Balances<B>,
) -> Result<Desired<B>, Rejection> {
    let desired = object.desired();
    for p in desired.positions() {
        let prefix = || {
            format!(
                "object {} in state {}",
                object.object_id().0,
                object.describe_state()
            )
        };
        let row = working.row_of(&p.batch).ok_or_else(|| {
            Rejection::new(format!(
                "{}: batch {:?} appeared on a second evaluation, so the desired state is not pure",
                prefix(),
                p.batch.primary_key()
            ))
        })?;
        row.check(&p.batch)
            .map_err(|r| Rejection::new(format!("{}: {r}", prefix())))?;
    }
    check_desired_balances(object, &desired)?;
    Ok(desired)
}

/// Left equals right per (ledger, unit) in one desired state; memo outside.
fn check_desired_balances<B: LedgerBatch>(
    object: &dyn ErasedObject<B>,
    desired: &Desired<B>,
) -> Result<(), Rejection> {
    let mut sums: BTreeMap<(LedgerKey, UnitKey), i128> = BTreeMap::new();
    for p in desired.positions() {
        let signed = match p.batch.batch_type() {
            BatchType::Debit => p.amount,
            BatchType::Credit => p
                .amount
                .checked_neg()
                .ok_or_else(|| Rejection::new("amount overflow"))?,
            BatchType::Memo => continue,
        };
        let slot = sums.entry((p.batch.ledger(), p.unit)).or_insert(0);
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
/// closes through the same batch key. Every batch was resolved by `evaluate`.
fn delta_for<B: LedgerBatch>(
    object: ObjectId,
    desired: &Desired<B>,
    working: &Balances<B>,
) -> Vec<Line> {
    let mut wanted: BTreeMap<(BatchKey, UnitKey), i128> = BTreeMap::new();
    for p in desired.positions() {
        let key = working.row_of(&p.batch).expect("evaluated").key;
        let slot = wanted.entry((key, p.unit)).or_insert(0);
        *slot = slot.checked_add(p.amount).expect("amount overflow");
    }
    let booked = working.positions_of(object);
    let keys: BTreeSet<(BatchKey, UnitKey)> = wanted.keys().chain(booked.keys()).copied().collect();
    let mut delta = Vec::new();
    for (batch, unit) in keys {
        let want = wanted.get(&(batch, unit)).copied().unwrap_or(0);
        let have = booked.get(&(batch, unit)).copied().unwrap_or(0);
        let amount = want.checked_sub(have).expect("amount overflow");
        if amount == 0 {
            continue;
        }
        delta.push(Line {
            batch,
            object_id: object,
            unit,
            amount,
        });
    }
    delta
}

fn apply_delta<B: LedgerBatch>(working: &mut Balances<B>, delta: &[Line]) {
    for l in delta {
        working.add(l.object_id, l.batch, l.unit, l.amount);
    }
}

/// Sums the lines per (object, batch, unit), drops the zeros and orders
/// them by that key.
fn net(lines: &[Line]) -> Vec<Line> {
    let mut sums: BTreeMap<(ObjectId, BatchKey, UnitKey), i128> = BTreeMap::new();
    for l in lines {
        *sums.entry((l.object_id, l.batch, l.unit)).or_insert(0) += l.amount;
    }
    sums.into_iter()
        .filter(|(_, a)| *a != 0)
        .map(|((object_id, batch, unit), amount)| Line {
            batch,
            object_id,
            unit,
            amount,
        })
        .collect()
}
