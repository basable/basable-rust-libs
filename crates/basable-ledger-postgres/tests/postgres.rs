//! The Postgres store against a real database: the conformance suite, the
//! invoice fixture through the engine (replays included: a commit whose
//! acknowledgement was lost is repaired by the retry reading the row), a
//! batch closed by a booking, a rules bump restating, and the in-memory
//! store agreeing with Postgres on a history.

use basable_db::{Nanoservice, Stateful};
use std::collections::BTreeMap;

use basable_ledger::{
    BatchAccount, BatchKey, Commit, CommitOutcome, InMemoryStore, Ledger, Line, Outcome, Request,
    Store, UnitKey,
};
use basable_ledger_postgres::{PgStore, SCHEMA_PLACEHOLDER, migrations};
use basable_ledger_testkit::conformance;
use basable_ledger_testkit::fixture::{
    Account, Currency, Invoice, InvoiceLedger, InvoiceLedgerV2, InvoiceState, Payment,
    PaymentLedger, PaymentState, VatReturn, VatReturnLedger, new_invoice, new_payment,
    new_vat_return,
};
use basable_testkit::{TestDb, runfile};

struct Orders;
impl Nanoservice for Orders {
    const NAME: &'static str = "orders";
}
impl Stateful for Orders {}

/// The fixture database with the ledger's tables in the orders schema, or
/// `None` without `TEST_DATABASE_URL`.
async fn db() -> Option<TestDb> {
    let db = TestDb::from_env_with_migrations(&runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await?;
    let ms = migrations().expect("migration parses");
    basable_db::migrate::apply(db.migrator(), &ms, &[(SCHEMA_PLACEHOLDER, "nano_orders")])
        .await
        .expect("ledger tables applied");
    Some(db)
}

async fn store(db: &TestDb) -> PgStore {
    let pool = db.nano_pool::<Orders>().await;
    PgStore::new(pool.pool().clone())
}

fn ledger(store: PgStore) -> Ledger<PgStore> {
    Ledger::builder(store)
        .register::<Invoice, InvoiceLedger>()
        .register::<Payment, PaymentLedger>()
        .register::<VatReturn, VatReturnLedger>()
        .build()
}

#[tokio::test]
async fn the_postgres_store_conforms() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let store = store(&db).await;
    conformance::run_all(&store).await;
    db.finish().await;
}

#[tokio::test]
async fn the_invoice_lifecycle_books_on_postgres() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let store = store(&db).await;
    let ledger = ledger(store.clone());
    let draft = new_invoice(10_000, 2_100, Currency::Eur);
    let p = draft.process_id;
    let issued = draft.next(InvoiceState::Issued, 0);
    let payment = new_payment(&issued, 12_100);
    let outcomes = ledger
        .book(Request::new().with(issued.clone()).with(payment.clone()))
        .await
        .expect("book");
    assert_eq!(outcomes, vec![Outcome::Booked, Outcome::Booked]);
    let settled = payment.next(PaymentState::Settled);
    let outcomes = ledger
        .book(Request::new().with(settled.clone()))
        .await
        .expect("book");
    assert_eq!(outcomes, vec![Outcome::Booked]);
    let paid = issued.next(InvoiceState::Paid, 12_100);
    assert_eq!(
        ledger
            .book(Request::new().with(paid.clone()))
            .await
            .expect("book"),
        vec![Outcome::Booked]
    );
    let b = ledger.balances(p).await.expect("balances");
    let eur = Currency::Eur;
    // The invoice holds cash, revenue and VAT; the payment still holds its
    // clearing pair, so the batches net: receivable 0 minus 12 100, cash
    // 12 100 plus 12 100.
    assert_eq!(b.of(paid.id).amount(&paid.cash_batch(), eur), 12_100);
    assert_eq!(b.of(paid.id).amount(&paid.receivable_batch(), eur), 0);
    assert_eq!(
        b.of(settled.id).amount(&paid.receivable_batch(), eur),
        -12_100
    );
    assert_eq!(b.batch(&paid.receivable_batch(), eur), -12_100);
    assert_eq!(b.batch(&paid.cash_batch(), eur), 24_200);
    // Replays are recognised against the real rows.
    assert_eq!(
        ledger
            .book(Request::new().with(paid.clone()))
            .await
            .expect("book"),
        vec![Outcome::AlreadyBooked]
    );
    assert_eq!(
        ledger
            .book(Request::new().with(issued.clone()))
            .await
            .expect("book"),
        vec![Outcome::Stale]
    );
    let journals = store.read(p).await.expect("read");
    assert_eq!(journals.len(), 3);
    // Both types are indexed at the version they were evaluated under.
    for type_name in ["invoice", "payment"] {
        assert_eq!(
            store
                .processes_behind(type_name, 2, 10)
                .await
                .expect("behind"),
            vec![p]
        );
        assert!(
            store
                .processes_behind(type_name, 1, 10)
                .await
                .expect("behind")
                .is_empty()
        );
    }
    // The batch sum spans the whole process.
    assert_eq!(
        store
            .batch_sum(paid.cash_batch().batch_key())
            .await
            .expect("sum"),
        vec![(UnitKey(978), 24_200)]
    );
    db.finish().await;
}

/// A batch closes by a booking that brings its sum to zero; the sum is read
/// over the lines index across processes, and a late line is the next
/// return's remainder.
#[tokio::test]
async fn a_batch_is_closed_by_booking_its_balance_to_zero_on_postgres() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let store = store(&db).await;
    let ledger = ledger(store.clone());
    let mut issued = Vec::new();
    for (net, vat) in [(10_000, 2_100), (5_000, 1_050), (1_000, 210)] {
        let i = new_invoice(net, vat, Currency::Eur).next(InvoiceState::Issued, 0);
        assert_eq!(
            ledger
                .book(Request::new().with(i.clone()))
                .await
                .expect("book"),
            vec![Outcome::Booked]
        );
        issued.push(i);
    }
    let vat = Account::Vat("2026-10".into()).batch_key();
    let tax_office = Account::TaxOffice("2026-10".into()).batch_key();
    let eur = UnitKey(978);
    assert_eq!(store.batch_sum(vat).await.expect("sum"), vec![(eur, 3_360)]);
    ledger
        .book(Request::new().with(new_vat_return("2026-10", 3_360, Currency::Eur)))
        .await
        .expect("file");
    assert_eq!(store.batch_sum(vat).await.expect("sum"), vec![(eur, 0)]);
    assert_eq!(
        store.batch_sum(tax_office).await.expect("sum"),
        vec![(eur, 3_360)]
    );
    let mut corrected = issued[2].next(InvoiceState::Issued, 0);
    corrected.net = 500;
    corrected.vat = 105;
    ledger
        .book(Request::new().with(corrected))
        .await
        .expect("correct");
    assert_eq!(store.batch_sum(vat).await.expect("sum"), vec![(eur, -105)]);
    db.finish().await;
}

#[tokio::test]
async fn a_rules_bump_restates_on_postgres() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let store = store(&db).await;
    let v1 = ledger(store.clone());
    let draft = new_invoice(10_000, 2_100, Currency::Eur);
    let p = draft.process_id;
    let issued = draft.next(InvoiceState::Issued, 0);
    assert_eq!(
        v1.book(Request::new().with(issued.clone()))
            .await
            .expect("book"),
        vec![Outcome::Booked]
    );
    let v2 = Ledger::builder(store.clone())
        .register::<Invoice, InvoiceLedgerV2>()
        .register::<Payment, PaymentLedger>()
        .build();
    let restated = v2.restate(p).await.expect("restate");
    assert_eq!(restated.objects, 1);
    assert_eq!(restated.lines, 2);
    let b = v2.balances(p).await.expect("balances");
    assert_eq!(
        b.of(issued.id).amount(&issued.vat_batch(), Currency::Eur),
        0
    );
    assert_eq!(
        b.of(issued.id)
            .amount(&InvoiceLedgerV2::vat_batch(&issued), Currency::Eur),
        2_100
    );
    let journals = store.read(p).await.expect("read");
    assert!(journals[1].snapshot_diffs.is_empty());
    assert!(
        store
            .processes_behind("invoice", 2, 10)
            .await
            .expect("behind")
            .is_empty(),
        "indexed at version 2"
    );
    db.finish().await;
}

/// The in-memory store and Postgres agree on a history.
#[tokio::test]
async fn postgres_agrees_with_the_in_memory_store() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let pg = ledger(store(&db).await);
    let mem = Ledger::builder(InMemoryStore::new())
        .register::<Invoice, InvoiceLedger>()
        .register::<Payment, PaymentLedger>()
        .build();
    let draft = new_invoice(7_777, 1_633, Currency::Eur);
    let p = draft.process_id;
    let history = vec![
        draft.clone(),
        draft.next(InvoiceState::Issued, 0),
        draft
            .next(InvoiceState::Issued, 0)
            .next(InvoiceState::PartiallyPaid, 3_000),
        draft
            .next(InvoiceState::Issued, 0)
            .next(InvoiceState::PartiallyPaid, 3_000)
            .next(InvoiceState::Paid, 9_410),
    ];
    for snapshot in &history {
        let a = pg
            .book(Request::new().with(snapshot.clone()))
            .await
            .expect("pg");
        let b = mem
            .book(Request::new().with(snapshot.clone()))
            .await
            .expect("mem");
        assert_eq!(a, b);
    }
    let a = pg.balances(p).await.expect("pg");
    let b = mem.balances(p).await.expect("mem");
    assert_eq!(a, b);
    db.finish().await;
}

/// The lines index serves the batch sum: a batch key that also appears as a
/// unit or an amount in other lines is not matched by them.
#[tokio::test]
async fn the_batch_sum_matches_keys_only() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let store = store(&db).await;
    let invoice = new_invoice(1, 0, Currency::Eur);
    // Batch 978 in unit 978 with amount 978, and a line in batch 5 whose
    // unit is 978: the sum of batch 978 sees the first line only, the sum
    // of batch 5 the second.
    let outcome = store
        .commit(Commit {
            process_id: invoice.process_id,
            after_sequence: 0,
            lines: vec![
                Line {
                    batch: BatchKey(978),
                    object_id: invoice.id,
                    unit: UnitKey(978),
                    amount: 978,
                },
                Line {
                    batch: BatchKey(5),
                    object_id: invoice.id,
                    unit: UnitKey(978),
                    amount: 978,
                },
            ],
            snapshot_diffs: Vec::new(),
            versions: BTreeMap::new(),
        })
        .await
        .expect("commit");
    assert_eq!(outcome, CommitOutcome::Committed);
    assert_eq!(
        store.batch_sum(BatchKey(978)).await.expect("sum"),
        vec![(UnitKey(978), 978)]
    );
    assert_eq!(
        store.batch_sum(BatchKey(5)).await.expect("sum"),
        vec![(UnitKey(978), 978)]
    );
    db.finish().await;
}
