//! The optional projector: applies in booked order, agrees with the
//! journals, skips what the overlap shows again, stops on a gap, recovers
//! from a wider window, and draws a trial balance that balances.

use std::collections::BTreeMap;
use std::time::Duration;

use basable_db::{Nanoservice, Stateful};
use basable_ledger::{BatchType, Ledger, Outcome, ProcessId, Request, Store, UnitKey};
use basable_ledger_postgres::{PgStore, Projector, ProjectorError, SCHEMA_PLACEHOLDER};
use basable_ledger_testkit::fixture::{
    Currency, Invoice, InvoiceLedger, InvoiceState, PLATFORM_LEDGER, Payment, PaymentLedger,
    PaymentState, new_invoice, new_payment,
};
use basable_ledger_testkit::projection;
use basable_testkit::{TestDb, runfile};

struct Orders;
impl Nanoservice for Orders {
    const NAME: &'static str = "orders";
}
impl Stateful for Orders {}

async fn db() -> Option<TestDb> {
    let db = TestDb::from_env_with_migrations(&runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await?;
    let mut ms = basable_ledger_postgres::migrations().expect("parses");
    ms.extend(basable_ledger_postgres::projector_migrations().expect("parses"));
    basable_db::migrate::apply(db.migrator(), &ms, &[(SCHEMA_PLACEHOLDER, "nano_orders")])
        .await
        .expect("ledger and projector tables");
    Some(db)
}

#[tokio::test]
async fn the_projector_agrees_with_the_journals_and_skips_the_overlap() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let pool = db.nano_pool::<Orders>().await;
    let store = PgStore::new(pool.pool().clone());
    let projector = Projector::new(pool.pool().clone(), Duration::from_secs(2));
    let ledger = Ledger::builder(store.clone())
        .register::<Invoice, InvoiceLedger>()
        .register::<Payment, PaymentLedger>()
        .build();
    let a = new_invoice(10_000, 2_100, Currency::Eur).next(InvoiceState::Issued, 0);
    let b = new_invoice(5_000, 1_050, Currency::Usd).next(InvoiceState::Issued, 0);
    let payment = new_payment(&a, 4_000);
    ledger
        .book(
            Request::new()
                .with(a.clone())
                .with(b.clone())
                .with(payment.clone()),
        )
        .await
        .expect("book");
    ledger
        .book(Request::new().with(payment.next(PaymentState::Settled)))
        .await
        .expect("book");
    ledger
        .book(Request::new().with(a.next(InvoiceState::PartiallyPaid, 4_000)))
        .await
        .expect("book");
    let processes: Vec<ProcessId> = vec![a.process_id, b.process_id];

    let first = projector.run_once().await.expect("run");
    assert_eq!(
        first.applied, 4,
        "three journals of a's process and one of b's"
    );
    assert_eq!(first.skipped, 0);
    projection::check_agrees(&store, &processes, |batch, unit| {
        let projector = projector.clone();
        async move { projector.balance(batch, unit).await.expect("balance") }
    })
    .await;

    // The next run sees the same journals again through the overlap and applies none.
    let second = projector.run_once().await.expect("run");
    assert_eq!(
        second,
        basable_ledger_postgres::Applied {
            applied: 0,
            skipped: 4
        }
    );
    projection::check_agrees(&store, &processes, |batch, unit| {
        let projector = projector.clone();
        async move { projector.balance(batch, unit).await.expect("balance") }
    })
    .await;

    // The trial balance of the ledger balances: debit equals credit per unit.
    let rows = projector
        .trial_balance(PLATFORM_LEDGER)
        .await
        .expect("trial balance");
    let mut sums: BTreeMap<UnitKey, i128> = BTreeMap::new();
    for row in &rows {
        match row.batch_type {
            BatchType::Debit => *sums.entry(row.unit).or_insert(0) += row.amount,
            BatchType::Credit => *sums.entry(row.unit).or_insert(0) -= row.amount,
            BatchType::Memo => {}
        }
    }
    assert!(sums.values().all(|s| *s == 0), "{sums:?}");
    assert!(
        rows.iter().any(|r| r.batch_type == BatchType::Memo),
        "points are projected too"
    );
    // The settled payment's minus 4 000 on the receivable batch is offset by
    // the invoice's own 8 100 left: the batch is positive, no negative balance.
    assert!(
        projector
            .negative_balances()
            .await
            .expect("negative")
            .is_empty()
    );
    pool.close().await;
    db.finish().await;
}

#[tokio::test]
async fn a_missed_journal_stops_the_run_and_a_wider_window_recovers() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let pool = db.nano_pool::<Orders>().await;
    let store = PgStore::new(pool.pool().clone());
    let projector = Projector::new(pool.pool().clone(), Duration::from_secs(2));
    let ledger = Ledger::builder(store.clone())
        .register::<Invoice, InvoiceLedger>()
        .register::<Payment, PaymentLedger>()
        .build();
    let a = new_invoice(1_000, 210, Currency::Eur).next(InvoiceState::Issued, 0);
    let p = a.process_id;
    assert_eq!(
        ledger
            .book(Request::new().with(a.clone()))
            .await
            .expect("book"),
        vec![Outcome::Booked]
    );
    let first = projector.run_once().await.expect("run");
    assert_eq!(first.applied, 1);

    // Book sequence 2 and backdate it beyond the overlap: a transaction
    // that outlived the window, as the projector would experience it.
    let partly = a.next(InvoiceState::PartiallyPaid, 400);
    ledger
        .book(Request::new().with(partly.clone()))
        .await
        .expect("book");
    sqlx::query(
        "UPDATE journal SET booked_at = booked_at - interval '1 hour' \
         WHERE process_id = $1 AND sequence = 2",
    )
    .bind(p.0)
    .execute(pool.pool())
    .await
    .expect("backdate");
    // Sequence 3 arrives in the window; the projector sees 3 after 1 and stops.
    let paid = partly.next(InvoiceState::Paid, 1_210);
    ledger
        .book(Request::new().with(paid.clone()))
        .await
        .expect("book");
    match projector.run_once().await {
        Err(ProjectorError::Gap {
            process,
            expected,
            found,
        }) => {
            assert_eq!(process, p);
            assert_eq!((expected, found), (2, 3));
        }
        other => panic!("expected a gap, got {other:?}"),
    }
    // Nothing of the failed run was applied: the balances are still those of sequence 1.
    let read = store.read(p).await.expect("read");
    let receivable = read.journals[0].lines[0].batch_key;
    let before = projector
        .balance(receivable, UnitKey(978))
        .await
        .expect("balance");
    assert_eq!(before, 1_210);
    // A wider window applies 2 and 3 in order.
    let recovered = projector
        .run_from(chrono::DateTime::UNIX_EPOCH)
        .await
        .expect("recover");
    assert_eq!(recovered.applied, 2);
    assert_eq!(recovered.skipped, 1);
    projection::check_agrees(&store, &[p], |batch, unit| {
        let projector = projector.clone();
        async move { projector.balance(batch, unit).await.expect("balance") }
    })
    .await;
    pool.close().await;
    db.finish().await;
}
