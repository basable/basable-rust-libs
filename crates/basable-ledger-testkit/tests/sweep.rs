//! The restatement sweep over the in-memory store: a rules bump, the dry
//! run, the pass that restates every process behind, and the second pass
//! that finds nothing.

use basable_ledger::{Request, UnitKey};
use basable_ledger_testkit::Scenario;
use basable_ledger_testkit::fixture::{Currency, InvoiceLedgerV2, InvoiceState, new_invoice};

#[tokio::test]
async fn a_sweep_restates_every_process_behind_and_then_finds_nothing() {
    let v1 = Scenario::new();
    let mut invoices = Vec::new();
    for i in 0..5 {
        let issued = new_invoice(1_000 * (i + 1), 210 * (i + 1), Currency::Eur)
            .next(InvoiceState::Issued, 0);
        v1.booked(issued.clone()).await;
        invoices.push(issued);
    }
    // One process already paid: still restated, still balanced.
    let paid = invoices[0].next(InvoiceState::Paid, 1_210);
    v1.booked(paid.clone()).await;

    let v2 = v1.upgraded();
    let preview = v2.ledger().preview_sweep(10, 2).await.expect("preview");
    assert_eq!(preview.processes, 5);
    assert_eq!(preview.objects, 5);
    assert!(!preview.remaining);
    assert_eq!(preview.samples.len(), 2);
    // VAT moves from the month batch to the quarter batch, 3 150 in total.
    let total: i128 = (1..=5).map(|i| 210 * i).sum();
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

    // A pass bounded below the worklist reports remaining work.
    let first = v2.ledger().sweep(2).await.expect("sweep");
    assert_eq!(first.processes, 2);
    assert!(first.remaining);
    let second = v2.ledger().sweep(10).await.expect("sweep");
    assert_eq!(second.processes, 3);
    assert_eq!(second.objects, 3);
    assert_eq!(second.lines, 6);
    assert!(!second.remaining);
    let third = v2.ledger().sweep(10).await.expect("sweep");
    assert_eq!(third, basable_ledger::SweepReport::default());

    for invoice in &invoices {
        assert_eq!(v2.store().version(invoice.process_id, "invoice"), Some(2));
        let b = v2.balances(invoice.process_id).await;
        assert_eq!(
            b.of(invoice.id).amount(&invoice.vat_batch(), Currency::Eur),
            0
        );
        assert_eq!(
            b.of(invoice.id)
                .amount(&InvoiceLedgerV2::vat_batch(invoice), Currency::Eur),
            invoice.vat
        );
    }
    // Bookings under the new rules continue from the restated positions.
    let partly = invoices[1].next(InvoiceState::PartiallyPaid, 500);
    assert_eq!(
        v2.book_all(Request::new().with(partly.clone()))
            .await
            .expect("book"),
        vec![basable_ledger::Outcome::Booked]
    );
    let b = v2.balances(partly.process_id).await;
    assert_eq!(
        b.of(partly.id)
            .amount(&partly.receivable_batch(), Currency::Eur),
        2_000 + 420 - 500
    );
}
