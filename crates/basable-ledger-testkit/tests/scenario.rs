//! The invoice lifecycle through the engine: what books, what is refused,
//! what a journal holds, how a batch closes, and what a rules bump restates.

use basable_ledger::{BatchAccount, BatchType, Outcome, Request, UnitKey};
use basable_ledger_testkit::Scenario;
use basable_ledger_testkit::fixture::{
    Account, Currency, InvoiceLedgerV2, InvoiceState, PLATFORM_LEDGER, PaymentState, new_invoice,
    new_payment, new_vat_return,
};

#[tokio::test]
async fn an_issued_invoice_holds_receivable_revenue_and_vat() {
    let s = Scenario::new();
    let draft = new_invoice(10_000, 2_100, Currency::Eur);
    let p = draft.process_id;
    s.booked(draft.clone()).await;
    // Draft holds nothing but is booked: a journal with a diff and no lines.
    let journals = s.journals(p);
    assert_eq!(journals.len(), 1);
    assert!(journals[0].lines.is_empty());
    assert_eq!(journals[0].snapshot_diffs.len(), 1);
    assert!(journals[0].snapshot_diffs[0].full.is_some());

    let issued = draft.next(InvoiceState::Issued, 0);
    s.booked(issued.clone()).await;
    let positions = s.positions(p, issued.id).await;
    let eur = UnitKey(978);
    let mut expected = vec![
        (issued.points_batch().batch_key(), UnitKey(1), 100),
        (issued.receivable_batch().batch_key(), eur, 12_100),
        (issued.revenue_batch().batch_key(), eur, 10_000),
        (issued.vat_batch().batch_key(), eur, 2_100),
    ];
    expected.sort();
    assert_eq!(positions, expected);
    let journals = s.journals(p);
    assert_eq!(journals.len(), 2);
    let second = &journals[1];
    assert_eq!(second.sequence, 2);
    assert_eq!(second.lines.len(), 4);
    assert!(second.snapshot_diffs[0].patch.is_some());
    // Every line carries its batch's ledger and type: the receivable line is
    // a debit in ledger 1, the VAT line a credit.
    let receivable = second
        .lines
        .iter()
        .find(|l| l.batch.key == issued.receivable_batch().batch_key())
        .expect("receivable line");
    assert_eq!(receivable.batch.ledger, PLATFORM_LEDGER);
    assert_eq!(receivable.batch.batch_type, BatchType::Debit);
    let vat = second
        .lines
        .iter()
        .find(|l| l.batch.key == issued.vat_batch().batch_key())
        .expect("vat line");
    assert_eq!(vat.batch.batch_type, BatchType::Credit);
}

#[tokio::test]
async fn a_partial_payment_moves_receivable_to_cash() {
    let s = Scenario::new();
    let draft = new_invoice(10_000, 2_100, Currency::Eur);
    let p = draft.process_id;
    let issued = draft.next(InvoiceState::Issued, 0);
    s.booked(issued.clone()).await;
    let partly = issued.next(InvoiceState::PartiallyPaid, 5_000);
    s.booked(partly.clone()).await;
    let b = s.balances(p).await;
    let eur = Currency::Eur;
    assert_eq!(
        b.of(partly.id).amount(&partly.receivable_batch(), eur),
        7_100
    );
    assert_eq!(b.of(partly.id).amount(&partly.cash_batch(), eur), 5_000);
    // The delta journal moved exactly 5 000 between the two batches.
    let last = s.journals(p).last().cloned().expect("journal");
    assert_eq!(last.lines.len(), 2);
    let paid = partly.next(InvoiceState::Paid, 12_100);
    s.booked(paid.clone()).await;
    let b = s.balances(p).await;
    assert_eq!(b.of(paid.id).amount(&paid.receivable_batch(), eur), 0);
    assert_eq!(b.of(paid.id).amount(&paid.cash_batch(), eur), 12_100);
}

#[tokio::test]
async fn duplicates_and_stale_snapshots_book_nothing() {
    let s = Scenario::new();
    let draft = new_invoice(100, 21, Currency::Eur);
    let issued = draft.next(InvoiceState::Issued, 0);
    s.booked(issued.clone()).await;
    assert_eq!(s.book(issued.clone()).await, Outcome::AlreadyBooked);
    assert_eq!(s.book(draft).await, Outcome::Stale);
    assert_eq!(s.journals(issued.process_id).len(), 1);
}

#[tokio::test]
async fn a_disallowed_change_is_rejected_with_the_reason() {
    let s = Scenario::new();
    let draft = new_invoice(100, 21, Currency::Eur);
    s.booked(draft.clone()).await;
    let paid = draft.next(InvoiceState::Paid, 121);
    match s.book(paid).await {
        Outcome::Rejected(r) => assert!(r.message().contains("may not move from Draft to Paid")),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn amounts_are_frozen_in_a_state_that_does_not_list_itself() {
    let s = Scenario::new();
    let draft = new_invoice(100, 21, Currency::Eur);
    let issued = draft.next(InvoiceState::Issued, 0);
    s.booked(issued.clone()).await;
    let paid = issued.next(InvoiceState::Paid, 121);
    s.booked(paid.clone()).await;
    // Paid lists no next state at all, so a changed amount while Paid rejects.
    let mut changed = paid.next(InvoiceState::Paid, 121);
    changed.net = 200;
    changed.paid = 242;
    match s.book(changed).await {
        Outcome::Rejected(r) => assert!(r.message().contains("allows no amount change")),
        other => panic!("expected a rejection, got {other:?}"),
    }
    // Issued lists itself: an amount change while Issued books an adjustment.
    let draft2 = new_invoice(100, 21, Currency::Eur);
    let issued = draft2.next(InvoiceState::Issued, 0);
    s.booked(issued.clone()).await;
    let mut corrected = issued.next(InvoiceState::Issued, 0);
    corrected.net = 150;
    corrected.vat = 31;
    s.booked(corrected.clone()).await;
    let b = s.balances(corrected.process_id).await;
    assert_eq!(
        b.of(corrected.id)
            .amount(&corrected.receivable_batch(), Currency::Eur),
        181
    );
}

#[tokio::test]
async fn a_desired_state_that_fails_the_balance_check_rejects() {
    let s = Scenario::new();
    let draft = new_invoice(100, 21, Currency::Eur);
    let issued = draft.next(InvoiceState::Issued, 0);
    s.booked(issued.clone()).await;
    let half_paid = issued.next(InvoiceState::Paid, 60);
    match s.book(half_paid).await {
        Outcome::Rejected(r) => assert!(r.message().contains("paid without full payment")),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn a_payment_clears_the_invoices_receivable_batch_across_objects() {
    let s = Scenario::new();
    let draft = new_invoice(10_000, 2_100, Currency::Eur);
    let p = draft.process_id;
    let issued = draft.next(InvoiceState::Issued, 0);
    let payment = new_payment(&issued, 12_100);
    // One call, two types, one journal.
    let outcomes = s
        .book_all(Request::new().with(issued.clone()).with(payment.clone()))
        .await
        .expect("book");
    assert_eq!(outcomes, vec![Outcome::Booked, Outcome::Booked]);
    assert_eq!(s.journals(p).len(), 1);
    let settled = payment.next(PaymentState::Settled);
    s.booked(settled.clone()).await;
    let b = s.balances(p).await;
    let eur = Currency::Eur;
    // The payment holds minus 12 100 on the receivable batch; the batch nets to zero.
    assert_eq!(
        b.of(settled.id).amount(&issued.receivable_batch(), eur),
        -12_100
    );
    assert_eq!(b.batch(&issued.receivable_batch(), eur), 0);
    assert_eq!(b.batch(&issued.cash_batch(), eur), 12_100);
    // A second payment would overdraw the receivable: the process-wide check refuses.
    let second = new_payment(&issued, 1).next(PaymentState::Settled);
    match s.book(second).await {
        Outcome::Rejected(r) => assert!(r.message().contains("exceeds the open receivable")),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn a_rejected_object_rejects_its_whole_process_in_the_call() {
    let s = Scenario::new();
    let draft = new_invoice(100, 21, Currency::Eur);
    let issued = draft.next(InvoiceState::Issued, 0);
    let bad_payment = new_payment(&issued, 1_000).next(PaymentState::Settled);
    let other = new_invoice(5, 1, Currency::Usd);
    let outcomes = s
        .book_all(
            Request::new()
                .with(issued.clone())
                .with(bad_payment)
                .with(other.clone()),
        )
        .await
        .expect("book");
    assert!(matches!(outcomes[0], Outcome::Rejected(_)));
    assert!(matches!(outcomes[1], Outcome::Rejected(_)));
    assert_eq!(
        outcomes[2],
        Outcome::Booked,
        "another process is unaffected"
    );
    assert!(s.journals(issued.process_id).is_empty());
}

#[tokio::test]
async fn a_rules_bump_restates_the_process_by_appending_a_journal() {
    let v1 = Scenario::new();
    let draft = new_invoice(10_000, 2_100, Currency::Eur);
    let p = draft.process_id;
    let issued = draft.next(InvoiceState::Issued, 0);
    v1.booked(issued.clone()).await;
    assert_eq!(v1.store().version(p, "invoice"), Some(1));

    let v2 = v1.upgraded();
    let preview = v2.ledger().preview_restatement(p).await.expect("preview");
    assert_eq!(preview.objects, 1);
    let quarter = InvoiceLedgerV2::vat_batch(&issued);
    // Deltas come in batch key order.
    let mut expected = vec![
        (issued.vat_batch().batch_key(), UnitKey(978), -2_100),
        (quarter.batch_key(), UnitKey(978), 2_100),
    ];
    expected.sort();
    assert_eq!(preview.deltas, expected);
    let restated = v2.ledger().restate(p).await.expect("restate");
    assert_eq!(restated.objects, 1);
    assert_eq!(restated.lines, 2);
    assert_eq!(v2.store().version(p, "invoice"), Some(2));
    let journals = v2.journals(p);
    assert_eq!(journals.len(), 2);
    assert!(
        journals[1].snapshot_diffs.is_empty(),
        "a restatement has lines and no diffs"
    );
    let b = v2.balances(p).await;
    assert_eq!(
        b.of(issued.id).amount(&issued.vat_batch(), Currency::Eur),
        0
    );
    assert_eq!(b.of(issued.id).amount(&quarter, Currency::Eur), 2_100);
    // Restating again does nothing.
    let again = v2.ledger().restate(p).await.expect("restate");
    assert_eq!(again.lines, 0);
    assert_eq!(v2.journals(p).len(), 2);
    // And a booking under the new code of a process still at version 1 restates first.
    let draft2 = new_invoice(100, 21, Currency::Eur);
    let issued2 = draft2.next(InvoiceState::Issued, 0);
    v1.booked(issued2.clone()).await;
    let paid2 = issued2.next(InvoiceState::Paid, 121);
    v2.booked(paid2.clone()).await;
    let journals = v2.journals(paid2.process_id);
    assert_eq!(journals.len(), 2);
    let b = v2.balances(paid2.process_id).await;
    assert_eq!(
        b.of(paid2.id)
            .amount(&InvoiceLedgerV2::vat_batch(&paid2), Currency::Eur),
        21
    );
    assert_eq!(b.of(paid2.id).amount(&paid2.vat_batch(), Currency::Eur), 0);
}

/// A batch closes by a booking that brings it to zero, not by a status. The
/// VAT return claims what the month's VAT batch holds; a correction landing
/// in the month afterwards leaves a remainder for the next return.
#[tokio::test]
async fn a_batch_is_closed_by_booking_its_balance_to_zero() {
    let s = Scenario::new();
    let mut issued = Vec::new();
    for (net, vat) in [(10_000, 2_100), (5_000, 1_050), (1_000, 210)] {
        let i = new_invoice(net, vat, Currency::Eur).next(InvoiceState::Issued, 0);
        s.booked(i.clone()).await;
        issued.push(i);
    }
    let vat = Account::Vat("2026-10".into());
    let tax_office = Account::TaxOffice("2026-10".into());
    let eur = UnitKey(978);
    // The producer reads the batch and files the return for what it holds.
    assert_eq!(s.batch_sum(&vat).await, vec![(eur, 3_360)]);
    s.booked(new_vat_return("2026-10", 3_360, Currency::Eur))
        .await;
    assert_eq!(s.batch_sum(&vat).await, vec![(eur, 0)], "closed");
    assert_eq!(s.batch_sum(&tax_office).await, vec![(eur, 3_360)]);
    // A late correction to an October invoice lands in October's batch.
    let mut corrected = issued[2].next(InvoiceState::Issued, 0);
    corrected.net = 500;
    corrected.vat = 105;
    s.booked(corrected).await;
    assert_eq!(s.batch_sum(&vat).await, vec![(eur, -105)], "a remainder");
    // The next return takes the remainder; the batch is at zero again.
    s.booked(new_vat_return("2026-10", -105, Currency::Eur))
        .await;
    assert_eq!(s.batch_sum(&vat).await, vec![(eur, 0)]);
    assert_eq!(s.batch_sum(&tax_office).await, vec![(eur, 3_255)]);
}

#[tokio::test]
async fn memo_batches_stay_outside_the_check() {
    let s = Scenario::new();
    let draft = new_invoice(10_000, 2_100, Currency::Eur);
    let issued = draft.next(InvoiceState::Issued, 0);
    s.booked(issued.clone()).await;
    let b = s.balances(issued.process_id).await;
    assert_eq!(
        b.of(issued.id).amount(&issued.points_batch(), UnitKey(1)),
        100
    );
    assert_eq!(
        b.batch_account(&issued.points_batch()).map(|a| a.1),
        Some(BatchType::Memo)
    );
}
