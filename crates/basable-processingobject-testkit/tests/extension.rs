//! The optional same-id extension row (porting note 98): the conformance
//! status's `OrderStatus`, kept in `conformance_order_status` under the
//! status row. Absent at a birth without the part and present at one with
//! it; a completion or a mid-attempt write carrying the part inserts or
//! replaces the row whole, one without it leaves the row alone (presence is
//! a fact about the object, not about the writer); a hard delete cascades
//! it with the status row; every read path attaches it. No Go counterpart:
//! the monorepo's conformance type has no extension, the pattern lives in
//! the cluster operator's adapter.

use basable_processingobject::{CreateOptions, Error, ListQuery, Outcome};
use basable_processingobject_testkit::{Harness, OrderStatus, Spec, Status};
use uuid::Uuid;

fn spec() -> Spec {
    Spec {
        widgets: 2,
        content: "ordered".into(),
    }
}

fn plain(w: i32) -> Status {
    Status {
        provisioned_widgets: w,
        external_id: "ext".into(),
        order: None,
    }
}

fn ordered(w: i32, order_id: &str) -> Status {
    Status {
        order: Some(OrderStatus {
            key: "order-key".into(),
            order_id: order_id.into(),
        }),
        ..plain(w)
    }
}

/// The extension rows for `id`, read across the nanoservice boundary.
async fn extension_rows(h: &Harness, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM nano_conformance.conformance_order_status WHERE id = $1",
    )
    .bind(id)
    .fetch_one(h.db.superuser())
    .await
    .unwrap()
}

#[tokio::test]
async fn the_row_is_absent_or_present_from_birth_and_every_read_attaches_it() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let without = h.create(spec()).await.unwrap();
    let with_id = Uuid::new_v4();
    let with = h
        .store
        .create(
            with_id,
            h.identity(with_id),
            &spec(),
            &ordered(0, "o-1"),
            CreateOptions::none(),
        )
        .await
        .unwrap();
    assert_eq!(extension_rows(&h, without.id).await, 0);
    assert_eq!(extension_rows(&h, with.id).await, 1);

    assert_eq!(
        h.store.read(&without).await.unwrap().status,
        Status::default()
    );
    assert_eq!(h.store.read(&with).await.unwrap().status, ordered(0, "o-1"));

    // `read_many` and `list` load the typed rows through the same adapter
    // read, so the LEFT JOIN serves them too.
    let many = h.store.read_many(&[without.id, with.id]).await.unwrap();
    assert_eq!(many[0].status.order, None);
    assert_eq!(many[1].status.order, ordered(0, "o-1").order);
    let page = h.store.list(&ListQuery::default()).await.unwrap();
    let listed = |id: Uuid| {
        page.items
            .iter()
            .find(|o| o.id == id)
            .unwrap()
            .status
            .order
            .clone()
    };
    assert_eq!(listed(without.id), None);
    assert_eq!(listed(with.id), ordered(0, "o-1").order);
    h.finish().await;
}

#[tokio::test]
async fn a_status_with_the_part_writes_the_row_whole_and_one_without_it_leaves_it_alone() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h.create(spec()).await.unwrap();

    // A completion carrying the part inserts the row.
    h.drive_once(|_| Box::pin(async { Ok(Outcome::converged(Some(ordered(2, "o-1")))) }))
        .await
        .unwrap();
    assert_eq!(h.store.read(&r).await.unwrap().status, ordered(2, "o-1"));
    assert_eq!(extension_rows(&h, r.id).await, 1);

    // The next one replaces it whole.
    h.store.nudge(&r).await.unwrap();
    h.drive_once(|_| Box::pin(async { Ok(Outcome::converged(Some(ordered(2, "o-2")))) }))
        .await
        .unwrap();
    assert_eq!(h.store.read(&r).await.unwrap().status, ordered(2, "o-2"));
    assert_eq!(extension_rows(&h, r.id).await, 1);

    // A mid-attempt write is the same adapter write.
    h.store.nudge(&r).await.unwrap();
    h.drive_once(|claim| {
        Box::pin(async move {
            claim.write_status(ordered(2, "o-3")).await?;
            Ok(Outcome::converged(None))
        })
    })
    .await
    .unwrap();
    assert_eq!(h.store.read(&r).await.unwrap().status, ordered(2, "o-3"));

    // A status without the part writes its own columns and leaves the row:
    // whether the object has the part is not this writer's to decide.
    h.store.nudge(&r).await.unwrap();
    h.drive_once(|_| Box::pin(async { Ok(Outcome::converged(Some(plain(3)))) }))
        .await
        .unwrap();
    assert_eq!(
        h.store.read(&r).await.unwrap().status,
        ordered(3, "o-3"),
        "the status columns moved, the extension row stayed"
    );
    assert_eq!(extension_rows(&h, r.id).await, 1);
    h.finish().await;
}

#[tokio::test]
async fn a_hard_delete_cascades_the_row_with_the_status() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h.create(spec()).await.unwrap();
    h.drive_once(|_| Box::pin(async { Ok(Outcome::converged(Some(ordered(2, "o-1")))) }))
        .await
        .unwrap();
    assert_eq!(extension_rows(&h, r.id).await, 1);

    h.store.mark_deleted(&r).await.unwrap();
    let (_, done) = h
        .drive_once(|_| Box::pin(async { Ok(Outcome::delete()) }))
        .await
        .unwrap();
    assert!(done.outcome().unwrap().is_delete());
    assert!(matches!(h.store.read(&r).await, Err(Error::NotFound(_))));
    assert_eq!(
        extension_rows(&h, r.id).await,
        0,
        "envelope, status, extension: one cascade"
    );
    assert!(h.is_archived(r.id).await.unwrap());
    h.finish().await;
}
