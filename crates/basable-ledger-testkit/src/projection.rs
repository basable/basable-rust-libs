//! A check any balance projection must pass: for every process given, the
//! projection's balance per (batch key, unit) must equal the sum of the
//! journals' lines, which is what the engine itself computes. The framework
//! keeps no balance table; a user's trigger, a ClickHouse table fed by
//! change data capture, or a query over the lines index all answer to this.
//!
//! A balance of zero is a closed batch and the projection should hold no
//! row for it: a trigger deletes the row its upsert brought to zero, and a
//! ClickHouse `SummingMergeTree` drops a row whose summed columns are all
//! zero when it merges. The check asks the projection only for the nonzero
//! balances, so a projection that keeps zero rows passes too; removing them
//! is what keeps the table the size of the open items.

use std::collections::BTreeMap;
use std::future::Future;

use basable_ledger::{BatchKey, ProcessId, Store, UnitKey};

/// The lines of the given processes summed per (batch key, unit), zeros
/// dropped: what a projection must show.
pub async fn expected_balances<S: Store>(
    store: &S,
    processes: &[ProcessId],
) -> BTreeMap<(BatchKey, UnitKey), i128> {
    let mut sums = BTreeMap::new();
    for process in processes {
        let journals = store.read(*process).await.expect("read");
        for line in journals.iter().flat_map(|j| j.lines.iter()) {
            *sums.entry((line.batch, line.unit)).or_insert(0) += line.amount;
        }
    }
    sums.retain(|_, a| *a != 0);
    sums
}

/// Asserts the projection agrees with the journals on every (batch, unit)
/// the processes hold a nonzero balance in.
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
