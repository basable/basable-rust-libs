//! What a batch's row does through the engine: it lands just before the first
//! journal that books on the batch and not otherwise, one primary key is one
//! batch wherever it is named, a value that disagrees with its row rejects, a
//! row whose journal never landed stays and is used, and new rules may name
//! new batches for old objects.

use std::collections::BTreeMap;
use std::sync::Mutex;

use basable_ledger::{
    BatchKey, BatchType, Commit, CommitOutcome, InMemoryStore, Journal, Ledger, LedgerKey, Outcome,
    ProcessId, Request, Restated, Store, StoreError, StoredBatch, UnitKey,
};
use basable_ledger_testkit::Scenario;
use basable_ledger_testkit::fixture::{
    Batch, Currency, InvoiceLedgerV2, InvoiceState, Transfer, TransferLedger, new_invoice,
    new_transfer,
};

fn named(name: &str, batch_type: BatchType) -> Batch {
    Batch::Named {
        name: name.to_owned(),
        ledger: LedgerKey(1),
        batch_type,
    }
}

fn credit(name: &str) -> Batch {
    named(name, BatchType::Credit)
}

fn debit(name: &str) -> Batch {
    named(name, BatchType::Debit)
}

#[tokio::test]
async fn a_batch_gets_its_row_when_a_journal_first_books_on_it() {
    let s = Scenario::new();
    let draft = new_invoice(100, 21, Currency::Eur);
    assert_eq!(
        s.ledger()
            .batch_key(&draft.receivable_batch())
            .await
            .unwrap(),
        None
    );
    // A draft holds nothing, so it names nothing and creates nothing.
    s.booked(draft.clone()).await;
    assert_eq!(s.store().batch_count(), 0);
    let issued = draft.next(InvoiceState::Issued, 0);
    s.booked(issued.clone()).await;
    assert!(
        s.ledger()
            .batch_key(&issued.receivable_batch())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_batch_named_with_nothing_to_book_gets_no_row() {
    let s = Scenario::new();
    // Issued names the cash batch with what is paid so far: nothing.
    let issued = new_invoice(100, 21, Currency::Eur).next(InvoiceState::Issued, 0);
    s.booked(issued.clone()).await;
    assert_eq!(
        s.ledger().batch_key(&issued.cash_batch()).await.unwrap(),
        None
    );
    // It gets one when a line lands in it.
    let partly = issued.next(InvoiceState::PartiallyPaid, 50);
    s.booked(partly.clone()).await;
    assert!(
        s.ledger()
            .batch_key(&partly.cash_batch())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_rejected_booking_leaves_no_row() {
    let s = Scenario::new();
    // Paid is not a state an invoice may start in.
    let half_paid = new_invoice(100, 21, Currency::Eur).next(InvoiceState::Paid, 60);
    assert!(matches!(
        s.book(half_paid.clone()).await,
        Outcome::Rejected(_)
    ));
    assert_eq!(s.store().batch_count(), 0);
    assert!(s.journals(half_paid.process_id).is_empty());
    // A desired state that does not balance names its batches and books none.
    let unbalanced = new_transfer(debit("left"), debit("right"), 5);
    assert!(matches!(s.book(unbalanced).await, Outcome::Rejected(_)));
    assert_eq!(s.store().batch_count(), 0);
}

#[tokio::test]
async fn a_stale_snapshot_creates_nothing() {
    let s = Scenario::new();
    let t = new_transfer(debit("a"), credit("b"), 5);
    s.booked(t.clone()).await;
    let rows = s.store().batch_count();
    // An older version naming other batches is not evaluated at all.
    let mut stale = t.clone();
    stale.version = 0;
    stale.left = debit("never");
    stale.right = credit("never either");
    assert_eq!(s.book(stale).await, Outcome::Stale);
    assert_eq!(s.store().batch_count(), rows);
}

#[tokio::test]
async fn one_batch_named_by_two_processes_has_one_key() {
    let s = Scenario::new();
    let first = new_transfer(debit("shared"), credit("first"), 5);
    let second = new_transfer(debit("shared"), credit("second"), 7);
    s.booked(first.clone()).await;
    let key = s.key(&debit("shared")).await;
    s.booked(second.clone()).await;
    assert_eq!(s.key(&debit("shared")).await, key);
    assert_eq!(
        s.batch_sum(&debit("shared")).await,
        vec![(UnitKey(978), 12)]
    );
    // The other batches have keys of their own, from the one counter.
    assert_ne!(s.key(&credit("first")).await, key);
    assert_ne!(
        s.key(&credit("first")).await,
        s.key(&credit("second")).await
    );
}

/// The first value's other columns are the row's, and a value differing only
/// in them is the same batch: identity is the primary key alone.
#[tokio::test]
async fn two_values_with_one_primary_key_are_one_batch() {
    let s = Scenario::new();
    let monday = Batch::Settlement {
        merchant: "acme".into(),
        settle_day: "monday".into(),
    };
    let friday = Batch::Settlement {
        merchant: "acme".into(),
        settle_day: "friday".into(),
    };
    s.booked(new_transfer(monday.clone(), credit("x"), 5)).await;
    s.booked(new_transfer(friday.clone(), credit("y"), 7)).await;
    assert_eq!(s.key(&monday).await, s.key(&friday).await);
    assert_eq!(s.batch_sum(&friday).await, vec![(UnitKey(978), 12)]);
    // Within one call too: one transfer naming both is one position.
    let both = new_transfer(monday.clone(), friday.clone(), 1);
    match s.book(both).await {
        // Both debits, so it does not balance: the two values summed.
        Outcome::Rejected(r) => assert!(r.message().contains("does not balance"), "{r}"),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn the_same_thing_in_two_books_is_two_batches() {
    let s = Scenario::new();
    let in_book = |ledger: u32, batch_type: BatchType| Batch::InBook {
        name: "revenue/2026-11".into(),
        ledger: LedgerKey(ledger),
        batch_type,
    };
    let contra = |ledger: u32, batch_type: BatchType| Batch::InBook {
        name: "receivable/2026-11".into(),
        ledger: LedgerKey(ledger),
        batch_type,
    };
    s.booked(new_transfer(
        contra(1, BatchType::Debit),
        in_book(1, BatchType::Credit),
        5,
    ))
    .await;
    s.booked(new_transfer(
        contra(2, BatchType::Debit),
        in_book(2, BatchType::Credit),
        7,
    ))
    .await;
    assert_ne!(
        s.key(&in_book(1, BatchType::Credit)).await,
        s.key(&in_book(2, BatchType::Credit)).await
    );
    assert_eq!(
        s.batch_sum(&in_book(2, BatchType::Credit)).await,
        vec![(UnitKey(978), 7)]
    );
}

#[tokio::test]
async fn a_value_that_disagrees_with_its_row_rejects_across_calls() {
    let s = Scenario::new();
    s.booked(new_transfer(debit("a"), credit("b"), 5)).await;
    // "a" is a debit for good; a later call calling it a credit is refused.
    let t = new_transfer(debit("c"), credit("a"), 5);
    let process = t.process_id;
    let rows = s.store().batch_count();
    match s.book(t).await {
        Outcome::Rejected(r) => {
            assert!(r.message().contains("named/a"), "{r}");
            assert!(r.message().contains("as debit"), "{r}");
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(s.journals(process).is_empty());
    assert_eq!(
        s.store().batch_count(),
        rows,
        "the new batch \"c\" got no row"
    );
}

#[tokio::test]
async fn a_value_that_disagrees_within_one_call_rejects() {
    let s = Scenario::new();
    let t = new_transfer(debit("twice"), credit("twice"), 5);
    assert!(matches!(s.book(t).await, Outcome::Rejected(_)));
    assert_eq!(s.store().batch_count(), 0);
}

/// New rules may name a batch no object was ever booked on: its row lands with
/// the restatement, which is what an allocated key on the object could not do.
#[tokio::test]
async fn a_restatement_creates_the_rows_of_new_batches() {
    let v1 = Scenario::new();
    let issued = new_invoice(1_000, 210, Currency::Eur).next(InvoiceState::Issued, 0);
    v1.booked(issued.clone()).await;
    let quarter = InvoiceLedgerV2::vat_batch(&issued);
    assert_eq!(v1.ledger().batch_key(&quarter).await.unwrap(), None);
    let v2 = v1.upgraded();
    assert_eq!(
        v2.ledger().restate(issued.process_id).await.unwrap(),
        Restated {
            objects: 1,
            lines: 2
        }
    );
    let key = v2.key(&quarter).await;
    assert_eq!(
        v2.positions(issued.process_id, issued.id)
            .await
            .iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, _, a)| *a),
        Some(210)
    );
}

#[tokio::test]
async fn one_journal_books_on_several_new_batches_with_the_diffs() {
    let s = Scenario::new();
    let invoice = new_invoice(1_000, 210, Currency::Eur).next(InvoiceState::Issued, 0);
    let transfer = new_transfer(debit("in the same"), credit("call"), 3);
    let outcomes = s
        .book_all(Request::new().with(invoice.clone()).with(transfer.clone()))
        .await
        .unwrap();
    assert_eq!(outcomes, vec![Outcome::Booked, Outcome::Booked]);
    // Four invoice batches and two transfer ones, each with a real key.
    assert_eq!(s.store().batch_count(), 6);
    for journal in s
        .journals(invoice.process_id)
        .iter()
        .chain(s.journals(transfer.process_id).iter())
    {
        assert!(journal.lines.iter().all(|l| l.batch.0 > 0), "{journal:?}");
    }
}

/// Another writer getting in between a booking's plan and its first row.
enum Interference {
    /// A concurrent first use creates this row.
    Row(Batch),
    /// A concurrent booking of the same process takes the sequence.
    Journal(Commit),
}

/// The in-memory store, with one [`Interference`] on the first `ensure`.
struct Interfered {
    inner: InMemoryStore<Batch>,
    pending: Mutex<Option<Interference>>,
}

impl Store for Interfered {
    type Batch = Batch;

    async fn read(&self, process: ProcessId) -> Result<Vec<Journal>, StoreError> {
        self.inner.read(process).await
    }

    async fn find(&self, batch: &Batch) -> Result<Option<StoredBatch>, StoreError> {
        self.inner.find(batch).await
    }

    async fn ensure(&self, batch: &Batch) -> Result<StoredBatch, StoreError> {
        let pending = self.pending.lock().expect("lock").take();
        match pending {
            Some(Interference::Row(other)) => {
                self.inner.ensure(&other).await?;
            }
            Some(Interference::Journal(commit)) => {
                self.inner.commit(commit).await?;
            }
            None => {}
        }
        self.inner.ensure(batch).await
    }

    async fn commit(&self, commit: Commit) -> Result<CommitOutcome, StoreError> {
        self.inner.commit(commit).await
    }

    async fn record_versions(
        &self,
        process: ProcessId,
        versions: &BTreeMap<String, u32>,
    ) -> Result<(), StoreError> {
        self.inner.record_versions(process, versions).await
    }

    async fn batch_sum(&self, batch: BatchKey) -> Result<Vec<(UnitKey, i128)>, StoreError> {
        self.inner.batch_sum(batch).await
    }

    async fn processes_behind(
        &self,
        type_name: &str,
        version: u32,
        limit: u32,
    ) -> Result<Vec<ProcessId>, StoreError> {
        self.inner.processes_behind(type_name, version, limit).await
    }
}

fn interfered(interference: Interference) -> Ledger<Interfered> {
    let store = Interfered {
        inner: InMemoryStore::new(),
        pending: Mutex::new(Some(interference)),
    };
    Ledger::builder(store)
        .register::<Transfer, TransferLedger>()
        .build()
}

/// Both first uses saw no row, and the other one created it under another type:
/// this booking is refused, the row stays as the other wrote it, and a row this
/// booking had already created stays with nothing booked on it.
#[tokio::test]
async fn a_value_that_disagrees_with_a_concurrent_first_use_rejects() {
    let contested = debit("z contested");
    let ledger = interfered(Interference::Row(credit("z contested")));
    let t = new_transfer(contested.clone(), credit("a first"), 5);
    let outcomes = ledger.book(Request::new().with(t.clone())).await.unwrap();
    match &outcomes[0] {
        Outcome::Rejected(r) => assert!(r.message().contains("as credit"), "{r}"),
        other => panic!("expected a rejection, got {other:?}"),
    }
    let store = &ledger.store().inner;
    assert!(store.journals(t.process_id).is_empty());
    assert_eq!(store.batch_count(), 2);
    let row = store.find(&contested).await.unwrap().expect("a row");
    assert_eq!(row.batch_type, BatchType::Credit);
    assert_eq!(ledger.batch_sum(&credit("a first")).await.unwrap(), vec![]);
}

/// A booking that loses the sequence after creating its row reads again, finds
/// the row and books on it.
#[tokio::test]
async fn a_lost_race_keeps_the_row_and_the_retry_books_on_it() {
    let t = new_transfer(debit("left"), credit("right"), 5);
    let ledger = interfered(Interference::Journal(Commit {
        process_id: t.process_id,
        after_sequence: 0,
        lines: Vec::new(),
        snapshot_diffs: Vec::new(),
        versions: BTreeMap::new(),
    }));
    assert_eq!(
        ledger.book(Request::new().with(t.clone())).await.unwrap(),
        vec![Outcome::Booked]
    );
    let store = &ledger.store().inner;
    assert_eq!(store.batch_count(), 2);
    let journals = store.journals(t.process_id);
    assert_eq!(journals.len(), 2);
    let key = ledger.batch_key(&debit("left")).await.unwrap().unwrap();
    assert!(journals[1].lines.iter().any(|l| l.batch == key));
    assert!(journals[1].lines.iter().all(|l| l.batch.0 > 0));
}

/// A crash between a row and its journal leaves a row with nothing booked on
/// it: it sums to nothing, and the next booking on the batch uses it.
#[tokio::test]
async fn a_row_with_no_journal_is_the_batch_the_next_booking_uses() {
    let s = Scenario::new();
    let left = debit("left");
    let row = s.store().ensure(&left).await.unwrap();
    assert_eq!(s.ledger().batch_sum(&left).await.unwrap(), vec![]);
    s.booked(new_transfer(left.clone(), credit("right"), 5))
        .await;
    assert_eq!(s.key(&left).await, row.key);
    assert_eq!(s.store().batch_count(), 2);
    assert_eq!(
        s.ledger().batch_sum(&left).await.unwrap(),
        vec![(UnitKey(978), 5)]
    );
}
