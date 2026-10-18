//! The ledger's tests: its tables exist after the migrations, and one
//! property test per object type on the in-memory store, to un-ignore once
//! the snapshot has its fields and the desired states their positions.

use basable_testkit::TestDb;

#[tokio::test]
async fn ledger_tables_exist() {
    let Some(db) = TestDb::from_env().await else { return };
    // pg_tables, not information_schema: the latter shows only what the
    // querying role has privileges on, and the ledger's tables belong to
    // the nanoservice role, not the migrator.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_tables \
         WHERE schemaname = 'nano_accounting' AND tablename = 'journal')",
    )
    .fetch_one(db.migrator())
    .await
    .unwrap();
    assert!(exists, "the ledger's journal must exist in nano_accounting after the migrations");
}

#[tokio::test]
#[ignore = "implement first"]
async fn order_books_through_every_state() {
    // Build `basable_ledger::Ledger::builder(basable_ledger::InMemoryStore::new())
    //     .register::<messages::OrderSnapshot, accounting::ledger::order::OrderAccounting>().build()`,
    // book a sample snapshot through each allowed change with a rising
    // version, assert every outcome is `Outcome::Booked`, and assert the
    // object's positions (`ledger.balances(process)`) equal the desired
    // state of its last snapshot. A state whose desired state does not
    // balance is a `Rejected` outcome here, before it is one in production.
    todo!("book a sample order through its states on the in-memory store");
}

#[tokio::test]
#[ignore = "implement first"]
async fn payment_books_through_every_state() {
    // Build `basable_ledger::Ledger::builder(basable_ledger::InMemoryStore::new())
    //     .register::<messages::PaymentSnapshot, accounting::ledger::payment::PaymentAccounting>().build()`,
    // book a sample snapshot through each allowed change with a rising
    // version, assert every outcome is `Outcome::Booked`, and assert the
    // object's positions (`ledger.balances(process)`) equal the desired
    // state of its last snapshot. A state whose desired state does not
    // balance is a `Rejected` outcome here, before it is one in production.
    todo!("book a sample payment through its states on the in-memory store");
}
