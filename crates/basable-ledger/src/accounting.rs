//! What the user implements beside the reader: the accounts that name
//! batches, the units, and the `Accounting` of one object type.

use crate::balances::Balances;
use crate::batches::Batches;
use crate::desired::Desired;
use crate::error::Rejection;
use crate::object::LedgerObject;
use crate::types::{BatchType, LedgerKey, UnitKey};

/// Whatever names a batch implements this, and this is all the framework
/// knows about a batch: which ledger it balances in and which column of the
/// book its amounts fall in. What the batch means (an account, an entity, a
/// subledger, a GL code, a settle time) is the user's.
pub trait BatchAccount {
    /// The balancing boundary: each ledger balances on its own.
    fn ledger(&self) -> LedgerKey;

    /// Debit for assets and expenses, credit for liabilities, equity and
    /// revenue, memo for what is not money.
    fn batch_type(&self) -> BatchType;
}

/// A unit of account: the framework needs only a key to group and balance
/// by. The code, its decimals and its display are the user's.
pub trait Unit {
    /// The key a line carries.
    fn unit_key(&self) -> UnitKey;
}

impl Unit for UnitKey {
    fn unit_key(&self) -> UnitKey {
        *self
    }
}

impl Unit for u32 {
    fn unit_key(&self) -> UnitKey {
        UnitKey(*self)
    }
}

/// The accounting of one object type: its allowed state changes, the
/// positions that exist while an object is in each state, and the balance
/// checks a booking must pass. Three associated functions, no `self`: the
/// rules have no state of their own.
pub trait Accounting<O: LedgerObject>: Send + Sync + 'static {
    /// The hardcoded version of these rules. Bump it when a desired state or
    /// the classification changes; every process whose recorded version is
    /// older is then restated under the new code. Adding or removing allowed
    /// changes alters what future snapshots may do, not what is booked, and
    /// needs no bump.
    const VERSION: u32;

    /// From the booked state, which states the object may move to. `None`
    /// is an object not booked yet. Listing the current state itself allows
    /// an amount change while in it; leaving it out makes that state
    /// amount-immutable. A terminal state lists nothing.
    fn validate_state_change(current: Option<O::State>) -> &'static [O::State];

    /// The positions that must exist while the object is in `state`. Pure:
    /// it reads the object, names batches through `batches`, and touches
    /// nothing else. In every state the amounts in debit batches must sum to
    /// the amounts in credit batches per ledger and unit.
    fn desired_state(state: O::State, object: &O, batches: &mut Batches) -> Desired;

    /// Checks the booked balances, this object's and the whole process's,
    /// against what the desired state asks for, before anything books. An
    /// `Err` rejects the process's part of the call. The default accepts.
    fn validate_balances(
        state: O::State,
        object: &O,
        current: &Balances,
        desired: &Desired,
    ) -> Result<(), Rejection> {
        let _ = (state, object, current, desired);
        Ok(())
    }
}
