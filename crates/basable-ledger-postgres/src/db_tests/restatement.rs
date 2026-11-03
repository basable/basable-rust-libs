//! The startup restatement over Postgres: the worklist comes from the
//! version index, the per-process dry run books nothing, the run restates
//! every process behind, the next run finds nothing, and a booking by a
//! replica on the older rules is taken again by the next run.

use basable_ledger::{Ledger, Outcome, Request, RestatementReport, Store, UnitKey};
use basable_ledger_testkit::fixture::{
    Currency, Invoice, InvoiceLedger, InvoiceLedgerV2, InvoiceState, Payment, PaymentLedger,
    new_invoice,
};

use super::{Orders, db};
use crate::PgStore;

#[tokio::test]
async fn the_startup_restatement_restates_on_postgres() {
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
    // The dry run of one process: VAT moves from the month to the quarter.
    let preview = v2
        .preview_restatement(invoices[0].process_id)
        .await
        .expect("preview");
    let month = v1
        .batch_key(&invoices[0].vat_batch())
        .await
        .expect("find")
        .expect("a row");
    let quarter = InvoiceLedgerV2::vat_batch(&invoices[0]);
    assert_eq!(preview.deltas, vec![(month, UnitKey(978), -210)]);
    assert_eq!(
        preview.new_batches,
        vec![(quarter.clone(), UnitKey(978), 210)]
    );
    assert_eq!(
        v2.batch_key(&quarter).await.expect("find"),
        None,
        "the dry run created no row"
    );
    assert_eq!(
        store
            .read(invoices[0].process_id)
            .await
            .expect("read")
            .len(),
        1,
        "the dry run booked nothing"
    );

    let report = v2.restate_behind().await.expect("restate");
    assert_eq!(
        report,
        RestatementReport {
            processes: 4,
            objects: 4,
            lines: 8,
        }
    );
    assert_eq!(
        v2.restate_behind().await.expect("restate"),
        RestatementReport::default()
    );
    assert!(
        store
            .processes_behind("invoice", 2, 10)
            .await
            .expect("behind")
            .is_empty()
    );
    assert!(
        store
            .processes_behind("payment", 9, 10)
            .await
            .expect("behind")
            .is_empty(),
        "no payment was booked, so no payment row"
    );
    for invoice in &invoices {
        let journals = store.read(invoice.process_id).await.expect("read");
        assert_eq!(journals.len(), 2);
        let b = v2.balances(invoice.process_id).await.expect("balances");
        assert_eq!(
            b.of(invoice.id)
                .amount(&InvoiceLedgerV2::vat_batch(invoice), Currency::Eur),
            invoice.vat
        );
    }

    // The previous deployment's last replica books a restated process under
    // its rules: accepted, booked its way, and indexed at its version, so the
    // new deployment's next pass takes the process again.
    let paid = invoices[0].next(InvoiceState::Paid, 1_210);
    assert_eq!(
        v1.book(Request::new().with(paid.clone()))
            .await
            .expect("book"),
        vec![Outcome::Booked]
    );
    assert_eq!(
        store
            .processes_behind("invoice", 2, 10)
            .await
            .expect("behind"),
        vec![paid.process_id]
    );
    let b = v1.balances(paid.process_id).await.expect("balances");
    assert_eq!(
        b.of(paid.id).amount(&paid.vat_batch(), Currency::Eur),
        210,
        "the old replica put the VAT back in the month"
    );
    let report = v2.restate_behind().await.expect("restate");
    assert_eq!(report.processes, 1);
    let b = v2.balances(paid.process_id).await.expect("balances");
    assert_eq!(b.of(paid.id).amount_by_key(month, Currency::Eur), 0);
    assert_eq!(
        b.of(paid.id)
            .amount(&InvoiceLedgerV2::vat_batch(&paid), Currency::Eur),
        210
    );
    pool.close().await;
    db.finish().await;
}
