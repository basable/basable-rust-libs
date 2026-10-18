//! The startup restatement over the in-memory store: a rules bump, the run
//! that restates every process behind, the run after it that finds nothing,
//! and the replica of the previous deployment whose booking under its older
//! rules is taken again by the next run.

use basable_ledger::{Outcome, Request, RestatementReport};
use basable_ledger_testkit::Scenario;
use basable_ledger_testkit::fixture::{Currency, InvoiceLedgerV2, InvoiceState, new_invoice};

#[tokio::test]
async fn a_startup_restatement_restates_every_process_behind_and_then_finds_nothing() {
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
    let report = v2.ledger().restate_behind().await.expect("restate");
    assert_eq!(
        report,
        RestatementReport {
            processes: 5,
            objects: 5,
            lines: 10,
        }
    );
    // Nothing is behind any more.
    assert_eq!(
        v2.ledger().restate_behind().await.expect("restate"),
        RestatementReport::default()
    );

    for invoice in &invoices {
        assert_eq!(v2.store().version(invoice.process_id, "invoice"), Some(2));
        // The invoice is the only type booked, so the only version row.
        assert_eq!(v2.store().version(invoice.process_id, "payment"), None);
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
        vec![Outcome::Booked]
    );
    let b = v2.balances(partly.process_id).await;
    assert_eq!(
        b.of(partly.id)
            .amount(&partly.receivable_batch(), Currency::Eur),
        2_000 + 420 - 500
    );
}

#[tokio::test]
async fn a_booking_by_a_replica_on_the_older_rules_converges_at_the_next_run() {
    let v1 = Scenario::new();
    let issued = new_invoice(1_000, 210, Currency::Eur).next(InvoiceState::Issued, 0);
    v1.booked(issued.clone()).await;
    let untouched = new_invoice(2_000, 420, Currency::Eur).next(InvoiceState::Issued, 0);

    // The new deployment starts and restates while the old one still serves.
    let v2 = v1.upgraded();
    v2.ledger().restate_behind().await.expect("restate");
    assert_eq!(v2.store().version(issued.process_id, "invoice"), Some(2));

    // The old replica books the paid state under version 1: accepted, and
    // under its rules the VAT goes back to the month batch, so the process
    // is indexed at version 1 again.
    let paid = issued.next(InvoiceState::Paid, 1_210);
    assert_eq!(v1.book(paid.clone()).await, Outcome::Booked);
    assert_eq!(v1.store().version(issued.process_id, "invoice"), Some(1));
    let b = v1.balances(issued.process_id).await;
    assert_eq!(b.of(paid.id).amount(&paid.vat_batch(), Currency::Eur), 210);
    assert_eq!(
        b.of(paid.id)
            .amount(&InvoiceLedgerV2::vat_batch(&paid), Currency::Eur),
        0
    );
    // A process the new deployment has not reached yet books under the old
    // rules as before.
    assert_eq!(v1.book(untouched.clone()).await, Outcome::Booked);
    assert_eq!(v1.store().version(untouched.process_id, "invoice"), Some(1));

    // The new deployment's next pass takes both and the book converges.
    let report = v2.ledger().restate_behind().await.expect("restate");
    assert_eq!(report.processes, 2);
    for (process, object, vat) in [
        (issued.process_id, paid.id, 210),
        (untouched.process_id, untouched.id, 420),
    ] {
        assert_eq!(v2.store().version(process, "invoice"), Some(2));
        let b = v2.balances(process).await;
        assert_eq!(
            b.of(object).amount(&untouched.vat_batch(), Currency::Eur),
            0
        );
        assert_eq!(
            b.of(object)
                .amount(&InvoiceLedgerV2::vat_batch(&untouched), Currency::Eur),
            vat
        );
    }
    assert_eq!(v2.journals(issued.process_id).len(), 4);
}
