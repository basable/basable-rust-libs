//! The store conformance suite: what every `Store` adapter must do, run
//! against the in-memory store here and against Postgres in its own crate.
//! Each check is a function so an adapter's test file can run them one by
//! one; `run_all` runs them in order.

use std::collections::BTreeMap;
use std::sync::Arc;

use basable_ledger::{
    BatchKey, BatchType, Commit, CommitOutcome, Journal, LedgerBatch, LedgerKey, Line, ObjectId,
    ProcessId, SnapshotDiff, Store, UnitKey,
};
use uuid::Uuid;

use crate::fixture::Batch;

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

fn line(batch: BatchKey, object: ObjectId, unit: u32, amount: i128) -> Line {
    Line {
        batch,
        object_id: object,
        unit: UnitKey(unit),
        amount,
    }
}

fn commit(process: ProcessId, after: i64, batch: BatchKey, object: ObjectId) -> Commit {
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
        versions: BTreeMap::new(),
    }
}

/// A commit whose one line lands in a batch.
fn one_line(process: ProcessId, after: i64, batch: BatchKey, object: ObjectId) -> Commit {
    Commit {
        process_id: process,
        after_sequence: after,
        lines: vec![line(batch, object, 978, 100)],
        snapshot_diffs: Vec::new(),
        versions: BTreeMap::new(),
    }
}

/// A batch of one kind no other run of the suite has named.
fn named(batch_type: BatchType) -> Batch {
    Batch::Named {
        name: Uuid::now_v7().to_string(),
        ledger: LedgerKey(1),
        batch_type,
    }
}

fn versions(pairs: &[(&str, u32)]) -> BTreeMap<String, u32> {
    pairs.iter().map(|(t, v)| ((*t).to_owned(), *v)).collect()
}

/// A commit with no lines or diffs that carries the versions of some types.
fn versions_commit(process: ProcessId, after: i64, pairs: &[(&str, u32)]) -> Commit {
    Commit {
        process_id: process,
        after_sequence: after,
        lines: Vec::new(),
        snapshot_diffs: Vec::new(),
        versions: versions(pairs),
    }
}

fn sequence(journals: &[Journal]) -> i64 {
    journals.last().map(|j| j.sequence).unwrap_or(0)
}

/// A process never booked reads as empty.
pub async fn reads_empty_process<S: Store<Batch = Batch>>(store: &S) {
    let journals = store.read(process()).await.expect("read");
    assert!(journals.is_empty());
    assert_eq!(sequence(&journals), 0);
}

/// The first commit goes at sequence 1; the read returns the journal, its
/// lines with their batches and its diffs.
pub async fn commits_and_reads_back<S: Store<Batch = Batch>>(store: &S) {
    let p = process();
    let o = object();
    let batch = unique_key();
    assert_eq!(
        store.commit(commit(p, 0, batch, o)).await.expect("commit"),
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
}

/// The primary key on (process, sequence) is the compare-and-set: a second
/// commit at the same sequence is `SequenceTaken`, and the next one goes in.
pub async fn sequence_is_the_compare_and_set<S: Store<Batch = Batch>>(store: &S) {
    let p = process();
    let o = object();
    let batch = unique_key();
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
pub async fn concurrent_commits_land_once<S: Store<Batch = Batch> + Clone + 'static>(store: &S) {
    let p = process();
    let batch = unique_key();
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
pub async fn sums_a_batch_across_processes<S: Store<Batch = Batch>>(store: &S) {
    let batch = unique_key();
    let other = unique_key();
    assert!(store.batch_sum(batch).await.expect("sum").is_empty());
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
        store.batch_sum(batch).await.expect("sum"),
        vec![(UnitKey(840), 5), (UnitKey(978), 50)]
    );
    assert_eq!(
        store.batch_sum(other).await.expect("sum"),
        vec![(UnitKey(978), 7)]
    );
}

/// The commit indexes the version each type was evaluated under, latest
/// wins, and `record_versions` moves the index without a journal.
pub async fn records_versions<S: Store<Batch = Batch>>(store: &S) {
    let p = process();
    let invoice = format!("invoice-{}", Uuid::now_v7());
    let payment = format!("payment-{}", Uuid::now_v7());
    assert_eq!(
        store
            .commit(versions_commit(
                p,
                0,
                &[(invoice.as_str(), 2), (payment.as_str(), 1)]
            ))
            .await
            .expect("commit"),
        CommitOutcome::Committed
    );
    assert_eq!(
        store
            .processes_behind(&invoice, 3, 10)
            .await
            .expect("behind"),
        vec![p]
    );
    assert!(
        store
            .processes_behind(&invoice, 2, 10)
            .await
            .expect("behind")
            .is_empty()
    );
    assert_eq!(
        store
            .processes_behind(&payment, 2, 10)
            .await
            .expect("behind"),
        vec![p]
    );
    // A later commit under older rules puts the process back on the worklist.
    assert_eq!(
        store
            .commit(versions_commit(p, 1, &[(invoice.as_str(), 1)]))
            .await
            .expect("commit"),
        CommitOutcome::Committed
    );
    assert_eq!(
        store
            .processes_behind(&invoice, 2, 10)
            .await
            .expect("behind"),
        vec![p]
    );
    // A restatement with nothing to book moves the index on its own.
    store
        .record_versions(p, &versions(&[(invoice.as_str(), 3)]))
        .await
        .expect("record");
    assert!(
        store
            .processes_behind(&invoice, 3, 10)
            .await
            .expect("behind")
            .is_empty()
    );
    assert_eq!(
        store
            .processes_behind(&invoice, 4, 10)
            .await
            .expect("behind"),
        vec![p]
    );
    assert_eq!(
        store.read(p).await.expect("read").len(),
        2,
        "no journal for it"
    );
}

/// `processes_behind` lists the processes whose recorded version of a type
/// is below the running one, bounded by the limit.
pub async fn lists_processes_behind<S: Store<Batch = Batch>>(store: &S) {
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

/// A batch no booking has used has no row.
pub async fn finds_no_row_for_a_batch_never_booked<S: Store<Batch = Batch>>(store: &S) {
    assert_eq!(
        store.find(&named(BatchType::Debit)).await.expect("find"),
        None
    );
}

/// `ensure` creates a batch's row: it holds the value's ledger and type, a
/// second call and `find` return the same row, and a journal carrying the key
/// reads back in any process.
pub async fn ensure_creates_the_row<S: Store<Batch = Batch>>(store: &S) {
    let batch = named(BatchType::Credit);
    let row = store.ensure(&batch).await.expect("ensure");
    assert_eq!(row.ledger, batch.ledger());
    assert_eq!(row.batch_type, BatchType::Credit);
    assert!(row.key.0 > 0, "from a sequence, not {}", row.key.0);
    assert_eq!(store.find(&batch).await.expect("find"), Some(row));
    assert_eq!(store.ensure(&batch).await.expect("ensure"), row);
    for p in [process(), process()] {
        let o = object();
        assert_eq!(
            store
                .commit(one_line(p, 0, row.key, o))
                .await
                .expect("commit"),
            CommitOutcome::Committed
        );
        assert_eq!(
            store.read(p).await.expect("read")[0].lines,
            vec![line(row.key, o, 978, 100)]
        );
    }
}

/// One counter allocates over every table: batches of different kinds never
/// share a key.
pub async fn one_counter_allocates_over_every_table<S: Store<Batch = Batch>>(store: &S) {
    let batches = [
        Batch::Receivable(object()),
        Batch::CashClearing(object()),
        Batch::Revenue(Uuid::now_v7().to_string()),
        Batch::Vat(Uuid::now_v7().to_string()),
        named(BatchType::Debit),
    ];
    let mut keys = Vec::new();
    for batch in batches {
        keys.push(store.ensure(&batch).await.expect("ensure").key);
    }
    let distinct: std::collections::BTreeSet<BatchKey> = keys.iter().copied().collect();
    assert_eq!(distinct.len(), keys.len(), "{keys:?}");
}

/// A row needs no journal: with nothing booked, and after a commit that lost
/// the sequence, it stays and sums to nothing.
pub async fn a_row_stays_without_its_journal<S: Store<Batch = Batch>>(store: &S) {
    let p = process();
    assert_eq!(
        store
            .commit(commit(p, 0, unique_key(), object()))
            .await
            .expect("commit"),
        CommitOutcome::Committed
    );
    let batch = named(BatchType::Debit);
    let row = store.ensure(&batch).await.expect("ensure");
    assert_eq!(
        store
            .commit(one_line(p, 0, row.key, object()))
            .await
            .expect("commit"),
        CommitOutcome::SequenceTaken
    );
    assert_eq!(store.find(&batch).await.expect("find"), Some(row));
    assert_eq!(store.read(p).await.expect("read").len(), 1);
    assert!(store.batch_sum(row.key).await.expect("sum").is_empty());
}

/// Concurrent first uses of one batch get one row, and their journals land
/// under its key.
pub async fn concurrent_first_uses_share_one_key<S: Store<Batch = Batch> + Clone + 'static>(
    store: &S,
) {
    let batch = named(BatchType::Debit);
    let store = Arc::new(store.clone());
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let batch = batch.clone();
        tasks.push(tokio::spawn(async move {
            let p = process();
            let row = store.ensure(&batch).await.expect("ensure");
            let outcome = store
                .commit(one_line(p, 0, row.key, object()))
                .await
                .expect("commit");
            (p, outcome)
        }));
    }
    let mut landed = Vec::new();
    for t in tasks {
        let (p, outcome) = t.await.expect("task");
        assert_eq!(outcome, CommitOutcome::Committed);
        landed.push(p);
    }
    let row = store.find(&batch).await.expect("find").expect("a row");
    for p in landed {
        assert_eq!(
            store.read(p).await.expect("read")[0].lines[0].batch,
            row.key
        );
    }
}

/// `ensure` returns a row that exists as it is: a value calling it something
/// else changes nothing, and `check` tells them apart.
pub async fn ensure_never_changes_a_row<S: Store<Batch = Batch>>(store: &S) {
    let name = Uuid::now_v7().to_string();
    let debit = Batch::Named {
        name: name.clone(),
        ledger: LedgerKey(1),
        batch_type: BatchType::Debit,
    };
    let credit = Batch::Named {
        name,
        ledger: LedgerKey(1),
        batch_type: BatchType::Credit,
    };
    let row = store.ensure(&debit).await.expect("ensure");
    assert_eq!(store.ensure(&credit).await.expect("ensure"), row);
    assert!(row.check(&credit).is_err());
    assert_eq!(store.find(&debit).await.expect("find"), Some(row));
}

/// Every check, in order.
pub async fn run_all<S: Store<Batch = Batch> + Clone + 'static>(store: &S) {
    reads_empty_process(store).await;
    commits_and_reads_back(store).await;
    sequence_is_the_compare_and_set(store).await;
    concurrent_commits_land_once(store).await;
    sums_a_batch_across_processes(store).await;
    records_versions(store).await;
    lists_processes_behind(store).await;
    finds_no_row_for_a_batch_never_booked(store).await;
    ensure_creates_the_row(store).await;
    one_counter_allocates_over_every_table(store).await;
    a_row_stays_without_its_journal(store).await;
    concurrent_first_uses_share_one_key(store).await;
    ensure_never_changes_a_row(store).await;
}
