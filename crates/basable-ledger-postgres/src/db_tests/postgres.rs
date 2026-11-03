//! The Postgres store against a real database: the conformance suite, the
//! invoice fixture through the engine (replays included: a commit whose
//! acknowledgement was lost is repaired by the retry reading the row), a
//! batch closed by a booking, a rules bump restating, and the in-memory
//! store agreeing with Postgres on a history.

use std::collections::BTreeMap;

use basable_ledger::{
    BatchKey, BatchType, Commit, CommitOutcome, InMemoryStore, LedgerKey, Line, Outcome, Request,
    Store, UnitKey,
};
use basable_ledger_testkit::conformance;
use basable_ledger_testkit::fixture::{
    Batch, Currency, Invoice, InvoiceLedger, InvoiceLedgerV2, InvoiceState, Payment, PaymentLedger,
    PaymentState, new_invoice, new_payment, new_transfer, new_vat_return,
};

use super::{db, ledger, store};

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
        ledger.batch_sum(&paid.cash_batch()).await.expect("sum"),
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
    let vat = Batch::Vat("2026-10".into());
    let tax_office = Batch::TaxOffice("2026-10".into());
    let eur = UnitKey(978);
    assert_eq!(
        ledger.batch_sum(&vat).await.expect("sum"),
        vec![(eur, 3_360)]
    );
    ledger
        .book(Request::new().with(new_vat_return("2026-10", 3_360, Currency::Eur)))
        .await
        .expect("file");
    assert_eq!(ledger.batch_sum(&vat).await.expect("sum"), vec![(eur, 0)]);
    assert_eq!(
        ledger.batch_sum(&tax_office).await.expect("sum"),
        vec![(eur, 3_360)]
    );
    let mut corrected = issued[2].next(InvoiceState::Issued, 0);
    corrected.net = 500;
    corrected.vat = 105;
    ledger
        .book(Request::new().with(corrected))
        .await
        .expect("correct");
    assert_eq!(
        ledger.batch_sum(&vat).await.expect("sum"),
        vec![(eur, -105)]
    );
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
    let v2 = basable_ledger::Ledger::builder(store.clone())
        .register::<Invoice, InvoiceLedgerV2>()
        .register::<Payment, PaymentLedger>()
        .build();
    let restated = v2.restate(p).await.expect("restate");
    assert_eq!(restated.objects, 1);
    assert_eq!(restated.lines, 2);
    let month = v1
        .batch_key(&issued.vat_batch())
        .await
        .expect("find")
        .expect("a row");
    let b = v2.balances(p).await.expect("balances");
    assert_eq!(b.of(issued.id).amount_by_key(month, Currency::Eur), 0);
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
    let mem = basable_ledger::Ledger::builder(InMemoryStore::<Batch>::new())
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
    // Keys are each store's own, so the two agree by value.
    let a = pg.balances(p).await.expect("pg");
    let b = mem.balances(p).await.expect("mem");
    let last = history.last().expect("one");
    for (batch, unit) in [
        (last.receivable_batch(), UnitKey(978)),
        (last.cash_batch(), UnitKey(978)),
        (last.revenue_batch(), UnitKey(978)),
        (last.vat_batch(), UnitKey(978)),
        (last.points_batch(), UnitKey(1)),
    ] {
        assert_eq!(
            a.of(last.id).amount(&batch, unit),
            b.of(last.id).amount(&batch, unit),
            "{batch:?}"
        );
    }
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
            new_batches: Vec::new(),
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

/// A batch's row through the engine on real tables: one key over several
/// tables and processes, the first value's other columns, a value that
/// disagrees with its row refused, nothing left by a refused booking.
#[tokio::test]
async fn batches_get_their_rows_on_postgres() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let store = store(&db).await;
    let ledger = ledger(store.clone());
    let monday = Batch::Settlement {
        merchant: "acme".into(),
        settle_day: "monday".into(),
    };
    let friday = Batch::Settlement {
        merchant: "acme".into(),
        settle_day: "friday".into(),
    };
    let revenue = Batch::Revenue("2026-11".into());
    for (settlement, amount) in [(monday.clone(), 5), (friday.clone(), 7)] {
        assert_eq!(
            ledger
                .book(Request::new().with(new_transfer(settlement, revenue.clone(), amount)))
                .await
                .expect("book"),
            vec![Outcome::Booked]
        );
    }
    let key = ledger
        .batch_key(&monday)
        .await
        .expect("find")
        .expect("a row");
    assert_eq!(ledger.batch_key(&friday).await.expect("find"), Some(key));
    assert_ne!(ledger.batch_key(&revenue).await.expect("find"), Some(key));
    assert_eq!(
        ledger.batch_sum(&friday).await.expect("sum"),
        vec![(UnitKey(978), 12)]
    );
    let day: String = sqlx::query_scalar("SELECT settle_day FROM settlement_batch")
        .fetch_one(store.pool())
        .await
        .expect("row");
    assert_eq!(day, "monday", "the first value's column is the row's");

    // The same name in two books is two batches.
    let in_book = |ledger: u32, name: &str, batch_type: BatchType| Batch::InBook {
        name: name.into(),
        ledger: LedgerKey(ledger),
        batch_type,
    };
    for book in [1, 2] {
        let t = new_transfer(
            in_book(book, "receivable", BatchType::Debit),
            in_book(book, "revenue", BatchType::Credit),
            5,
        );
        assert_eq!(
            ledger.book(Request::new().with(t)).await.expect("book"),
            vec![Outcome::Booked]
        );
    }
    assert_ne!(
        ledger
            .batch_key(&in_book(1, "revenue", BatchType::Credit))
            .await
            .expect("find"),
        ledger
            .batch_key(&in_book(2, "revenue", BatchType::Credit))
            .await
            .expect("find"),
    );

    // A batch is a debit for good: a later call naming it a credit is refused,
    // and the new batch beside it gets no row.
    let named = |name: &str, batch_type: BatchType| Batch::Named {
        name: name.into(),
        ledger: LedgerKey(1),
        batch_type,
    };
    let booked = new_transfer(
        named("for good", BatchType::Debit),
        named("beside it", BatchType::Credit),
        1,
    );
    assert_eq!(
        ledger
            .book(Request::new().with(booked))
            .await
            .expect("book"),
        vec![Outcome::Booked]
    );
    let wrong = named("fresh", BatchType::Debit);
    let refused = new_transfer(wrong.clone(), named("for good", BatchType::Credit), 1);
    let outcomes = ledger
        .book(Request::new().with(refused.clone()))
        .await
        .expect("book");
    assert!(matches!(outcomes[0], Outcome::Rejected(_)), "{outcomes:?}");
    assert_eq!(ledger.batch_key(&wrong).await.expect("find"), None);
    assert!(
        store
            .read(refused.process_id)
            .await
            .expect("read")
            .is_empty()
    );
    db.finish().await;
}

/// Two processes using the same new batches for the first time, naming them
/// in opposite orders, cannot deadlock: the commit creates rows in primary key
/// order. Postgres would abort one with 40P01, which this would surface.
#[tokio::test]
async fn first_uses_in_opposite_orders_do_not_deadlock() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let store = store(&db).await;
    let ledger = std::sync::Arc::new(ledger(store.clone()));
    for round in 0..8 {
        let debit = Batch::Named {
            name: format!("debit-{round}"),
            ledger: LedgerKey(1),
            batch_type: BatchType::Debit,
        };
        let credit = Batch::Named {
            name: format!("credit-{round}"),
            ledger: LedgerKey(1),
            batch_type: BatchType::Credit,
        };
        let mut tasks = Vec::new();
        for i in 0..8 {
            let ledger = ledger.clone();
            let (left, right) = if i % 2 == 0 {
                (debit.clone(), credit.clone())
            } else {
                (credit.clone(), debit.clone())
            };
            tasks.push(tokio::spawn(async move {
                ledger
                    .book(Request::new().with(new_transfer(left, right, 1)))
                    .await
                    .expect("no deadlock")
            }));
        }
        for t in tasks {
            assert_eq!(t.await.expect("task"), vec![Outcome::Booked]);
        }
        assert_eq!(
            ledger.batch_sum(&debit).await.expect("sum"),
            vec![(UnitKey(978), 8)]
        );
    }
    db.finish().await;
}
