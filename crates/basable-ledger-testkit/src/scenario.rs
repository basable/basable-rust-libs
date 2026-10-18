//! The scenario DSL: a ledger over the in-memory store with the fixture types
//! registered, and the readers a test asserts with.

use basable_ledger::{
    Balances, BatchAccount, BatchKey, InMemoryStore, Journal, Ledger, LedgerError, LedgerObject,
    ObjectId, Outcome, ProcessId, Request, Store, UnitKey,
};

use crate::fixture::{
    Invoice, InvoiceLedger, InvoiceLedgerV2, Payment, PaymentLedger, VatReturn, VatReturnLedger,
};

/// A ledger over a fresh in-memory store, the invoice (version 1), payment
/// and VAT return types registered.
pub struct Scenario {
    ledger: Ledger<InMemoryStore>,
    store: InMemoryStore,
}

impl Default for Scenario {
    fn default() -> Self {
        Self::new()
    }
}

impl Scenario {
    /// Version 1 of the invoice rules.
    pub fn new() -> Self {
        let store = InMemoryStore::new();
        let ledger = Ledger::builder(store.clone())
            .register::<Invoice, InvoiceLedger>()
            .register::<Payment, PaymentLedger>()
            .register::<VatReturn, VatReturnLedger>()
            .build();
        Scenario { ledger, store }
    }

    /// Version 2 of the invoice rules over the same store, as a deploy of
    /// new code would see it.
    pub fn upgraded(&self) -> Scenario {
        let store = self.store.clone();
        let ledger = Ledger::builder(store.clone())
            .register::<Invoice, InvoiceLedgerV2>()
            .register::<Payment, PaymentLedger>()
            .register::<VatReturn, VatReturnLedger>()
            .build();
        Scenario { ledger, store }
    }

    /// The ledger.
    pub fn ledger(&self) -> &Ledger<InMemoryStore> {
        &self.ledger
    }

    /// The store.
    pub fn store(&self) -> &InMemoryStore {
        &self.store
    }

    /// Books one object.
    pub async fn book<O: LedgerObject>(&self, object: O) -> Outcome {
        let outcomes = self
            .ledger
            .book(Request::new().with(object))
            .await
            .expect("book");
        outcomes.into_iter().next().expect("one outcome")
    }

    /// Books a call.
    pub async fn book_all(&self, request: Request) -> Result<Vec<Outcome>, LedgerError> {
        self.ledger.book(request).await
    }

    /// Books one object and asserts it booked.
    pub async fn booked<O: LedgerObject>(&self, object: O) {
        let outcome = self.book(object).await;
        assert_eq!(outcome, Outcome::Booked);
    }

    /// Every journal of a process.
    pub fn journals(&self, process: ProcessId) -> Vec<Journal> {
        self.store.journals(process)
    }

    /// The balances of a process.
    pub async fn balances(&self, process: ProcessId) -> Balances {
        self.ledger.balances(process).await.expect("balances")
    }

    /// One object's nonzero positions, per batch key and unit, in key order.
    pub async fn positions(
        &self,
        process: ProcessId,
        object: ObjectId,
    ) -> Vec<(BatchKey, UnitKey, i128)> {
        let balances = self.balances(process).await;
        balances.of(object).positions().collect()
    }

    /// A batch's sum per unit over every process: what a claim reads.
    pub async fn batch_sum(&self, account: &impl BatchAccount) -> Vec<(UnitKey, i128)> {
        self.store
            .batch_sum(account.batch_key())
            .await
            .expect("batch sum")
    }
}
