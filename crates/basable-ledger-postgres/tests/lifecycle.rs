//! Close, claim, consume, the epoch roll a consumed batch causes in the
//! engine, and the row lock that keeps a line from landing after a close.

use std::time::Duration;

use basable_db::{Nanoservice, Stateful};
use basable_ledger::{BatchStatus, Ledger, Outcome, Request, Store, UnitKey};
use basable_ledger_postgres::{BatchLifecycle, LifecycleError, PgStore, SCHEMA_PLACEHOLDER};
use basable_ledger_testkit::fixture::{
    Currency, Invoice, InvoiceLedger, InvoiceState, Payment, PaymentLedger, new_invoice,
};
use basable_testkit::{TestDb, runfile};
use sqlx::Connection;

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
    let ms = basable_ledger_postgres::migrations().expect("parses");
    basable_db::migrate::apply(db.migrator(), &ms, &[(SCHEMA_PLACEHOLDER, "nano_orders")])
        .await
        .expect("ledger tables");
    Some(db)
}

fn ledger(store: PgStore) -> Ledger<PgStore> {
    Ledger::builder(store)
        .register::<Invoice, InvoiceLedger>()
        .register::<Payment, PaymentLedger>()
        .build()
}

#[tokio::test]
async fn a_batch_is_closed_claimed_and_consumed_and_the_engine_rolls_an_epoch() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let pool = db.nano_pool::<Orders>().await;
    let store = PgStore::new(pool.pool().clone());
    let lifecycle = BatchLifecycle::new(pool.pool().clone());
    let ledger = ledger(store.clone());
    let a = new_invoice(10_000, 2_100, Currency::Eur).next(InvoiceState::Issued, 0);
    let b = new_invoice(5_000, 1_050, Currency::Eur).next(InvoiceState::Issued, 0);
    assert_eq!(
        ledger
            .book(Request::new().with(a.clone()).with(b.clone()))
            .await
            .expect("book"),
        vec![Outcome::Booked, Outcome::Booked]
    );
    // Both invoices share the month's revenue batch.
    let revenue = store
        .resolve(std::slice::from_ref(&a.revenue_batch()))
        .await
        .expect("resolve")
        .remove(0)
        .expect("revenue batch");
    assert_eq!(
        lifecycle.status(revenue.key).await.expect("status"),
        BatchStatus::Open
    );

    // A claim on an open batch is refused; close first.
    assert!(matches!(
        lifecycle.claim(revenue.key).await,
        Err(LifecycleError::WrongStatus {
            needed: BatchStatus::Closed,
            ..
        })
    ));
    lifecycle.close(revenue.key).await.expect("close");
    lifecycle
        .close(revenue.key)
        .await
        .expect("closing twice is a no-op");
    assert_eq!(
        lifecycle.status(revenue.key).await.expect("status"),
        BatchStatus::Closed
    );
    let claim = lifecycle.claim(revenue.key).await.expect("claim");
    assert_eq!(claim.amounts, vec![(UnitKey(978), 15_000)]);
    assert_eq!(
        lifecycle.status(revenue.key).await.expect("status"),
        BatchStatus::Claimed
    );
    // Claiming again reads what was recorded.
    assert_eq!(lifecycle.claim(revenue.key).await.expect("claim"), claim);
    lifecycle.consume(revenue.key).await.expect("consume");
    assert_eq!(
        lifecycle.status(revenue.key).await.expect("status"),
        BatchStatus::Consumed
    );

    // A correction to invoice a now rolls the revenue batch to epoch 2.
    let mut corrected = a.next(InvoiceState::Issued, 0);
    corrected.net = 9_000;
    corrected.vat = 1_890;
    assert_eq!(
        ledger
            .book(Request::new().with(corrected.clone()))
            .await
            .expect("book"),
        vec![Outcome::Booked]
    );
    let rows: Vec<(i32, i16)> =
        sqlx::query_as("SELECT epoch, status FROM batch WHERE batch_id = $1 ORDER BY epoch")
            .bind(a.revenue_batch().as_str())
            .fetch_all(pool.pool())
            .await
            .expect("rows");
    assert_eq!(rows, vec![(1, 4), (2, 1)]);
    // The consumed epoch kept its 15 000; the new epoch holds the minus 1 000.
    assert_eq!(
        store.batch_sum(revenue.key).await.expect("sum"),
        vec![(UnitKey(978), 15_000)]
    );
    let epoch2 = store
        .resolve(std::slice::from_ref(&a.revenue_batch()))
        .await
        .expect("resolve")
        .remove(0)
        .expect("row");
    assert_eq!(epoch2.epoch, 2);
    assert_eq!(
        store.batch_sum(epoch2.key).await.expect("sum"),
        vec![(UnitKey(978), -1_000)]
    );
    pool.close().await;
    db.finish().await;
}

/// A commit holds its batches `FOR SHARE` until it commits, so a close
/// started meanwhile waits for it; and a booking that resolved a batch as
/// open but finds it closed at commit rolls to the next epoch instead of
/// landing in the closed one.
#[tokio::test]
async fn nothing_lands_in_a_batch_after_its_close() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let pool = db.nano_pool::<Orders>().await;
    let store = PgStore::new(pool.pool().clone());
    let lifecycle = BatchLifecycle::new(pool.pool().clone());
    let ledger = ledger(store.clone());
    let a = new_invoice(100, 21, Currency::Eur).next(InvoiceState::Issued, 0);
    ledger
        .book(Request::new().with(a.clone()))
        .await
        .expect("book");
    let vat = store
        .resolve(std::slice::from_ref(&a.vat_batch()))
        .await
        .expect("resolve")
        .remove(0)
        .expect("vat batch");

    // A writer in flight: the same lock a commit takes, held open.
    let mut writer = pool.pool().acquire().await.expect("connection");
    let mut tx = writer.begin().await.expect("begin");
    sqlx::query("SELECT status FROM batch WHERE batch_key = $1 FOR SHARE")
        .bind(vat.key.0)
        .execute(&mut *tx)
        .await
        .expect("share lock");
    let closing = {
        let lifecycle = lifecycle.clone();
        tokio::spawn(async move { lifecycle.close(vat.key).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!closing.is_finished(), "the close waits for the writer");
    assert_eq!(
        lifecycle.status(vat.key).await.expect("status"),
        BatchStatus::Open
    );
    tx.commit().await.expect("writer commits");
    closing
        .await
        .expect("task")
        .expect("the close goes through once the writer is done");
    assert_eq!(
        lifecycle.status(vat.key).await.expect("status"),
        BatchStatus::Closed
    );

    // A booking into the closed batch rolls to epoch 2: the status check at
    // commit refuses the closed one, the engine reads again and resolves the
    // next epoch.
    let mut corrected = a.next(InvoiceState::Issued, 0);
    corrected.vat = 42;
    corrected.net = 200;
    assert_eq!(
        ledger
            .book(Request::new().with(corrected.clone()))
            .await
            .expect("book"),
        vec![Outcome::Booked]
    );
    let rows: Vec<(i32, i16)> =
        sqlx::query_as("SELECT epoch, status FROM batch WHERE batch_id = $1 ORDER BY epoch")
            .bind(a.vat_batch().as_str())
            .fetch_all(pool.pool())
            .await
            .expect("rows");
    assert_eq!(rows, vec![(1, 2), (2, 1)]);
    assert_eq!(
        store.batch_sum(vat.key).await.expect("sum"),
        vec![(UnitKey(978), 21)]
    );
    let claim = lifecycle.claim(vat.key).await.expect("claim");
    assert_eq!(claim.amounts, vec![(UnitKey(978), 21)]);
    pool.close().await;
    db.finish().await;
}
