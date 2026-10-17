//! A check any balance projection can run: for every process given, the
//! projection's balance per (batch key, unit) must equal the sum of the
//! journals' lines, which is what the engine itself computes. The
//! projector of `basable-ledger-postgres` passes it; so should a trigger a
//! user adds, or a ClickHouse table.

use std::collections::BTreeMap;
use std::future::Future;

use basable_ledger::{BatchKey, ProcessId, Store, UnitKey};

/// The lines of the given processes summed per (batch key, unit): what a
/// projection must show.
pub async fn expected_balances<S: Store>(
    store: &S,
    processes: &[ProcessId],
) -> BTreeMap<(BatchKey, UnitKey), i128> {
    let mut sums = BTreeMap::new();
    for process in processes {
        let read = store.read(*process).await.expect("read");
        for line in read.journals.iter().flat_map(|j| j.lines.iter()) {
            *sums.entry((line.batch_key, line.unit)).or_insert(0) += line.amount;
        }
    }
    sums.retain(|_, a| *a != 0);
    sums
}

/// Asserts the projection agrees with the journals on every (batch, unit)
/// the processes touched.
pub async fn check_agrees<S, F, Fut>(store: &S, processes: &[ProcessId], projected: F)
where
    S: Store,
    F: Fn(BatchKey, UnitKey) -> Fut,
    Fut: Future<Output = i128>,
{
    let expected = expected_balances(store, processes).await;
    assert!(!expected.is_empty(), "the processes booked no lines");
    for ((batch, unit), amount) in expected {
        let got = projected(batch, unit).await;
        assert_eq!(
            got, amount,
            "batch {} unit {}: the projection shows {got}, the journals sum to {amount}",
            batch.0, unit.0
        );
    }
}
