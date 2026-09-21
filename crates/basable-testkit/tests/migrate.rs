//! The dbmate-format runner against a real database: the ledger, idempotent
//! re-application, the boot-time verify, and the unique-violation helper.

mod common;

use basable_db::migrate::{self, MigrateError};
use common::{Inventory, fixture_db, fixtures};
use sqlx::Executor;

#[tokio::test]
async fn the_ledger_holds_every_version_and_a_rerun_applies_nothing() {
    let Some(db) = fixture_db().await else { return };
    let files = migrate::read_dir(&fixtures()).unwrap();
    let versions: Vec<&str> = files.iter().map(|m| m.version.as_str()).collect();
    assert_eq!(
        migrate::applied_versions(db.migrator()).await.unwrap(),
        versions
    );

    let again = migrate::apply(db.migrator(), &files, &[]).await.unwrap();
    assert!(again.is_empty(), "a second run applied {again:?}");

    migrate::verify(db.migrator(), &versions).await.unwrap();
    let missing = migrate::verify(db.migrator(), &["20990101000000"])
        .await
        .unwrap_err();
    assert!(
        matches!(missing, MigrateError::Missing(ref v) if v == &["20990101000000"]),
        "{missing}"
    );

    // The ledger is dbmate's own table, so the Job and this runner agree.
    let (width,): (i32,) = sqlx::query_as(
        "SELECT character_maximum_length::int FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'schema_migrations' AND column_name = 'version'",
    )
    .fetch_one(db.migrator())
    .await
    .unwrap();
    assert_eq!(width, 128);
    db.finish().await;
}

#[tokio::test]
async fn a_failing_migration_rolls_back_and_leaves_no_ledger_row() {
    let Some(db) = fixture_db().await else { return };
    let broken = migrate::parse(
        std::path::Path::new("20260102000000_broken.sql"),
        "-- migrate:up\nCREATE TABLE public.half (id int);\nSELECT 1/0;\n",
    )
    .unwrap();
    let err = migrate::apply(db.migrator(), &[broken], &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, MigrateError::Sql { ref version, .. } if version == "20260102000000"),
        "{err}"
    );
    let applied = migrate::applied_versions(db.migrator()).await.unwrap();
    assert!(!applied.iter().any(|v| v == "20260102000000"));
    let e = db
        .migrator()
        .execute("SELECT count(*) FROM public.half")
        .await
        .unwrap_err();
    assert_eq!(
        common::sqlstate(&e),
        "42P01",
        "the table from the failed file must not exist"
    );
    db.finish().await;
}

#[tokio::test]
async fn replacements_rewrite_the_sql_before_it_runs() {
    let Some(db) = fixture_db().await else { return };
    let m = migrate::parse(
        std::path::Path::new("20260102000001_marker.sql"),
        "-- migrate:up\nCREATE TABLE public.PLACEHOLDER (id int);\n",
    )
    .unwrap();
    migrate::apply(db.migrator(), &[m], &[("PLACEHOLDER", "replaced")])
        .await
        .unwrap();
    db.migrator()
        .execute("SELECT count(*) FROM public.replaced")
        .await
        .unwrap();
    db.finish().await;
}

#[tokio::test]
async fn a_unique_violation_is_recognised_by_sqlstate() {
    let Some(db) = fixture_db().await else { return };
    let inventory = db.nano_pool::<Inventory>().await;
    inventory
        .execute("INSERT INTO item (sku) VALUES ('dup')")
        .await
        .unwrap();
    let err = inventory
        .execute("INSERT INTO item (sku) VALUES ('dup')")
        .await
        .unwrap_err();
    assert!(basable_db::sqlstate::is_unique_violation(&err), "{err}");
    assert!(!basable_db::sqlstate::is_unique_violation(
        &sqlx::Error::PoolTimedOut
    ));
    assert_eq!(basable_db::sqlstate::of(&sqlx::Error::PoolTimedOut), None);
    inventory.close().await;
    db.finish().await;
}
