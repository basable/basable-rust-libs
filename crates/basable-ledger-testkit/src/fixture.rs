//! The fixture: an invoice, a payment, a VAT return and a transfer, the
//! object types every suite books. The invoice is the plan's worked example;
//! the payment is a child object in the same process that clears the
//! invoice's receivable batch, which is how a negative position on one
//! object's side of a batch arises and why the balance check sees the whole
//! process; the VAT return is a root object of its own that books a period's
//! VAT batch to zero, which is how a batch closes; the transfer names
//! whichever two batches a test gives it.

use basable_ledger::{
    Accounting, Balances, BatchType, Desired, LedgerBatch, LedgerKey, LedgerObject, ObjectId,
    ProcessId, Rejection, Unit, UnitKey, amount_string, balance, desired,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The one ledger of the fixture, apart from [`Batch::InBook`].
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

/// The fixture's batches: one value is one batch, its variant the kind and
/// its fields which one. The framework sees only the primary key, the ledger
/// and the type. A test's store keeps the rows: the in-memory store by the
/// primary key, Postgres in its own test tables.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Batch {
    /// What a customer owes on one invoice.
    Receivable(ObjectId),
    /// Cash received against one invoice, until the payout clears it.
    CashClearing(ObjectId),
    /// A month's revenue.
    Revenue(String),
    /// VAT owed for a period: a month under version 1 of the invoice rules,
    /// a quarter under version 2.
    Vat(String),
    /// What a filed VAT return owes the tax office for a period.
    TaxOffice(String),
    /// A month's written-off receivables.
    BadDebt(String),
    /// A month's loyalty points: not money.
    Points(String),
    /// What a merchant is owed. The settle day is another column of the row,
    /// not part of what identifies the batch: two values that differ only in
    /// it are one batch, and the first one's day is the row's.
    Settlement {
        /// What identifies the batch.
        merchant: String,
        /// The other column.
        settle_day: String,
    },
    /// A batch whose ledger and type the test chooses, identified by its name
    /// alone: naming it under another ledger or type disagrees with its row.
    Named {
        /// What identifies the batch.
        name: String,
        /// The ledger this value says.
        ledger: LedgerKey,
        /// The type this value says.
        batch_type: BatchType,
    },
    /// A batch with the ledger in its primary key: the same name in two books
    /// is two batches with two keys.
    InBook {
        /// Part of what identifies the batch.
        name: String,
        /// Part of what identifies the batch.
        ledger: LedgerKey,
        /// The type.
        batch_type: BatchType,
    },
}

impl Batch {
    /// The batch's name, `<kind>/<what identifies it>`: its primary key.
    pub fn name(&self) -> String {
        match self {
            Batch::Receivable(id) => format!("receivable/{}", id.0),
            Batch::CashClearing(id) => format!("cash/{}", id.0),
            Batch::Revenue(p) => format!("revenue/{p}"),
            Batch::Vat(p) => format!("vat/{p}"),
            Batch::TaxOffice(p) => format!("tax_office/{p}"),
            Batch::BadDebt(p) => format!("bad_debt/{p}"),
            Batch::Points(p) => format!("points/{p}"),
            Batch::Settlement { merchant, .. } => format!("settlement/{merchant}"),
            Batch::Named { name, .. } => format!("named/{name}"),
            Batch::InBook { name, ledger, .. } => format!("in_book/{}/{name}", ledger.0),
        }
    }
}

impl LedgerBatch for Batch {
    type PrimaryKey = String;

    fn primary_key(&self) -> String {
        self.name()
    }

    fn ledger(&self) -> LedgerKey {
        match self {
            Batch::Named { ledger, .. } | Batch::InBook { ledger, .. } => *ledger,
            _ => PLATFORM_LEDGER,
        }
    }

    fn batch_type(&self) -> BatchType {
        match self {
            Batch::Receivable(_)
            | Batch::CashClearing(_)
            | Batch::BadDebt(_)
            | Batch::Settlement { .. } => BatchType::Debit,
            Batch::Revenue(_) | Batch::Vat(_) | Batch::TaxOffice(_) => BatchType::Credit,
            Batch::Points(_) => BatchType::Memo,
            Batch::Named { batch_type, .. } | Batch::InBook { batch_type, .. } => *batch_type,
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
    /// Its state.
    pub state: InvoiceState,
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

    fn state(&self) -> InvoiceState {
        self.state
    }
}

/// The invoice's batches: one per account and invoice, or per account and
/// month for the shared ones.
impl Invoice {
    /// The receivable batch of this invoice.
    pub fn receivable_batch(&self) -> Batch {
        Batch::Receivable(self.id)
    }

    /// The cash-clearing batch of this invoice.
    pub fn cash_batch(&self) -> Batch {
        Batch::CashClearing(self.id)
    }

    /// The month's revenue batch.
    pub fn revenue_batch(&self) -> Batch {
        Batch::Revenue(self.issued_month.clone())
    }

    /// The month's VAT batch (version 1 of the rules).
    pub fn vat_batch(&self) -> Batch {
        Batch::Vat(self.issued_month.clone())
    }

    /// The month's bad-debt batch.
    pub fn bad_debt_batch(&self) -> Batch {
        Batch::BadDebt(self.issued_month.clone())
    }

    /// The month's loyalty points batch.
    pub fn points_batch(&self) -> Batch {
        Batch::Points(self.issued_month.clone())
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
    /// the caller: version 1 groups VAT per month, version 2 per quarter.
    pub fn positions(state: InvoiceState, o: &Invoice, vat: Batch) -> Desired<Batch> {
        use InvoiceState::*;
        let cur = o.currency;
        match state {
            Draft | Credited => Desired::empty(),
            Issued | PartiallyPaid => desired![
                balance(o.receivable_batch(), cur, o.net + o.vat - o.paid),
                balance(o.cash_batch(), cur, o.paid),
                balance(o.revenue_batch(), cur, o.net),
                balance(vat, cur, o.vat),
                balance(o.points_batch(), UnitKey(1), o.points),
            ],
            Paid => desired![
                balance(o.cash_batch(), cur, o.net + o.vat),
                balance(o.revenue_batch(), cur, o.net),
                balance(vat, cur, o.vat),
                balance(o.points_batch(), UnitKey(1), o.points),
            ],
            WrittenOff => desired![
                balance(o.bad_debt_batch(), cur, o.net + o.vat),
                balance(o.revenue_batch(), cur, o.net),
                balance(vat, cur, o.vat),
            ],
        }
    }
}

impl Accounting<Invoice> for InvoiceLedger {
    type Batch = Batch;
    const VERSION: u32 = 1;

    fn validate_state_change(current: Option<InvoiceState>) -> &'static [InvoiceState] {
        Self::changes(current)
    }

    fn desired_state(state: InvoiceState, o: &Invoice) -> Desired<Batch> {
        Self::positions(state, o, o.vat_batch())
    }

    fn validate_balances(
        state: InvoiceState,
        o: &Invoice,
        _current: &Balances<Batch>,
        _desired: &Desired<Batch>,
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
    pub fn vat_batch(o: &Invoice) -> Batch {
        let month: u32 = o.issued_month[5..7].parse().unwrap_or(1);
        let year = &o.issued_month[..4];
        Batch::Vat(format!("{year}-Q{}", month.div_ceil(3)))
    }
}

impl Accounting<Invoice> for InvoiceLedgerV2 {
    type Batch = Batch;
    const VERSION: u32 = 2;

    fn validate_state_change(current: Option<InvoiceState>) -> &'static [InvoiceState] {
        InvoiceLedger::changes(current)
    }

    fn desired_state(state: InvoiceState, o: &Invoice) -> Desired<Batch> {
        InvoiceLedger::positions(state, o, Self::vat_batch(o))
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
    /// Its state.
    pub state: PaymentState,
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

    fn state(&self) -> PaymentState {
        self.state
    }
}

/// The payment's accounting: settled money sits in the invoice's cash batch
/// and clears the invoice's receivable batch, so the payment holds a
/// negative position on a debit batch and the batch nets to zero when the
/// invoice is paid in full.
pub struct PaymentLedger;

impl Accounting<Payment> for PaymentLedger {
    type Batch = Batch;
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

    fn desired_state(state: PaymentState, p: &Payment) -> Desired<Batch> {
        match state {
            PaymentState::Pending | PaymentState::Reversed => Desired::empty(),
            PaymentState::Settled => desired![
                balance(Batch::CashClearing(p.invoice), p.currency, p.amount),
                balance(Batch::Receivable(p.invoice), p.currency, -p.amount),
            ],
        }
    }

    fn validate_balances(
        state: PaymentState,
        p: &Payment,
        current: &Balances<Batch>,
        _desired: &Desired<Batch>,
    ) -> Result<(), Rejection> {
        if state != PaymentState::Settled {
            return Ok(());
        }
        let open = current.batch(&Batch::Receivable(p.invoice), p.currency);
        if p.amount > open {
            return Err(Rejection::new(format!(
                "payment of {} exceeds the open receivable of {open}",
                p.amount
            )));
        }
        Ok(())
    }
}

/// A VAT return's lifecycle.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum VatReturnState {
    /// Being prepared: nothing is booked.
    Draft,
    /// Filed: the period's VAT is owed to the tax office.
    Filed,
}

/// The VAT return for a period: the user's claim on the period's VAT batch,
/// a root object in a process of its own. The producer read the batch's sum
/// and put it on the return; filing it books that amount off the VAT batch
/// and onto what the tax office is owed. That is how a batch closes: by a
/// booking that brings it to zero, not by a status. A correction that lands
/// in the period afterwards leaves a remainder, which the next return takes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct VatReturn {
    /// The process: its own id.
    pub process_id: ProcessId,
    /// The return's id.
    pub id: ObjectId,
    /// The producer's version.
    pub version: u64,
    /// Its state.
    pub state: VatReturnState,
    /// The period the return covers, as the VAT batch names it.
    pub period: String,
    /// The currency.
    pub currency: Currency,
    /// What the return claims: the batch's sum as the producer read it.
    #[serde(with = "amount_string")]
    pub amount: i128,
}

impl LedgerObject for VatReturn {
    const TYPE: &'static str = "vat_return";
    type State = VatReturnState;

    fn process_id(&self) -> ProcessId {
        self.process_id
    }

    fn object_id(&self) -> ObjectId {
        self.id
    }

    fn version(&self) -> u64 {
        self.version
    }

    fn state(&self) -> VatReturnState {
        self.state
    }
}

/// The VAT return's accounting: filed, it holds minus the claimed amount on
/// the period's VAT batch and the same amount on the tax office's.
pub struct VatReturnLedger;

impl Accounting<VatReturn> for VatReturnLedger {
    type Batch = Batch;
    const VERSION: u32 = 1;

    fn validate_state_change(current: Option<VatReturnState>) -> &'static [VatReturnState] {
        use VatReturnState::*;
        match current {
            None => &[Draft, Filed],
            Some(Draft) => &[Draft, Filed],
            Some(Filed) => &[],
        }
    }

    fn desired_state(state: VatReturnState, r: &VatReturn) -> Desired<Batch> {
        match state {
            VatReturnState::Draft => Desired::empty(),
            VatReturnState::Filed => desired![
                balance(Batch::Vat(r.period.clone()), r.currency, -r.amount),
                balance(Batch::TaxOffice(r.period.clone()), r.currency, r.amount),
            ],
        }
    }
}

/// A fresh invoice in `Draft`, version 1, for a new process.
pub fn new_invoice(net: i128, vat: i128, currency: Currency) -> Invoice {
    let id = Uuid::now_v7();
    Invoice {
        process_id: ProcessId(id),
        id: ObjectId(id),
        version: 1,
        state: InvoiceState::Draft,
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
        Invoice {
            version: self.version + 1,
            state,
            paid,
            ..self.clone()
        }
    }
}

/// A pending payment against an invoice.
pub fn new_payment(invoice: &Invoice, amount: i128) -> Payment {
    Payment {
        process_id: invoice.process_id,
        id: ObjectId(Uuid::now_v7()),
        invoice: invoice.id,
        version: 1,
        state: PaymentState::Pending,
        currency: invoice.currency,
        amount,
    }
}

impl Payment {
    /// The next snapshot.
    pub fn next(&self, state: PaymentState) -> Payment {
        Payment {
            version: self.version + 1,
            state,
            ..self.clone()
        }
    }
}

/// A filed VAT return for a period, claiming `amount`: what the producer
/// read as the period's VAT batch sum.
pub fn new_vat_return(period: &str, amount: i128, currency: Currency) -> VatReturn {
    let id = Uuid::now_v7();
    VatReturn {
        process_id: ProcessId(id),
        id: ObjectId(id),
        version: 1,
        state: VatReturnState::Filed,
        period: period.to_owned(),
        currency,
        amount,
    }
}

/// A transfer's lifecycle.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum TransferState {
    /// Not posted: nothing is booked.
    Draft,
    /// Posted: the amount is on both batches.
    Posted,
}

/// A transfer: a root object naming whichever two batches a test gives it,
/// which is how the suites exercise a batch's row.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Transfer {
    /// The process: its own id.
    pub process_id: ProcessId,
    /// The transfer's id.
    pub id: ObjectId,
    /// The producer's version.
    pub version: u64,
    /// Its state.
    pub state: TransferState,
    /// The first batch.
    pub left: Batch,
    /// The second batch.
    pub right: Batch,
    /// The amount, on both.
    #[serde(with = "amount_string")]
    pub amount: i128,
}

impl LedgerObject for Transfer {
    const TYPE: &'static str = "transfer";
    type State = TransferState;

    fn process_id(&self) -> ProcessId {
        self.process_id
    }

    fn object_id(&self) -> ObjectId {
        self.id
    }

    fn version(&self) -> u64 {
        self.version
    }

    fn state(&self) -> TransferState {
        self.state
    }
}

/// The transfer's accounting: posted, it holds the amount on both batches.
pub struct TransferLedger;

impl Accounting<Transfer> for TransferLedger {
    type Batch = Batch;
    const VERSION: u32 = 1;

    fn validate_state_change(current: Option<TransferState>) -> &'static [TransferState] {
        use TransferState::*;
        match current {
            None | Some(Draft) => &[Draft, Posted],
            Some(Posted) => &[],
        }
    }

    fn desired_state(state: TransferState, t: &Transfer) -> Desired<Batch> {
        match state {
            TransferState::Draft => Desired::empty(),
            TransferState::Posted => desired![
                balance(t.left.clone(), Currency::Eur, t.amount),
                balance(t.right.clone(), Currency::Eur, t.amount),
            ],
        }
    }
}

/// A posted transfer of `amount` between two batches, in a new process.
pub fn new_transfer(left: Batch, right: Batch, amount: i128) -> Transfer {
    let id = Uuid::now_v7();
    Transfer {
        process_id: ProcessId(id),
        id: ObjectId(id),
        version: 1,
        state: TransferState::Posted,
        left,
        right,
        amount,
    }
}
