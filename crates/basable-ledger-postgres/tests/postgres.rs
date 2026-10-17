//! The Postgres store against a real database: the conformance suite, the
//! invoice fixture through the engine, a rules bump restating, and the two
//! commit ambiguities the wire cannot tell apart.

use basable_db::{Nanoservice, Stateful};
use basable_ledger::{InMemoryStore, Ledger, LedgerError, Outcome, Request, UnitKey};
use basable_ledger_postgres::{PgStore, SCHEMA_PLACEHOLDER, migrations};
use basable_ledger_testkit::conformance;
use basable_ledger_testkit::fixture::{
    Currency, Invoice, InvoiceLedger, InvoiceLedgerV2, InvoiceState, Payment, PaymentLedger,
    PaymentState, new_invoice, new_payment,
};
use basable_testkit::{CommitFault, CommitFaultProxy, TestDb, runfile};
use sqlx::postgres::PgConnectOptions;

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
    let read = basable_ledger::Store::read(&store, p).await.expect("read");
    assert_eq!(read.sequence(), 3);
    assert_eq!(read.versions.get("invoice"), Some(&1));
    assert_eq!(read.versions.get("payment"), Some(&1));
    // The batch sum spans the whole process.
    let key = read
        .batches
        .values()
        .find(|b| b.id == paid.cash_batch())
        .expect("cash batch")
        .key;
    assert_eq!(
        basable_ledger::Store::batch_sum(&store, key)
            .await
            .expect("sum"),
        vec![(UnitKey(978), 24_200)]
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
    let read = basable_ledger::Store::read(&store, p).await.expect("read");
    assert_eq!(read.versions.get("invoice"), Some(&2));
    assert!(read.journals[1].snapshot_diffs.is_empty());
    db.finish().await;
}

/// A commit whose acknowledgement is lost: the producer's retry answers
/// `AlreadyBooked` when it landed and `Booked` when it did not. Nothing is
/// booked twice either way.
#[tokio::test]
async fn a_lost_commit_acknowledgement_is_repaired_by_the_retry() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    for mode in [CommitFault::Applied, CommitFault::RolledBack] {
        let (proxy, options): (CommitFaultProxy, PgConnectOptions) =
            CommitFaultProxy::for_options(db.app_options())
                .await
                .expect("proxy");
        let pool =
            basable_db::NanoPool::<Orders>::connect(options, basable_db::PoolConfig::default())
                .await
                .expect("pool through the proxy");
        let faulty = ledger(PgStore::new(pool.pool().clone()));
        let clean = ledger(store(&db).await);
        let draft = new_invoice(500, 105, Currency::Usd);
        let issued = draft.next(InvoiceState::Issued, 0);
        proxy.arm(mode);
        let first = faulty.book(Request::new().with(issued.clone())).await;
        assert!(
            matches!(first, Err(LedgerError::Store(_))),
            "{mode:?}: the faulted commit surfaces as a store error, got {first:?}"
        );
        let retry = clean
            .book(Request::new().with(issued.clone()))
            .await
            .expect("retry");
        let expected = match mode {
            CommitFault::Applied => Outcome::AlreadyBooked,
            CommitFault::RolledBack => Outcome::Booked,
        };
        assert_eq!(retry, vec![expected], "{mode:?}");
        let read = basable_ledger::Store::read(clean.store(), issued.process_id)
            .await
            .expect("read");
        assert_eq!(read.sequence(), 1, "{mode:?}: exactly one journal");
        pool.close().await;
    }
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
