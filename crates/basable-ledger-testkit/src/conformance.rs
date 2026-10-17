//! The store conformance suite: what every `Store` adapter must do, run
//! against the in-memory store here and against Postgres in its own crate.
//! Each check is a function so an adapter's test file can run them one by
//! one; `run_all` runs them in order.

use std::sync::Arc;

use basable_ledger::{
    BatchId, BatchKey, BatchStatus, BatchType, Commit, CommitLine, CommitOutcome, LedgerKey,
    LineBatch, NewBatch, ObjectId, ProcessId, SnapshotDiff, Store, UnitKey,
};
use uuid::Uuid;

fn process() -> ProcessId {
    ProcessId(Uuid::now_v7())
}

fn object() -> ObjectId {
    ObjectId(Uuid::now_v7())
}

fn unique_batch(prefix: &str) -> BatchId {
    (prefix, Uuid::now_v7()).into()
}

fn commit(process: ProcessId, after: i64, batch: LineBatch, object: ObjectId) -> Commit {
    Commit {
        process_id: process,
        after_sequence: after,
        new_batches: Vec::new(),
        event_time: chrono::Utc::now(),
        lines: vec![
            CommitLine {
                batch,
                object_id: object,
                unit: UnitKey(978),
                amount: 100,
            },
            CommitLine {
                batch,
                object_id: object,
                unit: UnitKey(978),
                amount: -100,
            },
        ],
        snapshot_diffs: vec![SnapshotDiff {
            object_id: object,
            type_name: "conformance".into(),
            full: Some(serde_json::json!({"v": after + 1})),
            patch: None,
        }],
    }
}

fn new_batch(id: &BatchId, epoch: i32) -> NewBatch {
    NewBatch {
        id: id.clone(),
        epoch,
        ledger: LedgerKey(1),
        batch_type: BatchType::Debit,
    }
}

/// A process never booked reads as empty.
pub async fn reads_empty_process<S: Store>(store: &S) {
    let read = store.read(process()).await.expect("read");
    assert!(read.journals.is_empty());
    assert!(read.versions.is_empty());
    assert!(read.batches.is_empty());
    assert_eq!(read.sequence(), 0);
}

/// The first commit goes at sequence 1 and creates its batch; the read
/// returns the journal, its lines and the batch row.
pub async fn commits_and_reads_back<S: Store>(store: &S) {
    let p = process();
    let o = object();
    let id = unique_batch("conf");
    let mut c = commit(p, 0, LineBatch::New(0), o);
    c.new_batches.push(new_batch(&id, 1));
    assert_eq!(
        store.commit(c).await.expect("commit"),
        CommitOutcome::Committed
    );
    let read = store.read(p).await.expect("read");
    assert_eq!(read.sequence(), 1);
    assert_eq!(read.journals.len(), 1);
    let journal = &read.journals[0];
    assert_eq!(journal.process_id, p);
    assert_eq!(journal.sequence, 1);
    assert_eq!(journal.lines.len(), 2);
    assert_eq!(journal.snapshot_diffs.len(), 1);
    assert_eq!(journal.snapshot_diffs[0].object_id, o);
    let key = journal.lines[0].batch_key;
    let row = read.batches.get(&key).expect("batch row returned");
    assert_eq!(row.id, id);
    assert_eq!(row.epoch, 1);
    assert_eq!(row.status, BatchStatus::Open);
    assert_eq!(row.ledger, LedgerKey(1));
    assert_eq!(row.batch_type, BatchType::Debit);
}

/// The primary key on (process, sequence) is the compare-and-set: a second
/// commit at the same sequence is `SequenceTaken`, and the next one goes in.
pub async fn sequence_is_the_compare_and_set<S: Store>(store: &S) {
    let p = process();
    let o = object();
    let id = unique_batch("conf");
    let mut c = commit(p, 0, LineBatch::New(0), o);
    c.new_batches.push(new_batch(&id, 1));
    assert_eq!(
        store.commit(c.clone()).await.expect("commit"),
        CommitOutcome::Committed
    );
    assert_eq!(
        store.commit(c).await.expect("commit"),
        CommitOutcome::SequenceTaken
    );
    let key = store.read(p).await.expect("read").journals[0].lines[0].batch_key;
    let second = commit(p, 1, LineBatch::Key(key), o);
    assert_eq!(
        store.commit(second).await.expect("commit"),
        CommitOutcome::Committed
    );
    let read = store.read(p).await.expect("read");
    assert_eq!(read.sequence(), 2);
    assert_eq!(
        read.journals.iter().map(|j| j.sequence).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

/// Under concurrency exactly one of the racing commits at one sequence lands.
pub async fn concurrent_commits_land_once<S: Store + Clone + 'static>(store: &S) {
    let p = process();
    let id = unique_batch("conf");
    let seed = {
        let mut c = commit(p, 0, LineBatch::New(0), object());
        c.new_batches.push(new_batch(&id, 1));
        c
    };
    assert_eq!(
        store.commit(seed).await.expect("commit"),
        CommitOutcome::Committed
    );
    let key = store.read(p).await.expect("read").journals[0].lines[0].batch_key;
    let store = Arc::new(store.clone());
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .commit(commit(p, 1, LineBatch::Key(key), object()))
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
    assert_eq!(store.read(p).await.expect("read").sequence(), 2);
}

/// `resolve` answers `None` for an unknown id and the highest epoch's row
/// for a known one; a lost race on (id, epoch) reuses the winner's row.
pub async fn resolves_batches<S: Store>(store: &S) {
    let id = unique_batch("conf");
    assert_eq!(
        store
            .resolve(std::slice::from_ref(&id))
            .await
            .expect("resolve"),
        vec![None]
    );
    let p1 = process();
    let p2 = process();
    let mut c1 = commit(p1, 0, LineBatch::New(0), object());
    c1.new_batches.push(new_batch(&id, 1));
    let mut c2 = commit(p2, 0, LineBatch::New(0), object());
    c2.new_batches.push(new_batch(&id, 1));
    assert_eq!(
        store.commit(c1).await.expect("commit"),
        CommitOutcome::Committed
    );
    assert_eq!(
        store.commit(c2).await.expect("commit"),
        CommitOutcome::Committed
    );
    let k1 = store.read(p1).await.expect("read").journals[0].lines[0].batch_key;
    let k2 = store.read(p2).await.expect("read").journals[0].lines[0].batch_key;
    assert_eq!(k1, k2, "both processes share the one (id, epoch) row");
    let resolved = store
        .resolve(std::slice::from_ref(&id))
        .await
        .expect("resolve");
    let row = resolved[0].as_ref().expect("row");
    assert_eq!(row.key, k1);
    assert_eq!(row.epoch, 1);
    // A second epoch becomes the resolved one.
    let mut c3 = commit(p1, 1, LineBatch::New(0), object());
    c3.new_batches.push(new_batch(&id, 2));
    assert_eq!(
        store.commit(c3).await.expect("commit"),
        CommitOutcome::Committed
    );
    let resolved = store
        .resolve(std::slice::from_ref(&id))
        .await
        .expect("resolve");
    assert_eq!(resolved[0].as_ref().expect("row").epoch, 2);
}

/// The sum of one batch's lines spans every process.
pub async fn sums_a_batch_across_processes<S: Store>(store: &S) {
    let id = unique_batch("conf");
    let p1 = process();
    let p2 = process();
    let mut c1 = commit(p1, 0, LineBatch::New(0), object());
    c1.new_batches.push(new_batch(&id, 1));
    c1.lines = vec![CommitLine {
        batch: LineBatch::New(0),
        object_id: object(),
        unit: UnitKey(978),
        amount: 70,
    }];
    assert_eq!(
        store.commit(c1).await.expect("commit"),
        CommitOutcome::Committed
    );
    let key: BatchKey = store.read(p1).await.expect("read").journals[0].lines[0].batch_key;
    let mut c2 = commit(p2, 0, LineBatch::Key(key), object());
    c2.lines = vec![
        CommitLine {
            batch: LineBatch::Key(key),
            object_id: object(),
            unit: UnitKey(978),
            amount: -20,
        },
        CommitLine {
            batch: LineBatch::Key(key),
            object_id: object(),
            unit: UnitKey(840),
            amount: 5,
        },
    ];
    assert_eq!(
        store.commit(c2).await.expect("commit"),
        CommitOutcome::Committed
    );
    let sums = store.batch_sum(key).await.expect("sum");
    assert_eq!(sums, vec![(UnitKey(840), 5), (UnitKey(978), 50)]);
}

/// The recorded version is an upsert that never moves backwards.
pub async fn records_versions<S: Store>(store: &S) {
    let p = process();
    store.record_version(p, "invoice", 2).await.expect("record");
    store.record_version(p, "payment", 1).await.expect("record");
    store.record_version(p, "invoice", 1).await.expect("record");
    let read = store.read(p).await.expect("read");
    assert_eq!(read.versions.get("invoice"), Some(&2));
    assert_eq!(read.versions.get("payment"), Some(&1));
    store.record_version(p, "invoice", 3).await.expect("record");
    let read = store.read(p).await.expect("read");
    assert_eq!(read.versions.get("invoice"), Some(&3));
}

/// Every check, in order.
pub async fn run_all<S: Store + Clone + 'static>(store: &S) {
    reads_empty_process(store).await;
    commits_and_reads_back(store).await;
    sequence_is_the_compare_and_set(store).await;
    concurrent_commits_land_once(store).await;
    resolves_batches(store).await;
    sums_a_batch_across_processes(store).await;
    records_versions(store).await;
}
