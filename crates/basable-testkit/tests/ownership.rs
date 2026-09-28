//! Plan B3, pinned: a nanoservice's role reaches its own tables and its own
//! envelope partition with every statement shape the framework uses, and
//! nothing else — not another nanoservice's tables, not the envelope parent.

mod common;

use common::{Inventory, Orders, fixture_db, sqlstate};
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test]
async fn the_orders_role_runs_every_framework_statement_shape_on_its_partition() {
    let Some(db) = fixture_db().await else { return };
    let orders = db.nano_pool::<Orders>().await;
    let id = Uuid::new_v4();
    let token = Uuid::new_v4();

    // Create: envelope row in the partition plus the typed rows, one
    // transaction, as TypedStore::create does.
    let mut tx = orders.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO processing_object_order (id, processing_object_type_key, external_id, name, namespace)
         VALUES ($1, 1, 'ord_x', 'first', $2)",
    )
    .bind(id)
    .bind(Uuid::nil())
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO order_spec (id, customer, lines) VALUES ($1, 'acme', 1)")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO order_status (id) VALUES ($1)")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Claim: the scan with SKIP LOCKED, then the lease write.
    let claimed: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM processing_object_order
         WHERE due_at <= now() AND claim_token IS NULL
         ORDER BY due_at, last_reconciled_at ASC NULLS FIRST, id
         LIMIT 50 FOR UPDATE SKIP LOCKED",
    )
    .fetch_all(&orders)
    .await
    .unwrap();
    assert_eq!(claimed, vec![(id,)]);
    let leased = sqlx::query(
        "UPDATE processing_object_order
         SET claim_token = $2, claimed_at = clock_timestamp(),
             lease_expires_at = clock_timestamp() + interval '30 seconds', attempts = attempts + 1
         WHERE id = $1 AND claim_token IS NULL",
    )
    .bind(id)
    .bind(token)
    .execute(&orders)
    .await
    .unwrap();
    assert_eq!(leased.rows_affected(), 1);

    // Completion: status by the whole row, envelope by the fenced update.
    sqlx::query(
        "UPDATE order_status SET phase = 'ready', updated_at = clock_timestamp() WHERE id = $1",
    )
    .bind(id)
    .execute(&orders)
    .await
    .unwrap();
    let completed = sqlx::query(
        "UPDATE processing_object_order
         SET observed_generation = generation, phase = 'converged', claim_token = NULL,
             claimed_at = NULL, lease_expires_at = NULL, last_reconciled_at = clock_timestamp(),
             next_reconcile_at = clock_timestamp() + interval '10 minutes'
         WHERE id = $1 AND claim_token = $2",
    )
    .bind(id)
    .bind(token)
    .execute(&orders)
    .await
    .unwrap();
    assert_eq!(completed.rows_affected(), 1);

    // The read model joins the typed rows in one snapshot.
    let mut snapshot = basable_db::begin_snapshot(&orders).await.unwrap();
    let (customer, phase): (String, String) = sqlx::query_as(
        "SELECT s.customer, t.phase FROM processing_object_order o
         JOIN order_spec s ON s.id = o.id JOIN order_status t ON t.id = o.id WHERE o.id = $1",
    )
    .bind(id)
    .fetch_one(&mut *snapshot)
    .await
    .unwrap();
    assert_eq!((customer.as_str(), phase.as_str()), ("acme", "ready"));
    let refused = sqlx::query("UPDATE order_status SET phase = 'x' WHERE id = $1")
        .bind(id)
        .execute(&mut *snapshot)
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&refused), "25006", "a snapshot is read only");
    snapshot.rollback().await.unwrap();

    // The identity trigger holds under the role too.
    let identity = sqlx::query("UPDATE order_spec SET customer = 'other' WHERE id = $1")
        .bind(id)
        .execute(&orders)
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&identity), "23514");
    assert!(basable_db::sqlstate::is_integrity_violation(&identity));

    // Delete cascades from the envelope through the composite FK.
    sqlx::query("DELETE FROM processing_object_order WHERE id = $1 AND claim_token IS NULL")
        .bind(id)
        .execute(&orders)
        .await
        .unwrap();
    let (left,): (i64,) = sqlx::query_as("SELECT count(*) FROM order_spec WHERE id = $1")
        .bind(id)
        .fetch_one(&orders)
        .await
        .unwrap();
    assert_eq!(left, 0);

    orders.close().await;
    db.finish().await;
}

#[tokio::test]
async fn the_orders_role_is_refused_elsewhere() {
    let Some(db) = fixture_db().await else { return };
    let orders = db.nano_pool::<Orders>().await;

    // Another nanoservice's table, qualified: a permission error.
    let e = orders
        .execute("SELECT count(*) FROM nano_inventory.item")
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&e), "42501");
    assert!(basable_db::sqlstate::is_insufficient_privilege(&e));
    let e = orders
        .execute("INSERT INTO nano_inventory.item (sku) VALUES ('x')")
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&e), "42501");

    // The envelope parent: the framework never needs it, so the role has
    // nothing on it, read or write.
    for sql in [
        "SELECT count(*) FROM basable.processing_object",
        "INSERT INTO basable.processing_object (processing_object_type_key, external_id, name, namespace) VALUES (1, 'x', 'x', '00000000-0000-0000-0000-000000000000')",
        "UPDATE basable.processing_object SET phase = 'blocked'",
        "DELETE FROM basable.processing_object",
    ] {
        let e = orders.execute(sql).await.unwrap_err();
        assert_eq!(sqlstate(&e), "42501", "{sql}");
    }

    // The registry is readable, not writable.
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM basable.processing_object_type")
        .fetch_one(&orders)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let e = orders
        .execute("INSERT INTO basable.processing_object_type (key, name) VALUES (9, 'x')")
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&e), "42501");

    // search_path is pinned to the role's own schema: an unqualified name
    // from another schema does not resolve, its own does.
    let e = orders
        .execute("SELECT count(*) FROM item")
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&e), "42P01");
    assert!(basable_db::sqlstate::is_undefined_table(&e));
    orders
        .execute("SELECT count(*) FROM order_spec")
        .await
        .unwrap();
    let (path,): (String,) = sqlx::query_as("SHOW search_path")
        .fetch_one(&orders)
        .await
        .unwrap();
    assert_eq!(path, "nano_orders");
    let (role,): (String,) = sqlx::query_as("SELECT current_user")
        .fetch_one(&orders)
        .await
        .unwrap();
    assert_eq!(role, "nano_orders");

    orders.close().await;
    db.finish().await;
}

#[tokio::test]
async fn the_migrator_owns_the_framework_and_nothing_of_a_nanoservice() {
    let Some(db) = fixture_db().await else { return };
    // The app login owns the framework schemas and the envelope parent.
    db.migrator()
        .execute("SELECT count(*) FROM basable.processing_object")
        .await
        .unwrap();
    db.migrator()
        .execute("SELECT count(*) FROM basable_config.configuration_object")
        .await
        .unwrap();
    // A nanoservice's tables belong to its role; unswitched, the login that
    // ran the migrations is refused like anyone else. Only SET ROLE, which
    // is what a NanoPool is, opens them.
    for sql in [
        "SELECT count(*) FROM nano_orders.order_spec",
        "SELECT count(*) FROM nano_inventory.item",
    ] {
        let e = db.migrator().execute(sql).await.unwrap_err();
        assert_eq!(sqlstate(&e), "42501", "{sql}");
    }

    let inventory = db.nano_pool::<Inventory>().await;
    inventory
        .execute("INSERT INTO item (sku) VALUES ('sku-1')")
        .await
        .unwrap();
    let e = inventory
        .execute("SELECT count(*) FROM nano_orders.order_spec")
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&e), "42501");
    assert_eq!(inventory.nanoservice(), "inventory");

    // The harness's own login crosses every line, for assertions like this.
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM nano_inventory.item")
        .fetch_one(db.superuser())
        .await
        .unwrap();
    assert_eq!(n, 1);

    inventory.close().await;
    db.finish().await;
}

#[tokio::test]
async fn a_pool_for_a_missing_role_fails_at_boot_not_on_the_first_query() {
    let Some(db) = fixture_db().await else { return };
    struct Ghost;
    impl basable_db::Nanoservice for Ghost {
        const NAME: &'static str = "ghost";
    }
    impl basable_db::Stateful for Ghost {}
    let err =
        basable_db::NanoPool::<Ghost>::connect(db.app_options(), basable_db::PoolConfig::default())
            .await
            .unwrap_err();
    assert_eq!(
        sqlstate(&err),
        "22023",
        "SET ROLE to an unknown role: {err}"
    );
    db.finish().await;
}
