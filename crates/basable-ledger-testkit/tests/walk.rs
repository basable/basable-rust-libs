//! Random histories of invoices: every journal balances, the positions
//! after any history equal the final state's desired state, and a replay
//! books nothing.

use basable_ledger_testkit::walk::check_history;

#[tokio::test]
async fn random_histories_are_path_independent() {
    let mut journals = 0;
    for seed in 1..=64u64 {
        journals += check_history(seed, 12).await;
    }
    assert!(
        journals > 64,
        "the walks should book more than one journal each"
    );
}

#[tokio::test]
async fn long_histories_too() {
    for seed in 1000..1008u64 {
        check_history(seed, 60).await;
    }
}
