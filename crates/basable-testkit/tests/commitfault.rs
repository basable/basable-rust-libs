//! The commit-fault proxy, proven in both modes: the client sees the same
//! broken connection, and only the database knows whether the row landed.

mod common;

use basable_testkit::{CommitFault, CommitFaultProxy};
use common::fixture_db;
use sqlx::Executor;
use sqlx::postgres::PgPoolOptions;

async fn commit_through_the_proxy(mode: CommitFault) -> (bool, String) {
    let db = fixture_db()
        .await
        .expect("TEST_DATABASE_URL is set for this test");
    let (proxy, options) = CommitFaultProxy::for_options(db.app_options())
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    db.migrator()
        .execute("CREATE TABLE public.receipt (id int PRIMARY KEY)")
        .await
        .unwrap();

    // Before arming, the proxy is transparent.
    let mut tx = pool.begin().await.unwrap();
    tx.execute("INSERT INTO public.receipt VALUES (0)")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    proxy.arm(mode);
    let mut tx = pool.begin().await.unwrap();
    tx.execute("INSERT INTO public.receipt VALUES (1)")
        .await
        .unwrap();
    let err = tx
        .commit()
        .await
        .expect_err("the acknowledgement was dropped");
    assert!(!proxy.is_armed(), "the fault fired once and disarmed");
    let text = err.to_string();

    // The connection is gone; the pool opens another and works again.
    pool.execute("SELECT 1").await.unwrap();

    let (landed,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM public.receipt WHERE id = 1)")
            .fetch_one(db.migrator())
            .await
            .unwrap();
    pool.close().await;
    drop(proxy);
    db.finish().await;
    (landed, text)
}

#[tokio::test]
async fn an_applied_fault_lands_the_row_and_loses_the_ack() {
    if std::env::var_os(basable_testkit::DATABASE_URL_VAR).is_none() {
        return;
    }
    let (landed, err) = commit_through_the_proxy(CommitFault::Applied).await;
    assert!(
        landed,
        "the COMMIT reached Postgres; the client only lost the answer ({err})"
    );
}

#[tokio::test]
async fn a_rolled_back_fault_loses_the_row_and_the_ack_alike() {
    if std::env::var_os(basable_testkit::DATABASE_URL_VAR).is_none() {
        return;
    }
    let (landed, err) = commit_through_the_proxy(CommitFault::RolledBack).await;
    assert!(
        !landed,
        "the COMMIT never reached Postgres, yet the client saw the same failure ({err})"
    );
}
