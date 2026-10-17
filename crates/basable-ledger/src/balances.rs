//! The booked balances of a process, as `validate_balances` sees them: per
//! object, per logical batch id and unit, summed over the batch's epochs,
//! computed from the process's journals on every read. No position table
//! exists anywhere.

use std::collections::BTreeMap;

use crate::accounting::Unit;
use crate::types::{BatchId, BatchType, LedgerKey, ObjectId, UnitKey};

/// The positions of every object of a process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Balances {
    by_object: BTreeMap<ObjectId, BTreeMap<(BatchId, UnitKey), i128>>,
    batches: BTreeMap<BatchId, (LedgerKey, BatchType)>,
}

impl Balances {
    /// No positions at all.
    pub fn new() -> Self {
        Self::default()
    }

    /// One object's positions. An object the process has never booked has
    /// none.
    pub fn of(&self, object: ObjectId) -> ObjectBalances<'_> {
        ObjectBalances {
            positions: self.by_object.get(&object),
        }
    }

    /// The balance of one logical batch in one unit over every object of
    /// the process.
    pub fn batch(&self, id: &BatchId, unit: impl Unit) -> i128 {
        let unit = unit.unit_key();
        self.by_object
            .values()
            .filter_map(|p| p.get(&(id.clone(), unit)))
            .sum()
    }

    /// The ledger and type of a batch the process has lines in.
    pub fn batch_account(&self, id: &BatchId) -> Option<(LedgerKey, BatchType)> {
        self.batches.get(id).copied()
    }

    /// The objects with positions, in id order.
    pub fn objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.by_object.keys().copied()
    }

    pub(crate) fn add(
        &mut self,
        object: ObjectId,
        id: &BatchId,
        account: (LedgerKey, BatchType),
        unit: UnitKey,
        amount: i128,
    ) {
        self.batches.entry(id.clone()).or_insert(account);
        let slot = self
            .by_object
            .entry(object)
            .or_default()
            .entry((id.clone(), unit))
            .or_insert(0);
        *slot = slot.checked_add(amount).expect("position overflow");
    }

    pub(crate) fn positions_of(&self, object: ObjectId) -> BTreeMap<(BatchId, UnitKey), i128> {
        self.by_object
            .get(&object)
            .map(|p| {
                p.iter()
                    .filter(|(_, a)| **a != 0)
                    .map(|(k, a)| (k.clone(), *a))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One object's positions.
#[derive(Clone, Copy, Debug)]
pub struct ObjectBalances<'a> {
    positions: Option<&'a BTreeMap<(BatchId, UnitKey), i128>>,
}

impl ObjectBalances<'_> {
    /// The object's balance on one batch in one unit; 0 when none.
    pub fn amount(&self, id: &BatchId, unit: impl Unit) -> i128 {
        let unit = unit.unit_key();
        self.positions
            .and_then(|p| p.get(&(id.clone(), unit)))
            .copied()
            .unwrap_or(0)
    }

    /// Every nonzero position, in batch id order.
    pub fn positions(&self) -> impl Iterator<Item = (&BatchId, UnitKey, i128)> + '_ {
        self.positions
            .into_iter()
            .flat_map(|p| p.iter())
            .filter(|(_, a)| **a != 0)
            .map(|((id, unit), a)| (id, *unit, *a))
    }

    /// Whether the object holds any nonzero position.
    pub fn is_clear(&self) -> bool {
        self.positions().next().is_none()
    }
}
