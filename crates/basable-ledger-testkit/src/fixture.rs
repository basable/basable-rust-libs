//! The fixture: an invoice and a payment, the two object types every suite
//! books. The invoice is the plan's worked example; the payment is a child
//! object in the same process that clears the invoice's receivable batch,
//! which is how a negative position on one object's side of a batch arises
//! and why the balance check sees the whole process.

use basable_ledger::{
    Accounting, Balances, BatchAccount, BatchId, BatchType, Batches, Desired, LedgerKey,
    LedgerObject, ObjectId, ProcessId, Rejection, Timestamp, Unit, UnitKey, amount_string, balance,
    desired,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The one ledger of the fixture.
pub const PLATFORM_LEDGER: LedgerKey = LedgerKey(1);

/// A currency, the fixture's unit type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Currency {
    /// Euro, key 978.
    Eur,
    /// US dollar, key 840.
    Usd,
}

impl Unit for Currency {
    fn unit_key(&self) -> UnitKey {
        match self {
            Currency::Eur => UnitKey(978),
            Currency::Usd => UnitKey(840),
        }
    }
}

/// The fixture's accounts: one variant per kind, with its entity where it
/// has one. The framework sees only the ledger and the type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Account {
    /// What a customer owes.
    Receivable(Uuid),
    /// Cash received, until the payout clears it.
    CashClearing,
    /// Revenue.
    Revenue,
    /// VAT owed to the tax office.
    VatPayable,
    /// Written-off receivables.
    BadDebt,
    /// Loyalty points: not money.
    Points,
}

impl BatchAccount for Account {
    fn ledger(&self) -> LedgerKey {
        PLATFORM_LEDGER
    }

    fn batch_type(&self) -> BatchType {
        match self {
            Account::Receivable(_) | Account::CashClearing | Account::BadDebt => BatchType::Debit,
            Account::Revenue | Account::VatPayable => BatchType::Credit,
            Account::Points => BatchType::Memo,
        }
    }
}

/// The invoice's lifecycle as the producer reports it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum InvoiceState {
    /// Not yet sent: nothing is owed.
    Draft,
    /// Sent: a receivable, revenue and VAT exist.
    Issued,
    /// Partly paid: the receivable shrank, cash clearing grew.
    PartiallyPaid,
    /// Paid in full: no receivable left.
    Paid,
    /// Cancelled by a credit note: nothing is left.
    Credited,
    /// Given up on: the receivable became bad debt.
    WrittenOff,
}

/// The invoice, the user's own type with whatever fields it needs.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Invoice {
    /// The process: the invoice is the root object, so its own id.
    pub process_id: ProcessId,
    /// The invoice's id.
    pub id: ObjectId,
    /// The producer's version.
    pub version: u64,
    /// When the producer last changed it.
    pub changed_at: Timestamp,
    /// Its state.
    pub state: InvoiceState,
    /// When the state was entered.
    pub state_since: Timestamp,
    /// The customer.
    pub customer: Uuid,
    /// The currency.
    pub currency: Currency,
    /// Net amount, in minor units.
    #[serde(with = "amount_string")]
    pub net: i128,
    /// VAT amount.
    #[serde(with = "amount_string")]
    pub vat: i128,
    /// Paid so far.
    #[serde(with = "amount_string")]
    pub paid: i128,
    /// The month the invoice was issued in, `YYYY-MM`.
    pub issued_month: String,
    /// Loyalty points granted with the invoice; not money.
    pub points: i128,
}

impl LedgerObject for Invoice {
    const TYPE: &'static str = "invoice";
    type State = InvoiceState;

    fn process_id(&self) -> ProcessId {
        self.process_id
    }

    fn object_id(&self) -> ObjectId {
        self.id
    }

    fn version(&self) -> u64 {
        self.version
    }

    fn changed_at(&self) -> Timestamp {
        self.changed_at
    }

    fn state(&self) -> (InvoiceState, Timestamp) {
        (self.state, self.state_since)
    }
}

/// The invoice's batch ids: one per account and invoice, or per account
/// and month for the shared ones.
impl Invoice {
    /// The receivable batch of this invoice.
    pub fn receivable_batch(&self) -> BatchId {
        ("receivable", self.id.0).into()
    }

    /// The cash-clearing batch of this invoice.
    pub fn cash_batch(&self) -> BatchId {
        ("cash", self.id.0).into()
    }

    /// The month's revenue batch.
    pub fn revenue_batch(&self) -> BatchId {
        ("revenue", &self.issued_month).into()
    }

    /// The month's VAT batch (version 1 of the rules).
    pub fn vat_batch(&self) -> BatchId {
        ("vat", &self.issued_month).into()
    }

    /// The month's bad-debt batch.
    pub fn bad_debt_batch(&self) -> BatchId {
        ("bad_debt", &self.issued_month).into()
    }

    /// The month's loyalty points batch.
    pub fn points_batch(&self) -> BatchId {
        ("points", &self.issued_month).into()
    }
}

/// The invoice's accounting, version 1.
pub struct InvoiceLedger;

impl InvoiceLedger {
    /// The allowed changes, shared by both versions of the rules.
    pub fn changes(current: Option<InvoiceState>) -> &'static [InvoiceState] {
        use InvoiceState::*;
        match current {
            None => &[Draft, Issued],
            Some(Draft) => &[Draft, Issued, Credited],
            Some(Issued) => &[Issued, PartiallyPaid, Paid, Credited, WrittenOff],
            Some(PartiallyPaid) => &[PartiallyPaid, Paid, WrittenOff],
            Some(Paid) | Some(Credited) | Some(WrittenOff) => &[],
        }
    }

    /// The positions of an invoice in a state, with the VAT batch chosen by
    /// `vat_batch`: version 1 groups VAT per month, version 2 per quarter.
    pub fn positions(
        state: InvoiceState,
        o: &Invoice,
        b: &mut Batches,
        vat_batch: BatchId,
    ) -> Desired {
        use InvoiceState::*;
        let receivable = b.open(Account::Receivable(o.customer), o.receivable_batch());
        let cash = b.open(Account::CashClearing, o.cash_batch());
        let revenue = b.open(Account::Revenue, o.revenue_batch());
        let vat = b.open(Account::VatPayable, vat_batch);
        let bad_debt = b.open(Account::BadDebt, o.bad_debt_batch());
        let points = b.open(Account::Points, o.points_batch());
        let cur = o.currency;
        match state {
            Draft | Credited => Desired::empty(),
            Issued | PartiallyPaid => desired![
                balance(receivable, cur, o.net + o.vat - o.paid),
                balance(cash, cur, o.paid),
                balance(revenue, cur, o.net),
                balance(vat, cur, o.vat),
                balance(points, UnitKey(1), o.points),
            ],
            Paid => desired![
                balance(cash, cur, o.net + o.vat),
                balance(revenue, cur, o.net),
                balance(vat, cur, o.vat),
                balance(points, UnitKey(1), o.points),
            ],
            WrittenOff => desired![
                balance(bad_debt, cur, o.net + o.vat),
                balance(revenue, cur, o.net),
                balance(vat, cur, o.vat),
            ],
        }
    }
}

impl Accounting<Invoice> for InvoiceLedger {
    const VERSION: u32 = 1;

    fn validate_state_change(current: Option<InvoiceState>) -> &'static [InvoiceState] {
        Self::changes(current)
    }

    fn desired_state(state: InvoiceState, o: &Invoice, b: &mut Batches) -> Desired {
        Self::positions(state, o, b, o.vat_batch())
    }

    fn validate_balances(
        state: InvoiceState,
        o: &Invoice,
        _current: &Balances,
        _desired: &Desired,
    ) -> Result<(), Rejection> {
        match state {
            InvoiceState::Paid if o.paid != o.net + o.vat => {
                Err(Rejection::new("paid without full payment"))
            }
            InvoiceState::PartiallyPaid if o.paid <= 0 || o.paid >= o.net + o.vat => {
                Err(Rejection::new("partially paid means some but not all"))
            }
            _ => Ok(()),
        }
    }
}

/// The invoice's accounting, version 2: VAT grouped per quarter instead of
/// per month. Registering this in place of version 1 restates every
/// process booked under version 1.
pub struct InvoiceLedgerV2;

impl InvoiceLedgerV2 {
    /// The quarter's VAT batch, `vat/YYYY-Qn`.
    pub fn vat_batch(o: &Invoice) -> BatchId {
        let month: u32 = o.issued_month[5..7].parse().unwrap_or(1);
        let year = &o.issued_month[..4];
        ("vat", format!("{year}-Q{}", month.div_ceil(3))).into()
    }
}

impl Accounting<Invoice> for InvoiceLedgerV2 {
    const VERSION: u32 = 2;

    fn validate_state_change(current: Option<InvoiceState>) -> &'static [InvoiceState] {
        InvoiceLedger::changes(current)
    }

    fn desired_state(state: InvoiceState, o: &Invoice, b: &mut Batches) -> Desired {
        InvoiceLedger::positions(state, o, b, Self::vat_batch(o))
    }
}

/// A payment's lifecycle.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum PaymentState {
    /// Announced, no money yet.
    Pending,
    /// Money arrived in cash clearing, clearing the invoice's receivable.
    Settled,
    /// Reversed: nothing is left.
    Reversed,
}

/// A payment against an invoice: a child object in the invoice's process.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Payment {
    /// The invoice's process.
    pub process_id: ProcessId,
    /// The payment's id.
    pub id: ObjectId,
    /// The invoice.
    pub invoice: ObjectId,
    /// The producer's version.
    pub version: u64,
    /// When the producer last changed it.
    pub changed_at: Timestamp,
    /// Its state.
    pub state: PaymentState,
    /// When the state was entered.
    pub state_since: Timestamp,
    /// The currency.
    pub currency: Currency,
    /// The amount.
    #[serde(with = "amount_string")]
    pub amount: i128,
}

impl LedgerObject for Payment {
    const TYPE: &'static str = "payment";
    type State = PaymentState;

    fn process_id(&self) -> ProcessId {
        self.process_id
    }

    fn object_id(&self) -> ObjectId {
        self.id
    }

    fn parent_id(&self) -> Option<ObjectId> {
        Some(self.invoice)
    }

    fn version(&self) -> u64 {
        self.version
    }

    fn changed_at(&self) -> Timestamp {
        self.changed_at
    }

    fn state(&self) -> (PaymentState, Timestamp) {
        (self.state, self.state_since)
    }
}

/// The payment's accounting: settled money sits in the invoice's cash batch
/// and clears the invoice's receivable batch, so the payment holds a
/// negative position on a debit batch and the batch nets to zero when the
/// invoice is paid in full.
pub struct PaymentLedger;

impl Accounting<Payment> for PaymentLedger {
    const VERSION: u32 = 1;

    fn validate_state_change(current: Option<PaymentState>) -> &'static [PaymentState] {
        use PaymentState::*;
        match current {
            None => &[Pending, Settled],
            Some(Pending) => &[Pending, Settled, Reversed],
            Some(Settled) => &[Reversed],
            Some(Reversed) => &[],
        }
    }

    fn desired_state(state: PaymentState, p: &Payment, b: &mut Batches) -> Desired {
        match state {
            PaymentState::Pending | PaymentState::Reversed => Desired::empty(),
            PaymentState::Settled => {
                let receivable = b.existing(
                    Account::Receivable(Uuid::nil()),
                    ("receivable", p.invoice.0),
                );
                let cash = b.open(Account::CashClearing, ("cash", p.invoice.0));
                desired![
                    balance(cash, p.currency, p.amount),
                    balance(receivable, p.currency, -p.amount),
                ]
            }
        }
    }

    fn validate_balances(
        state: PaymentState,
        p: &Payment,
        current: &Balances,
        _desired: &Desired,
    ) -> Result<(), Rejection> {
        if state != PaymentState::Settled {
            return Ok(());
        }
        let open = current.batch(&("receivable", p.invoice.0).into(), p.currency);
        if p.amount > open {
            return Err(Rejection::new(format!(
                "payment of {} exceeds the open receivable of {open}",
                p.amount
            )));
        }
        Ok(())
    }
}

/// A fresh invoice in `Draft`, version 1, for a new process.
pub fn new_invoice(net: i128, vat: i128, currency: Currency) -> Invoice {
    let id = Uuid::now_v7();
    let now = chrono::Utc::now();
    Invoice {
        process_id: ProcessId(id),
        id: ObjectId(id),
        version: 1,
        changed_at: now,
        state: InvoiceState::Draft,
        state_since: now,
        customer: Uuid::new_v4(),
        currency,
        net,
        vat,
        paid: 0,
        issued_month: "2026-10".to_owned(),
        points: net / 100,
    }
}

impl Invoice {
    /// The next snapshot: version plus one, state and fields changed.
    pub fn next(&self, state: InvoiceState, paid: i128) -> Invoice {
        let now = chrono::Utc::now();
        Invoice {
            version: self.version + 1,
            changed_at: now,
            state,
            state_since: if state == self.state {
                self.state_since
            } else {
                now
            },
            paid,
            ..self.clone()
        }
    }
}

/// A pending payment against an invoice.
pub fn new_payment(invoice: &Invoice, amount: i128) -> Payment {
    let now = chrono::Utc::now();
    Payment {
        process_id: invoice.process_id,
        id: ObjectId(Uuid::now_v7()),
        invoice: invoice.id,
        version: 1,
        changed_at: now,
        state: PaymentState::Pending,
        state_since: now,
        currency: invoice.currency,
        amount,
    }
}

impl Payment {
    /// The next snapshot.
    pub fn next(&self, state: PaymentState) -> Payment {
        let now = chrono::Utc::now();
        Payment {
            version: self.version + 1,
            changed_at: now,
            state,
            state_since: now,
            ..self.clone()
        }
    }
}
