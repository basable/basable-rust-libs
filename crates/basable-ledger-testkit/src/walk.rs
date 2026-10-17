//! Generator-driven properties: random histories of an invoice along its
//! allowed changes, with random amounts, booked through a scenario. After
//! any history an object's booked positions equal the desired state of its
//! final snapshot (path independence), every journal balances, and
//! replaying the history books nothing new. A small xorshift generator
//! keeps the walks deterministic per seed without a dependency.

use std::collections::BTreeMap;

use basable_ledger::{Accounting, BatchId, BatchType, Batches, Outcome, UnitKey};

use crate::fixture::{Currency, Invoice, InvoiceLedger, InvoiceState, new_invoice};
use crate::scenario::Scenario;

/// A deterministic pseudo-random source.
pub struct Rng(u64);

impl Rng {
    /// From a seed; zero is replaced, as xorshift needs.
    pub fn new(seed: u64) -> Self {
        Rng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// The next value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value below `n`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// One of a slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// A random history of one invoice: `steps` snapshots after the first,
/// each a legal change (or an amount change where allowed), with amounts
/// that keep the invoice's own checks satisfied.
pub fn random_history(rng: &mut Rng, steps: usize) -> Vec<Invoice> {
    let net = 1_000 + rng.below(1_000_000) as i128;
    let vat = net * 21 / 100;
    let currency = *rng.pick(&[Currency::Eur, Currency::Usd]);
    let mut history = vec![new_invoice(net, vat, currency)];
    for _ in 0..steps {
        let last = history.last().expect("one");
        let allowed = InvoiceLedger::changes(Some(last.state));
        if allowed.is_empty() {
            break;
        }
        let next_state = *rng.pick(allowed);
        let total = last.net + last.vat;
        let paid = match next_state {
            InvoiceState::PartiallyPaid => 1 + rng.below((total - 1).max(1) as u64) as i128,
            InvoiceState::Paid => total,
            InvoiceState::Issued | InvoiceState::Draft => 0,
            InvoiceState::Credited | InvoiceState::WrittenOff => last.paid,
        };
        let mut next = last.next(next_state, paid);
        // An amount change while staying in a state that allows it.
        if next_state == last.state && rng.below(2) == 0 {
            next.net = 1_000 + rng.below(1_000_000) as i128;
            next.vat = next.net * 21 / 100;
            if next.state == InvoiceState::PartiallyPaid {
                next.paid = 1 + rng.below((next.net + next.vat - 1).max(1) as u64) as i128;
            }
        }
        history.push(next);
    }
    history
}

/// The desired state of a snapshot as positions per (batch id, unit), the
/// way the engine compares it.
pub fn desired_positions<A: Accounting<Invoice>>(
    invoice: &Invoice,
) -> BTreeMap<(BatchId, UnitKey), i128> {
    let mut batches = Batches::new();
    let desired = A::desired_state(invoice.state, invoice, &mut batches);
    let mut map = BTreeMap::new();
    for p in desired.positions() {
        let id = batches.get(p.batch).id.clone();
        *map.entry((id, p.unit)).or_insert(0) += p.amount;
    }
    map.retain(|_, a| *a != 0);
    map
}

/// Books a random history through a fresh scenario and checks the
/// properties. Returns the number of journals written.
pub async fn check_history(seed: u64, steps: usize) -> usize {
    let mut rng = Rng::new(seed);
    let history = random_history(&mut rng, steps);
    let scenario = Scenario::new();
    let process = history[0].process_id;
    for snapshot in &history {
        let outcome = scenario.book(snapshot.clone()).await;
        assert_eq!(
            outcome,
            Outcome::Booked,
            "seed {seed}: snapshot v{} in {:?} should book",
            snapshot.version,
            snapshot.state
        );
    }
    let last = history.last().expect("one");
    let positions: BTreeMap<(BatchId, UnitKey), i128> = scenario
        .positions(process, last.id)
        .await
        .into_iter()
        .map(|(id, unit, a)| ((id, unit), a))
        .collect();
    assert_eq!(
        positions,
        desired_positions::<InvoiceLedger>(last),
        "seed {seed}: path independence after {} snapshots",
        history.len()
    );

    let journals = scenario.journals(process);
    for journal in &journals {
        let mut sums: BTreeMap<UnitKey, i128> = BTreeMap::new();
        for line in &journal.lines {
            let row = scenario.batch(line.batch_key).expect("batch row");
            match row.batch_type {
                BatchType::Debit => *sums.entry(line.unit).or_insert(0) += line.amount,
                BatchType::Credit => *sums.entry(line.unit).or_insert(0) -= line.amount,
                BatchType::Memo => {}
            }
        }
        assert!(
            sums.values().all(|s| *s == 0),
            "seed {seed}: journal {} does not balance: {sums:?}",
            journal.sequence
        );
    }

    // Replaying the history books nothing new.
    for snapshot in &history {
        let outcome = scenario.book(snapshot.clone()).await;
        let expected = if snapshot.version == last.version {
            Outcome::AlreadyBooked
        } else {
            Outcome::Stale
        };
        assert_eq!(
            outcome, expected,
            "seed {seed}: replay of v{}",
            snapshot.version
        );
    }
    assert_eq!(scenario.journals(process).len(), journals.len());
    journals.len()
}
