//! The restatement sweep over Postgres: the worklist comes from the version
//! table's index, the dry run books nothing, the pass restates, the next
//! pass finds nothing.

use basable_db::{Nanoservice, Stateful};
use basable_ledger::{Ledger, Request, Store, SweepReport, UnitKey};
use basable_ledger_postgres::{PgStore, SCHEMA_PLACEHOLDER};
use basable_ledger_testkit::fixture::{
    Currency, Invoice, InvoiceLedger, InvoiceLedgerV2, InvoiceState, Payment, PaymentLedger,
    new_invoice,
};
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
    let ms = basable_ledger_postgres::migrations().expect("parses");
    basable_db::migrate::apply(db.migrator(), &ms, &[(SCHEMA_PLACEHOLDER, "nano_orders")])
        .await
        .expect("ledger tables");
    Some(db)
}

#[tokio::test]
async fn the_sweep_restates_on_postgres() {
    let Some(db) = db().await else {
        eprintln!("TEST_DATABASE_URL unset: skipping");
        return;
    };
    let pool = db.nano_pool::<Orders>().await;
    let store = PgStore::new(pool.pool().clone());
    let v1 = Ledger::builder(store.clone())
        .register::<Invoice, InvoiceLedger>()
        .register::<Payment, PaymentLedger>()
        .build();
    let mut invoices = Vec::new();
    for i in 0..4 {
        let issued = new_invoice(1_000 * (i + 1), 210 * (i + 1), Currency::Eur)
            .next(InvoiceState::Issued, 0);
        v1.book(Request::new().with(issued.clone()))
            .await
            .expect("book");
        invoices.push(issued);
    }
    let v2 = Ledger::builder(store.clone())
        .register::<Invoice, InvoiceLedgerV2>()
        .register::<Payment, PaymentLedger>()
        .build();
    let preview = v2.preview_sweep(10, 1).await.expect("preview");
    assert_eq!(preview.processes, 4);
    let total: i128 = (1..=4).map(|i| 210 * i).sum();
    assert_eq!(
        preview.deltas,
        vec![
            (invoices[0].vat_batch(), UnitKey(978), -total),
            (
                InvoiceLedgerV2::vat_batch(&invoices[0]),
                UnitKey(978),
                total
            ),
        ]
    );
    // The dry run booked nothing.
    for invoice in &invoices {
        assert_eq!(
            store
                .read(invoice.process_id)
                .await
                .expect("read")
                .sequence(),
            1
        );
    }
    let report = v2.sweep(10).await.expect("sweep");
    assert_eq!(report.processes, 4);
    assert_eq!(report.objects, 4);
    assert_eq!(report.lines, 8);
    assert!(!report.remaining);
    assert_eq!(v2.sweep(10).await.expect("sweep"), SweepReport::default());
    for invoice in &invoices {
        let read = store.read(invoice.process_id).await.expect("read");
        assert_eq!(read.sequence(), 2);
        assert_eq!(read.versions.get("invoice"), Some(&2));
        let b = v2.balances(invoice.process_id).await.expect("balances");
        assert_eq!(
            b.of(invoice.id)
                .amount(&InvoiceLedgerV2::vat_batch(invoice), Currency::Eur),
            invoice.vat
        );
    }
    pool.close().await;
    db.finish().await;
}
