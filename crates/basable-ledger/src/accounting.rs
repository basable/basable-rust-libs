//! What the user implements beside the reader: the accounts that name
//! batches, the units, and the `Accounting` of one object type.

use crate::balances::Balances;
use crate::desired::Desired;
use crate::error::Rejection;
use crate::object::LedgerObject;
use crate::types::{BatchKey, BatchType, LedgerKey, UnitKey};

/// Whatever names a batch implements this, and this is all the framework
/// knows about a batch: its key, which ledger it balances in and which
/// column of the book its amounts fall in. The framework keeps no batch
/// table. What the batch means (an account, an entity, a subledger, a GL
/// code, a period) and where its key comes from (a table of the user's own,
/// an encoding of the name, a hash of it) is the user's. A batch is closed
/// when a booking brings its balance to zero; nothing marks it.
pub trait BatchAccount {
    /// The key a line carries. One key is one batch, in one ledger, of one
    /// type, for good: naming a booked key under another ledger or type is
    /// a rejection.
    fn batch_key(&self) -> BatchKey;

    /// The balancing boundary: each ledger balances on its own.
    fn ledger(&self) -> LedgerKey;

    /// Debit for assets and expenses, credit for liabilities, equity and
    /// revenue, memo for what is not money.
    fn batch_type(&self) -> BatchType;
}

impl<A: BatchAccount + ?Sized> BatchAccount for &A {
    fn batch_key(&self) -> BatchKey {
        (**self).batch_key()
    }

    fn ledger(&self) -> LedgerKey {
        (**self).ledger()
    }

    fn batch_type(&self) -> BatchType {
        (**self).batch_type()
    }
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
    /// it reads the object and touches nothing else; the batches it names
    /// are the user's accounts, whose keys the object carries or the rules
    /// derive. In every state the amounts in debit batches must sum to the
    /// amounts in credit batches per ledger and unit.
    fn desired_state(state: O::State, object: &O) -> Desired;

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
