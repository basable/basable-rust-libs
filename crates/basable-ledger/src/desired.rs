//! A desired state: the positions that must exist while an object is in a
//! state, each a batch, a unit and a signed amount in the column of the
//! batch's type.

use crate::accounting::{BatchAccount, Unit};
use crate::types::{Batch, UnitKey};

/// One desired position.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Position {
    /// The batch: the account's key, ledger and type.
    pub batch: Batch,
    /// The unit.
    pub unit: UnitKey,
    /// Signed: positive grows the batch's balance in its own column, negative
    /// is a balance below zero there, which the book allows.
    pub amount: i128,
}

/// The positions an object holds in a state. Duplicates of one (batch,
/// unit) are summed when the engine compares them with what is booked.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Desired {
    positions: Vec<Position>,
}

impl Desired {
    /// No positions: the state holds nothing (a draft, a closed object).
    pub fn empty() -> Self {
        Self::default()
    }

    /// From a list of positions; the `desired!` macro builds one.
    pub fn new(positions: Vec<Position>) -> Self {
        Desired { positions }
    }

    /// Adds a position.
    pub fn push(&mut self, position: Position) {
        self.positions.push(position);
    }

    /// The positions as written.
    pub fn positions(&self) -> &[Position] {
        &self.positions
    }
}

/// A desired balance on a batch: `balance(account, unit, amount)`.
pub fn balance(account: impl BatchAccount, unit: impl Unit, amount: i128) -> Position {
    Position {
        batch: Batch::of(&account),
        unit: unit.unit_key(),
        amount,
    }
}

/// `desired![balance(a, eur, 100), balance(b, eur, 100)]`.
#[macro_export]
macro_rules! desired {
    ($($p:expr),* $(,)?) => {
        $crate::Desired::new(vec![$($p),*])
    };
}
