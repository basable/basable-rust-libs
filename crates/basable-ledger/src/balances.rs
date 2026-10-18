//! The booked balances of a process, as `validate_balances` sees them: per
//! object, per batch key and unit, computed from the process's journals on
//! every read. No position table exists anywhere.

use std::collections::BTreeMap;

use crate::accounting::Unit;
use crate::types::{Batch, BatchKey, BatchType, LedgerKey, ObjectId, UnitKey};

/// The positions of every object of a process, and the ledger and type of
/// every batch the process has lines in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Balances {
    by_object: BTreeMap<ObjectId, BTreeMap<(BatchKey, UnitKey), i128>>,
    batches: BTreeMap<BatchKey, (LedgerKey, BatchType)>,
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

    /// The balance of one batch in one unit over every object of the
    /// process. Takes a key or whatever names the batch.
    pub fn batch(&self, batch: impl Into<BatchKey>, unit: impl Unit) -> i128 {
        let key = batch.into();
        let unit = unit.unit_key();
        self.by_object
            .values()
            .filter_map(|p| p.get(&(key, unit)))
            .sum()
    }

    /// The ledger and type of a batch the process has lines in.
    pub fn batch_account(&self, batch: impl Into<BatchKey>) -> Option<(LedgerKey, BatchType)> {
        self.batches.get(&batch.into()).copied()
    }

    /// The objects with positions, in id order.
    pub fn objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.by_object.keys().copied()
    }

    /// Notes a batch's ledger and type; a key already noted under another
    /// ledger or type fails with what it was noted as.
    pub(crate) fn note(&mut self, batch: Batch) -> Result<(), (LedgerKey, BatchType)> {
        let account = (batch.ledger, batch.batch_type);
        let noted = *self.batches.entry(batch.key).or_insert(account);
        if noted == account { Ok(()) } else { Err(noted) }
    }

    /// Adds a line's amount to its object's position, noting the batch.
    pub(crate) fn add(
        &mut self,
        object: ObjectId,
        batch: Batch,
        unit: UnitKey,
        amount: i128,
    ) -> Result<(), (LedgerKey, BatchType)> {
        self.note(batch)?;
        let slot = self
            .by_object
            .entry(object)
            .or_default()
            .entry((batch.key, unit))
            .or_insert(0);
        *slot = slot.checked_add(amount).expect("position overflow");
        Ok(())
    }

    /// One object's nonzero positions, each with its batch.
    pub(crate) fn positions_of(
        &self,
        object: ObjectId,
    ) -> BTreeMap<(BatchKey, UnitKey), (Batch, i128)> {
        self.by_object
            .get(&object)
            .map(|p| {
                p.iter()
                    .filter(|(_, a)| **a != 0)
                    .map(|((key, unit), a)| {
                        let (ledger, batch_type) = self.batches[key];
                        let batch = Batch {
                            key: *key,
                            ledger,
                            batch_type,
                        };
                        ((*key, *unit), (batch, *a))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One object's positions.
#[derive(Clone, Copy, Debug)]
pub struct ObjectBalances<'a> {
    positions: Option<&'a BTreeMap<(BatchKey, UnitKey), i128>>,
}

impl ObjectBalances<'_> {
    /// The object's balance on one batch in one unit; 0 when none. Takes a
    /// key or whatever names the batch.
    pub fn amount(&self, batch: impl Into<BatchKey>, unit: impl Unit) -> i128 {
        let key = batch.into();
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
