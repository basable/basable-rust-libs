//! The store conformance suite against the in-memory store.

use basable_ledger::InMemoryStore;
use basable_ledger_testkit::conformance;

#[tokio::test]
async fn the_in_memory_store_conforms() {
    conformance::run_all(&InMemoryStore::new()).await;
}
