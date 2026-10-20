//! The envelope page (`TypedStore::list`, porting note 97): newest first
//! with an exact `next`, the keyset cursor under inserts and deletes between
//! pages, ties on `created_at` broken by `id`, the namespace, label, phase
//! and deletion-intent filters, and the limit bound. No Go counterpart:
//! `InspectPage` has no test of its own.

use basable_core::labels::Labels;
use basable_processingobject::{
    CreateOptions, Cursor, Deleting, Error, ListQuery, MAX_LIST_LIMIT, NamespacedName, Outcome,
    Phase, Ref,
};
use basable_processingobject_testkit::{Harness, Spec, Status};
use uuid::Uuid;

fn spec(n: i32) -> Spec {
    Spec {
        widgets: n,
        content: format!("object {n}"),
    }
}

fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// `count` objects, created one after another; the refs in creation order.
async fn seed(h: &Harness, count: i32) -> Vec<Ref> {
    let mut refs = Vec::with_capacity(count as usize);
    for n in 1..=count {
        refs.push(h.create(spec(n)).await.unwrap());
    }
    refs
}

fn ids(page: &[basable_processingobject::Object<Spec, Status>]) -> Vec<Uuid> {
    page.iter().map(|o| o.id).collect()
}

#[tokio::test]
async fn pages_newest_first_with_an_exact_next() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let refs = seed(&h, 5).await;
    let newest_first: Vec<Uuid> = refs.iter().rev().map(|r| r.id).collect();

    let p1 = h.store.list(&ListQuery::first(2)).await.unwrap();
    assert_eq!(ids(&p1.items), newest_first[0..2]);
    assert_eq!(p1.items[0].spec, spec(5), "the typed rows ride along");
    let c1 = p1.next.expect("three more exist");
    assert_eq!(c1, Cursor::after(&p1.items[1]));

    let p2 = h.store.list(&ListQuery::first(2).after(c1)).await.unwrap();
    assert_eq!(ids(&p2.items), newest_first[2..4]);
    let c2 = p2.next.expect("one more exists");

    let p3 = h.store.list(&ListQuery::first(2).after(c2)).await.unwrap();
    assert_eq!(ids(&p3.items), newest_first[4..5]);
    assert_eq!(p3.next, None, "the last page says so");

    // Exact: a page that ends precisely on the last object has no next, so
    // nobody has to ask for an empty page to learn the end.
    let whole = h.store.list(&ListQuery::first(5)).await.unwrap();
    assert_eq!(ids(&whole.items), newest_first);
    assert_eq!(whole.next, None);
    let empty = h
        .store
        .list(&ListQuery::first(5).after(Cursor::after(&whole.items[4])))
        .await
        .unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(empty.next, None);
    h.finish().await;
}

#[tokio::test]
async fn the_cursor_is_a_value_so_inserts_and_deletes_between_pages_shift_nothing() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let refs = seed(&h, 4).await;
    let p1 = h.store.list(&ListQuery::first(2)).await.unwrap();
    assert_eq!(ids(&p1.items), vec![refs[3].id, refs[2].id]);
    let token = p1.next.unwrap().to_string();

    // Between the pages: a newer object arrives, the cursor's own object and
    // one older object are hard-deleted (the envelope removed, as a
    // teardown's `Outcome::Delete` removes it; the typed rows cascade).
    let newer = h.create(spec(5)).await.unwrap();
    for gone in [refs[2].id, refs[1].id] {
        sqlx::query("DELETE FROM nano_conformance.processing_object_conformance WHERE id = $1")
            .bind(gone)
            .execute(h.db.superuser())
            .await
            .unwrap();
    }

    let cursor: Cursor = token.parse().expect("the token round-trips");
    let p2 = h
        .store
        .list(&ListQuery::first(2).after(cursor))
        .await
        .unwrap();
    assert_eq!(
        ids(&p2.items),
        vec![refs[0].id],
        "strictly after the pair: the newer object sorts before page one and the gone are simply absent"
    );
    assert_eq!(p2.next, None);

    // A fresh first page shows the newer object at the top.
    let again = h.store.list(&ListQuery::first(10)).await.unwrap();
    assert_eq!(ids(&again.items), vec![newer.id, refs[3].id, refs[0].id]);
    h.finish().await;
}

#[tokio::test]
async fn ties_on_created_at_are_broken_by_id() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let refs = seed(&h, 3).await;
    sqlx::query(
        "UPDATE nano_conformance.processing_object_conformance
         SET created_at = '2026-01-02 03:04:05.678901+00' WHERE id = ANY($1)",
    )
    .bind(refs.iter().map(|r| r.id).collect::<Vec<_>>())
    .execute(h.db.superuser())
    .await
    .unwrap();
    let mut expected: Vec<Uuid> = refs.iter().map(|r| r.id).collect();
    expected.sort();
    expected.reverse();

    let mut seen = Vec::new();
    let mut q = ListQuery::first(1);
    loop {
        let page = h.store.list(&q).await.unwrap();
        seen.extend(ids(&page.items));
        match page.next {
            Some(c) => q = q.after(c),
            None => break,
        }
    }
    assert_eq!(
        seen, expected,
        "equal timestamps page by id, descending, once each"
    );
    h.finish().await;
}

#[tokio::test]
async fn the_filters_narrow_the_page() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    // The first object converges before any other exists, so the drive is
    // deterministic; the rest stay pending.
    let converged = h.create(spec(1)).await.unwrap();
    h.drive_once(|_claim| Box::pin(async { Ok(Outcome::converged(None)) }))
        .await
        .unwrap();
    let plain = h.create(spec(2)).await.unwrap();
    let labelled_id = Uuid::new_v4();
    let labelled = h
        .store
        .create(
            labelled_id,
            h.identity(labelled_id),
            &spec(3),
            &Status::default(),
            CreateOptions::labels(labels(&[("tier", "gold"), ("region", "eu")])),
        )
        .await
        .unwrap();
    let other_namespace = Uuid::new_v4();
    let elsewhere_id = Uuid::new_v4();
    let elsewhere = h
        .store
        .create(
            elsewhere_id,
            NamespacedName::new(other_namespace, "elsewhere"),
            &spec(4),
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap();
    let deleting = h.create(spec(5)).await.unwrap();
    assert!(h.store.mark_deleted(&deleting).await.unwrap());

    let list = |q: ListQuery| {
        let store = h.store.clone();
        async move { ids(&store.list(&q).await.unwrap().items) }
    };

    assert_eq!(
        list(ListQuery::default()).await,
        vec![
            deleting.id,
            elsewhere.id,
            labelled.id,
            plain.id,
            converged.id
        ],
        "unfiltered: everything, tombstones included, newest first"
    );
    assert_eq!(
        list(ListQuery::default().in_namespace(h.namespace)).await,
        vec![deleting.id, labelled.id, plain.id, converged.id]
    );
    assert_eq!(
        list(ListQuery::default().in_namespace(other_namespace)).await,
        vec![elsewhere.id]
    );
    assert_eq!(
        list(ListQuery::default().with_labels(labels(&[("tier", "gold")]))).await,
        vec![labelled.id],
        "containment: a subset of the object's labels matches"
    );
    assert!(
        list(ListQuery::default().with_labels(labels(&[("tier", "silver")])))
            .await
            .is_empty()
    );
    assert_eq!(
        list(ListQuery::default().in_phase(Phase::Converged)).await,
        vec![converged.id]
    );
    assert_eq!(
        list(ListQuery::default().in_phase(Phase::Pending)).await,
        vec![deleting.id, elsewhere.id, labelled.id, plain.id]
    );
    assert_eq!(
        list(ListQuery::default().deleting(Deleting::Exclude)).await,
        vec![elsewhere.id, labelled.id, plain.id, converged.id]
    );
    assert_eq!(
        list(ListQuery::default().deleting(Deleting::Only)).await,
        vec![deleting.id]
    );
    assert_eq!(
        list(
            ListQuery::default()
                .in_namespace(h.namespace)
                .in_phase(Phase::Pending)
                .deleting(Deleting::Exclude)
        )
        .await,
        vec![labelled.id, plain.id],
        "filters conjoin"
    );
    h.finish().await;
}

#[tokio::test]
async fn a_limit_outside_the_bound_is_refused_before_any_query() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    for limit in [0, MAX_LIST_LIMIT + 1] {
        let err = h.store.list(&ListQuery::first(limit)).await.unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)), "{limit}: {err}");
    }
    let page = h
        .store
        .list(&ListQuery::first(MAX_LIST_LIMIT))
        .await
        .unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.next, None);
    h.finish().await;
}
