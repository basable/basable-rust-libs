//! The startup restatement over the in-memory store: a rules bump, the run
//! that restates every process behind, the run after it that finds nothing,
//! and the replica of the previous deployment that may not book a restated
//! process under its older rules.

use basable_ledger::{LedgerError, Outcome, Request, RestatementReport};
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
            versions_recorded: 5,
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
async fn a_replica_on_the_older_rules_may_not_book_a_restated_process() {
    let v1 = Scenario::new();
    let issued = new_invoice(1_000, 210, Currency::Eur).next(InvoiceState::Issued, 0);
    v1.booked(issued.clone()).await;
    let untouched = new_invoice(2_000, 420, Currency::Eur).next(InvoiceState::Issued, 0);

    // The new deployment starts and restates while the old one still serves.
    let v2 = v1.upgraded();
    v2.ledger().restate_behind().await.expect("restate");

    // The old replica's booking under version 1 would move the VAT back to
    // the month batch: refused as a whole, nothing is written.
    let paid = issued.next(InvoiceState::Paid, 1_210);
    let err = v1
        .book_all(Request::new().with(paid.clone()))
        .await
        .expect_err("booked under older rules");
    assert!(
        matches!(
            &err,
            LedgerError::RulesBehind { process, type_name, recorded: 2, running: 1 }
                if *process == issued.process_id && type_name == "invoice"
        ),
        "{err}"
    );
    assert_eq!(
        v1.journals(issued.process_id).len(),
        2,
        "the restatement only"
    );
    // A process the new deployment has not reached yet books under the old
    // rules as before; the new one restates it when it gets there.
    assert_eq!(v1.book(untouched.clone()).await, Outcome::Booked);
    assert_eq!(v1.store().version(untouched.process_id, "invoice"), Some(1));
    // The new replica books the paid state, and its journal is the third.
    assert_eq!(v2.book(paid).await, Outcome::Booked);
    assert_eq!(v2.journals(issued.process_id).len(), 3);
}
