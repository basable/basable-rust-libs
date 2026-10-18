//! What names a batch: the accounts of this ledger. The framework knows of a
//! batch only its key, its ledger and its type (debit, credit or memo); what
//! a batch means, and whatever else you keep about it, is yours, in a table
//! of your own keyed by the batch key or encoded in the key. A batch is
//! closed when a booking brings its balance to zero (a VAT return filed for
//! what the month's batch held, a payout for a clearing batch); nothing
//! marks it, and a line landing afterwards is the next claim's remainder.

use basable_ledger::{BatchAccount, BatchKey, BatchType, LedgerKey};

/// The one ledger of this application. A second one (an accrual ledger
/// beside the money ledger) is another constant and another `ledger()` arm;
/// each ledger balances on its own.
pub const LEDGER: LedgerKey = LedgerKey(1);

/// The kinds of batch the desired states name, each with what identifies
/// one batch of the kind: an object (a receivable per order), a period
/// (revenue per month), a counterparty (a payout per merchant and week).
///
/// TODO: one variant per kind, for example `Receivable(basable_ledger::ObjectId)`,
/// `CashClearing(basable_ledger::ObjectId)`, `Revenue(String)` (a month),
/// `VatPayable(String)` (a quarter), `Points(String)` (memo: not money).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Account {}

impl Account {
    /// The batch's name, `<kind>/<what identifies it>`; the key is derived
    /// from it below.
    pub fn name(&self) -> String {
        match *self {}
    }
}

impl BatchAccount for Account {
    /// The key a line carries: here an FNV-1a hash of the name, which makes
    /// the same name the same batch everywhere without a table. Allocate it
    /// from a table of your own instead when a batch needs more than its
    /// name (a settle time, a GL code), and keep that table keyed by it.
    fn batch_key(&self) -> BatchKey {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.name().as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        BatchKey(h as i64)
    }

    fn ledger(&self) -> LedgerKey {
        LEDGER
    }

    /// Debit for assets and expenses (a receivable, cash, bad debt), credit
    /// for liabilities, equity and revenue (revenue, VAT owed, a payable),
    /// memo for what is not money and stands outside the balance check.
    fn batch_type(&self) -> BatchType {
        match *self {}
    }
}
