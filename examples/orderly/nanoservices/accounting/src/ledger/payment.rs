//! The accounting of `payment` (the Directive §11): which state
//! changes are allowed, the positions an object holds in each state, and
//! the balance checks a booking must pass. Three functions and no state of
//! their own. The snapshot and its states are `messages::PaymentSnapshot`
//! and `messages::PaymentState` (crates/messages/src/ledger.rs),
//! the producer's as much as this nanoservice's.
//!
//! A desired state is a pure function of the snapshot: it names batches
//! through `super::accounts::Account` and reads nothing else. In every
//! state the amounts in debit batches equal the amounts in credit batches
//! per ledger and unit (memo batches stand outside), which the engine
//! checks on every booking and `tests/ledger.rs` checks over a history.

use basable_ledger::{Accounting, Balances, Desired, Rejection};
use messages::{PaymentState as State, PaymentSnapshot as Snapshot};

/// The rules, version 1. Bump `VERSION` when a desired state or the
/// classification of a state changes: every process booked under the older
/// number is restated under the new code, once, when the deployment carrying
/// it starts (`src/ledger/mod.rs`). Adding or removing an allowed change
/// needs no bump.
pub struct PaymentAccounting;

impl Accounting<Snapshot> for PaymentAccounting {
    const VERSION: u32 = 1;

    /// From the booked state, the states the object may move to; `None` is
    /// an object not booked yet. A state listing itself allows an amount
    /// change while in it; a state listing nothing is terminal, its amounts
    /// frozen. As the manifest declares it (`initial` and `changes`).
    fn validate_state_change(current: Option<State>) -> &'static [State] {
        match current {
            None => &[State::Pending, State::Settled],
            Some(State::Pending) => &[State::Settled, State::Reversed],
            Some(State::Settled) => &[State::Reversed],
            Some(State::Reversed) => &[],
        }
    }

    /// The positions that must exist while the object is in `state`, each a
    /// batch, a unit and a signed amount in the column of the batch's type.
    /// `basable_ledger::{balance, desired}` build them:
    /// `desired![balance(Account::Receivable(o.id), o.currency, o.net + o.vat), ...]`.
    fn desired_state(state: State, o: &Snapshot) -> Desired {
        let _ = o;
        match state {
            State::Pending => {
                // TODO: the positions of a payment in `pending`.
                Desired::empty()
            }
            State::Settled => {
                // TODO: the positions of a payment in `settled`.
                Desired::empty()
            }
            State::Reversed => {
                // TODO: the positions of a payment in `reversed`.
                Desired::empty()
            }
        }
    }

    /// Checks the booked balances, this object's and the whole process's,
    /// against what the desired state asks for, before anything books. The
    /// default accepts; a check that reads the process (a payment may not
    /// exceed the open receivable) goes here and rejects with its reason.
    fn validate_balances(
        _state: State,
        _o: &Snapshot,
        _current: &Balances,
        _desired: &Desired,
    ) -> Result<(), Rejection> {
        Ok(())
    }
}
