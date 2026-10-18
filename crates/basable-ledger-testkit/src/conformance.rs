//! The store conformance suite: what every `Store` adapter must do, run
//! against the in-memory store here and against Postgres in its own crate.
//! Each check is a function so an adapter's test file can run them one by
//! one; `run_all` runs them in order.

use std::collections::BTreeMap;
use std::sync::Arc;

use basable_ledger::{
    Batch, BatchKey, BatchType, Commit, CommitOutcome, Journal, LedgerKey, Line, ObjectId,
    ProcessId, SnapshotDiff, Store, UnitKey,
};
use uuid::Uuid;

fn process() -> ProcessId {
    ProcessId(Uuid::now_v7())
}

fn object() -> ObjectId {
    ObjectId(Uuid::now_v7())
}

/// A batch key no other run of the suite has used.
fn unique_key() -> BatchKey {
    BatchKey(Uuid::new_v4().as_u64_pair().1 as i64)
}

fn debit(key: BatchKey) -> Batch {
    Batch {
        key,
        ledger: LedgerKey(1),
        batch_type: BatchType::Debit,
    }
}

fn line(batch: Batch, object: ObjectId, unit: u32, amount: i128) -> Line {
    Line {
        batch,
        object_id: object,
        unit: UnitKey(unit),
        amount,
    }
}

fn commit(process: ProcessId, after: i64, batch: Batch, object: ObjectId) -> Commit {
    Commit {
        process_id: process,
        after_sequence: after,
        lines: vec![
            line(batch, object, 978, 100),
            line(batch, object, 978, -100),
        ],
        snapshot_diffs: vec![SnapshotDiff {
            object_id: object,
            type_name: "conformance".into(),
            full: Some(serde_json::json!({"v": after + 1})),
            patch: None,
        }],
        rules_versions: BTreeMap::new(),
    }
}

/// A commit with no lines or diffs that moves the versions of some types:
/// the shape of a restatement that changed no position.
fn versions_commit(process: ProcessId, after: i64, versions: &[(&str, u32)]) -> Commit {
    Commit {
        process_id: process,
        after_sequence: after,
        lines: Vec::new(),
        snapshot_diffs: Vec::new(),
        rules_versions: versions
            .iter()
            .map(|(t, v)| ((*t).to_owned(), *v))
            .collect(),
    }
}

fn sequence(journals: &[Journal]) -> i64 {
    journals.last().map(|j| j.sequence).unwrap_or(0)
}

/// A process never booked reads as empty.
pub async fn reads_empty_process<S: Store>(store: &S) {
    let journals = store.read(process()).await.expect("read");
    assert!(journals.is_empty());
    assert_eq!(sequence(&journals), 0);
}

/// The first commit goes at sequence 1; the read returns the journal, its
/// lines with their batches, its diffs and the versions it moved.
pub async fn commits_and_reads_back<S: Store>(store: &S) {
    let p = process();
    let o = object();
    let batch = debit(unique_key());
    let mut c = commit(p, 0, batch, o);
    c.rules_versions.insert("conformance".into(), 1);
    assert_eq!(
        store.commit(c).await.expect("commit"),
        CommitOutcome::Committed
    );
    let journals = store.read(p).await.expect("read");
    assert_eq!(sequence(&journals), 1);
    assert_eq!(journals.len(), 1);
    let journal = &journals[0];
    assert_eq!(journal.process_id, p);
    assert_eq!(journal.sequence, 1);
    assert_eq!(
        journal.lines,
        vec![line(batch, o, 978, 100), line(batch, o, 978, -100)]
    );
    assert_eq!(journal.snapshot_diffs.len(), 1);
    assert_eq!(journal.snapshot_diffs[0].object_id, o);
    assert_eq!(journal.rules_versions.get("conformance"), Some(&1));
}

/// The primary key on (process, sequence) is the compare-and-set: a second
/// commit at the same sequence is `SequenceTaken`, and the next one goes in.
pub async fn sequence_is_the_compare_and_set<S: Store>(store: &S) {
    let p = process();
    let o = object();
    let batch = debit(unique_key());
    let c = commit(p, 0, batch, o);
    assert_eq!(
        store.commit(c.clone()).await.expect("commit"),
        CommitOutcome::Committed
    );
    assert_eq!(
        store.commit(c).await.expect("commit"),
        CommitOutcome::SequenceTaken
    );
    assert_eq!(
        store.commit(commit(p, 1, batch, o)).await.expect("commit"),
        CommitOutcome::Committed
    );
    let journals = store.read(p).await.expect("read");
    assert_eq!(sequence(&journals), 2);
    assert_eq!(
        journals.iter().map(|j| j.sequence).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

/// Under concurrency exactly one of the racing commits at one sequence lands.
pub async fn concurrent_commits_land_once<S: Store + Clone + 'static>(store: &S) {
    let p = process();
    let batch = debit(unique_key());
    assert_eq!(
        store
            .commit(commit(p, 0, batch, object()))
            .await
            .expect("commit"),
        CommitOutcome::Committed
    );
    let store = Arc::new(store.clone());
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .commit(commit(p, 1, batch, object()))
                .await
                .expect("commit")
        }));
    }
    let mut committed = 0;
    for t in tasks {
        if t.await.expect("task") == CommitOutcome::Committed {
            committed += 1;
        }
    }
    assert_eq!(committed, 1, "exactly one racer may take sequence 2");
    assert_eq!(sequence(&store.read(p).await.expect("read")), 2);
}

/// The sum of one batch's lines spans every process and every unit; a batch
/// nobody wrote into sums to nothing.
pub async fn sums_a_batch_across_processes<S: Store>(store: &S) {
    let batch = debit(unique_key());
    let other = debit(unique_key());
    assert!(store.batch_sum(batch.key).await.expect("sum").is_empty());
    let p1 = process();
    let p2 = process();
    let mut c1 = commit(p1, 0, batch, object());
    c1.lines = vec![
        line(batch, object(), 978, 70),
        line(other, object(), 978, 7),
    ];
    assert_eq!(
        store.commit(c1).await.expect("commit"),
        CommitOutcome::Committed
    );
    let mut c2 = commit(p2, 0, batch, object());
    c2.lines = vec![
        line(batch, object(), 978, -20),
        line(batch, object(), 840, 5),
    ];
    assert_eq!(
        store.commit(c2).await.expect("commit"),
        CommitOutcome::Committed
    );
    assert_eq!(
        store.batch_sum(batch.key).await.expect("sum"),
        vec![(UnitKey(840), 5), (UnitKey(978), 50)]
    );
    assert_eq!(
        store.batch_sum(other.key).await.expect("sum"),
        vec![(UnitKey(978), 7)]
    );
}

/// The versions a journal moves ride in the row and fold to the highest per
/// type; the index the commit maintains agrees and never moves backwards.
pub async fn records_versions<S: Store>(store: &S) {
    let p = process();
    let invoice = format!("invoice-{}", Uuid::now_v7());
    let payment = format!("payment-{}", Uuid::now_v7());
    for (after, versions) in [
        (0, vec![(invoice.as_str(), 2), (payment.as_str(), 1)]),
        (1, vec![(invoice.as_str(), 1)]),
    ] {
        assert_eq!(
            store
                .commit(versions_commit(p, after, &versions))
                .await
                .expect("commit"),
            CommitOutcome::Committed
        );
    }
    let journals = store.read(p).await.expect("read");
    let versions = Journal::versions_of(&journals);
    assert_eq!(versions.get(&invoice), Some(&2));
    assert_eq!(versions.get(&payment), Some(&1));
    assert_eq!(
        store
            .processes_behind(&invoice, 3, 10)
            .await
            .expect("behind"),
        vec![p],
        "the index holds the highest version"
    );
    assert!(
        store
            .processes_behind(&invoice, 2, 10)
            .await
            .expect("behind")
            .is_empty(),
        "a later lower version does not move the index back"
    );
    assert_eq!(
        store
            .commit(versions_commit(p, 2, &[(invoice.as_str(), 3)]))
            .await
            .expect("commit"),
        CommitOutcome::Committed
    );
    let journals = store.read(p).await.expect("read");
    assert_eq!(Journal::versions_of(&journals).get(&invoice), Some(&3));
    assert!(
        store
            .processes_behind(&invoice, 3, 10)
            .await
            .expect("behind")
            .is_empty()
    );
}

/// `processes_behind` lists the processes whose recorded version of a type
/// is below the running one, bounded by the limit.
pub async fn lists_processes_behind<S: Store>(store: &S) {
    let type_name = format!("conf-{}", Uuid::now_v7());
    let p1 = process();
    let p2 = process();
    let p3 = process();
    for (p, v) in [(p1, 1), (p2, 2), (p3, 1)] {
        assert_eq!(
            store
                .commit(versions_commit(p, 0, &[(type_name.as_str(), v)]))
                .await
                .expect("commit"),
            CommitOutcome::Committed
        );
    }
    let mut behind = store
        .processes_behind(&type_name, 2, 10)
        .await
        .expect("behind");
    behind.sort();
    let mut expected = vec![p1, p3];
    expected.sort();
    assert_eq!(behind, expected);
    assert_eq!(
        store
            .processes_behind(&type_name, 2, 1)
            .await
            .expect("behind")
            .len(),
        1
    );
    assert!(
        store
            .processes_behind(&type_name, 1, 10)
            .await
            .expect("behind")
            .is_empty()
    );
}

/// Every check, in order.
pub async fn run_all<S: Store + Clone + 'static>(store: &S) {
    reads_empty_process(store).await;
    commits_and_reads_back(store).await;
    sequence_is_the_compare_and_set(store).await;
    concurrent_commits_land_once(store).await;
    sums_a_batch_across_processes(store).await;
    records_versions(store).await;
    lists_processes_behind(store).await;
}
