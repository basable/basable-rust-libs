//! The booked balances of a process, as `validate_balances` sees them: per
//! object, per batch key and unit, computed from the process's journals on
//! every read. No position table exists anywhere.

use std::collections::BTreeMap;

use crate::accounting::{LedgerBatch, Unit};
use crate::types::{BatchKey, ObjectId, StoredBatch, UnitKey};

/// The positions of every object of a process, and the batches the call in
/// hand has resolved, so that a balance can be asked for by the batch's
/// value. A stored line carries the key alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Balances<B: LedgerBatch> {
    by_object: BTreeMap<ObjectId, BTreeMap<(BatchKey, UnitKey), i128>>,
    rows: BTreeMap<B::PrimaryKey, StoredBatch>,
}

impl<B: LedgerBatch> Default for Balances<B> {
    fn default() -> Self {
        Balances {
            by_object: BTreeMap::new(),
            rows: BTreeMap::new(),
        }
    }
}

impl<B: LedgerBatch> Balances<B> {
    /// No positions at all.
    pub fn new() -> Self {
        Self::default()
    }

    /// One object's positions. An object the process has never booked has
    /// none.
    pub fn of(&self, object: ObjectId) -> ObjectBalances<'_, B> {
        ObjectBalances {
            positions: self.by_object.get(&object),
            rows: &self.rows,
        }
    }

    /// The balance of one batch in one unit over every object of the
    /// process. A batch no object of the process names under the running
    /// rules holds nothing here, since every booking first brings the
    /// process to those rules; [`batch_by_key`](Self::batch_by_key) reads
    /// any key the journals hold.
    pub fn batch(&self, batch: &B, unit: impl Unit) -> i128 {
        match self.rows.get(&batch.primary_key()) {
            Some(row) => self.batch_by_key(row.key, unit),
            None => 0,
        }
    }

    /// The balance of one batch key in one unit over every object of the
    /// process.
    pub fn batch_by_key(&self, key: BatchKey, unit: impl Unit) -> i128 {
        let unit = unit.unit_key();
        self.by_object
            .values()
            .filter_map(|p| p.get(&(key, unit)))
            .sum()
    }

    /// The objects with positions, in id order.
    pub fn objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.by_object.keys().copied()
    }

    /// The row the call resolved a batch to, placeholder included.
    pub(crate) fn row_of(&self, batch: &B) -> Option<StoredBatch> {
        self.rows.get(&batch.primary_key()).copied()
    }

    /// What the call resolved its batches to.
    pub(crate) fn set_rows(&mut self, rows: BTreeMap<B::PrimaryKey, StoredBatch>) {
        self.rows = rows;
    }

    /// Adds a line's amount to its object's position.
    pub(crate) fn add(&mut self, object: ObjectId, batch: BatchKey, unit: UnitKey, amount: i128) {
        let slot = self
            .by_object
            .entry(object)
            .or_default()
            .entry((batch, unit))
            .or_insert(0);
        *slot = slot.checked_add(amount).expect("position overflow");
    }

    /// One object's nonzero positions.
    pub(crate) fn positions_of(&self, object: ObjectId) -> BTreeMap<(BatchKey, UnitKey), i128> {
        self.by_object
            .get(&object)
            .map(|p| {
                p.iter()
                    .filter(|(_, a)| **a != 0)
                    .map(|(key, a)| (*key, *a))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One object's positions.
#[derive(Debug)]
pub struct ObjectBalances<'a, B: LedgerBatch> {
    positions: Option<&'a BTreeMap<(BatchKey, UnitKey), i128>>,
    rows: &'a BTreeMap<B::PrimaryKey, StoredBatch>,
}

impl<B: LedgerBatch> ObjectBalances<'_, B> {
    /// The object's balance on one batch in one unit; 0 when none.
    pub fn amount(&self, batch: &B, unit: impl Unit) -> i128 {
        match self.rows.get(&batch.primary_key()) {
            Some(row) => self.amount_by_key(row.key, unit),
            None => 0,
        }
    }

    /// The object's balance on one batch key in one unit; 0 when none.
    pub fn amount_by_key(&self, key: BatchKey, unit: impl Unit) -> i128 {
        let unit = unit.unit_key();
        self.positions
            .and_then(|p| p.get(&(key, unit)))
            .copied()
            .unwrap_or(0)
    }

    /// Every nonzero position, in batch key order.
    pub fn positions(&self) -> impl Iterator<Item = (BatchKey, UnitKey, i128)> + '_ {
        self.positions
            .into_iter()
            .flat_map(|p| p.iter())
            .filter(|(_, a)| **a != 0)
            .map(|((key, unit), a)| (*key, *unit, *a))
    }

    /// Whether the object holds any nonzero position.
    pub fn is_clear(&self) -> bool {
        self.positions().next().is_none()
    }
}
