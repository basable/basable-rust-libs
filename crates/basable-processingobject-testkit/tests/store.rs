//! The store half of the conformance suite, ported from the monorepo's
//! `golang/test/processingobject`: envelope identity (round trip, name
//! conflicts, adoption, release by deletion intent), the create carve-outs,
//! the intent writes, the read model, and identity stability across an
//! ambiguous create commit in both commit-fault modes.

use std::time::Duration;

use basable_core::labels::Labels;
use basable_db::{NanoPool, PoolConfig};
use basable_processingobject::{
    CreateOptions, Error, NamespacedName, Phase, SCHEDULE_IMMEDIATE, TypedStore,
};
use basable_processingobject_testkit::{
    Conformance, Harness, PUBLIC_ID_PREFIX, Spec, Status, TYPE_KEY, conformance_type, identity_name,
};
use basable_testkit::{CommitFault, CommitFaultProxy};
use sqlx::Row as _;
use uuid::Uuid;

fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[tokio::test]
async fn envelope_identity_round_trips_and_conflicts() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let identity = NamespacedName::new(h.namespace, "alpha");
    let holder = Uuid::new_v4();
    let spec = Spec {
        widgets: 1,
        content: "v1".into(),
    };
    let r = h
        .store
        .create(
            holder,
            identity.clone(),
            &spec,
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap();

    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(
        obj.meta.name, identity,
        "the identity round-trips through the read model"
    );
    assert_eq!(obj.meta.external_id, h.store.public_id(holder));
    assert!(
        obj.meta
            .external_id
            .starts_with(&format!("{PUBLIC_ID_PREFIX}_"))
    );
    assert_eq!(obj.spec, spec);
    assert_eq!(obj.status, Status::default());
    assert_eq!((obj.meta.generation, obj.meta.observed_generation), (1, 0));
    assert_eq!(obj.meta.phase, Phase::Pending);
    assert_eq!(obj.meta.next_reconcile_at, SCHEDULE_IMMEDIATE);
    assert!(!obj.observed_current() && !obj.deleting() && !obj.meta.parked());

    // The (namespace, name) pair has one live holder per type.
    let err = h
        .store
        .create(
            Uuid::new_v4(),
            identity.clone(),
            &spec,
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NameTaken { .. }), "{err}");

    // The same name in a different namespace is a different identity.
    h.store
        .create(
            Uuid::new_v4(),
            NamespacedName::new(Uuid::new_v4(), "alpha"),
            &spec,
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap();

    // Identity is required in full, rejected before anything is written.
    for bad in [
        NamespacedName::new(Uuid::nil(), "no-namespace"),
        NamespacedName::new(h.namespace, ""),
        NamespacedName::new(Uuid::nil(), ""),
    ] {
        let err = h
            .store
            .create(
                Uuid::new_v4(),
                bad,
                &spec,
                &Status::default(),
                CreateOptions::none(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)), "{err}");
    }

    // Adoption by id requires the identity the object was born with.
    let adopted = h
        .store
        .create(
            holder,
            identity.clone(),
            &spec,
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap();
    assert_eq!(adopted, r);
    let err = h
        .store
        .create(
            holder,
            NamespacedName::new(h.namespace, "renamed"),
            &spec,
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidConfig(_)), "{err}");

    // Uniqueness is scoped per type: the envelope parent's unique index
    // leads with the partition key, so the orders type may hold the same
    // pair. An envelope-only insert into that partition, rolled back.
    let mut tx = h.db.superuser().begin().await.unwrap();
    let other = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO nano_orders.processing_object_order
             (processing_object_type_key, id, external_id, name, namespace)
         VALUES (1, $1, $2, $3, $4)",
    )
    .bind(other)
    .bind(basable_publicid::encode("ord", other))
    .bind(&identity.name)
    .bind(identity.namespace)
    .execute(&mut *tx)
    .await
    .expect("a different type may hold the same (namespace, name)");
    tx.rollback().await.unwrap();
    h.finish().await;
}

#[tokio::test]
async fn external_id_is_not_nullable() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let (nullable,): (String,) = sqlx::query_as(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_schema = 'basable' AND table_name = 'processing_object'
           AND column_name = 'external_id'",
    )
    .fetch_one(h.db.superuser())
    .await
    .unwrap();
    assert_eq!(nullable, "NO");
    h.finish().await;
}

#[tokio::test]
async fn the_name_is_released_by_deletion_intent() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let identity = NamespacedName::new(h.namespace, "phoenix");
    let holder = h
        .store
        .create(
            Uuid::new_v4(),
            identity.clone(),
            &Spec {
                widgets: 1,
                content: "holder".into(),
            },
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap();

    assert!(h.store.mark_deleted(&holder).await.unwrap());
    assert!(
        !h.store.mark_deleted(&holder).await.unwrap(),
        "a repeat request changes nothing"
    );

    // A deleting envelope is not a live identity holder: the partial unique
    // index releases its name without physical deletion.
    let reborn = h
        .store
        .create(
            Uuid::new_v4(),
            identity,
            &Spec {
                widgets: 3,
                content: "reborn".into(),
            },
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap();
    assert_ne!(holder.id, reborn.id, "the name transfers to a new object");

    let old = h.store.read(&holder).await.unwrap();
    assert!(
        old.deleting(),
        "releasing the name does not remove the old envelope"
    );
    assert_eq!(old.meta.generation, 2, "teardown is new intent");
    assert!(!old.observed_current());

    // Intent writes are refused on a deleting object; a nudge is not.
    let err = h
        .store
        .update_spec(&holder, |_, _| Ok(()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Deleting(_)), "{err}");
    h.store.nudge(&holder).await.unwrap();
    // Adopting a deleting id is refused too.
    let err = h
        .store
        .create(
            holder.id,
            h.identity(holder.id),
            &Spec::default(),
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Deleting(_)), "{err}");
    h.finish().await;
}

#[tokio::test]
async fn create_adoption_checks_labels() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let id = Uuid::new_v4();
    let spec = Spec {
        widgets: 1,
        content: String::new(),
    };
    let cloud = CreateOptions::labels(labels(&[("infra_type", "cloud")]));
    h.store
        .create(id, h.identity(id), &spec, &Status::default(), cloud.clone())
        .await
        .unwrap();
    let obj = h.store.read(&h.store.r#ref(id)).await.unwrap();
    assert_eq!(obj.meta.labels, labels(&[("infra_type", "cloud")]));

    // Same id + same labels: the retry adopts.
    h.store
        .create(id, h.identity(id), &spec, &Status::default(), cloud)
        .await
        .expect("an identical retry adopts");

    // Same id + different labels: refused — believing labels landed that
    // did not would misroute the object for its whole lifetime.
    let err = h
        .store
        .create(
            id,
            h.identity(id),
            &spec,
            &Status::default(),
            CreateOptions::labels(labels(&[("infra_type", "bare_metal")])),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidConfig(_)), "{err}");

    // Same id + no labels at all: also a different identity.
    let err = h
        .store
        .create(
            id,
            h.identity(id),
            &spec,
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidConfig(_)), "{err}");

    // Invalid labels are refused before anything is written.
    let err = h
        .store
        .create(
            Uuid::new_v4(),
            h.identity(Uuid::new_v4()),
            &spec,
            &Status::default(),
            CreateOptions::labels(labels(&[("Infra", "x")])),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidConfig(_)), "{err}");
    h.finish().await;
}

#[tokio::test]
async fn update_spec_is_read_modify_write_under_the_lock() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let r = h
        .create(Spec {
            widgets: 1,
            content: "a".into(),
        })
        .await
        .unwrap();
    let before = h.meta(&r).await.unwrap();

    h.store
        .update_spec(&r, |spec, status| {
            assert_eq!(
                status,
                &Status::default(),
                "the closure sees the committed status"
            );
            spec.widgets += 1;
            Ok(())
        })
        .await
        .unwrap();
    h.store
        .update_spec(&r, |spec, _| {
            spec.content.push('b');
            Ok(())
        })
        .await
        .unwrap();

    let obj = h.store.read(&r).await.unwrap();
    assert_eq!(
        obj.spec,
        Spec {
            widgets: 2,
            content: "ab".into()
        },
        "each mutation is a delta"
    );
    assert_eq!(obj.meta.generation, 3);
    assert_eq!(obj.meta.wake_seq, 2);
    assert!(obj.meta.generation_changed_at > before.generation_changed_at);
    assert!(!obj.observed_current());

    // A refusing closure writes nothing.
    let err = h
        .store
        .update_spec(&r, |_, _| Err("not in a claimable phase".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Mutate(_)), "{err}");
    assert_eq!(h.meta(&r).await.unwrap().generation, 3);

    // A nudge advances the wake fence and pulls the schedule forward
    // without inventing intent.
    h.store.nudge(&r).await.unwrap();
    let after = h.meta(&r).await.unwrap();
    assert_eq!((after.generation, after.wake_seq), (3, 3));
    assert!(after.next_reconcile_at <= chrono::Utc::now());

    // A ref of another type, a nil id, an unknown id.
    let wrong = basable_processingobject::Ref::new("order", r.id);
    assert!(matches!(
        h.store.nudge(&wrong).await,
        Err(Error::InvalidConfig(_))
    ));
    let unknown = h.store.r#ref(Uuid::new_v4());
    assert!(matches!(
        h.store.read(&unknown).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        h.store.nudge(&unknown).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        h.store.mark_deleted(&unknown).await,
        Err(Error::NotFound(_))
    ));
    h.finish().await;
}

#[tokio::test]
async fn read_many_keeps_input_order_and_skips_the_gone() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let a = h
        .create(Spec {
            widgets: 1,
            content: "a".into(),
        })
        .await
        .unwrap();
    let b = h
        .create(Spec {
            widgets: 2,
            content: "b".into(),
        })
        .await
        .unwrap();
    let gone = Uuid::new_v4();
    let objs = h.store.read_many(&[b.id, gone, a.id]).await.unwrap();
    assert_eq!(
        objs.iter().map(|o| o.id).collect::<Vec<_>>(),
        vec![b.id, a.id]
    );
    assert_eq!(objs[0].spec.content, "b");
    assert!(h.store.read_many(&[]).await.unwrap().is_empty());

    // An envelope without its typed rows is corruption, never a case.
    sqlx::query("DELETE FROM nano_conformance.conformance_spec WHERE id = $1")
        .bind(a.id)
        .execute(h.db.superuser())
        .await
        .unwrap();
    let err = h.store.read(&a).await.unwrap_err();
    assert!(matches!(err, Error::Invariant(_)), "{err}");
    h.finish().await;
}

#[tokio::test]
async fn bind_verifies_the_registry_and_the_partition() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    // Wrong key for the name: the registry says otherwise.
    let decl = basable_processingobject::ProcessingObjectType::new(
        "conformance",
        31999,
        "ek",
        basable_processingobject_testkit::ConformanceAdapter,
    );
    let err = TypedStore::bind(&h.pool, decl)
        .await
        .expect_err("an unregistered key is refused");
    assert!(matches!(err, Error::InvalidConfig(_)), "{err}");
    // Registered key, wrong name.
    let decl = basable_processingobject::ProcessingObjectType::new(
        "widget",
        TYPE_KEY,
        "ek",
        basable_processingobject_testkit::ConformanceAdapter,
    );
    let err = TypedStore::bind(&h.pool, decl).await.unwrap_err();
    assert!(matches!(err, Error::InvalidConfig(_)), "{err}");
    // The probe left nothing behind.
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM nano_conformance.processing_object_conformance WHERE name = 'schema-probe'")
            .fetch_one(h.db.superuser())
            .await
            .unwrap();
    assert_eq!(n, 0);
    assert_eq!(conformance_type().name, h.store.name());
    h.finish().await;
}

/// Behaviour 18: a create whose COMMIT acknowledgement is lost surfaces the
/// ambiguity; replaying with the SAME id adopts (applied) or creates fresh
/// (rolled back). Either way exactly one spec row exists.
async fn create_is_idempotent_under_ambiguous_commit(mode: CommitFault) {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let (proxy, options) = CommitFaultProxy::for_options(h.db.app_options())
        .await
        .unwrap();
    let pool = NanoPool::<Conformance>::connect(options, PoolConfig::default())
        .await
        .unwrap();
    let store = TypedStore::bind(&pool, conformance_type()).await.unwrap();

    let id = Uuid::new_v4();
    let identity = NamespacedName::new(Uuid::new_v4(), identity_name(id));
    let spec = Spec {
        widgets: 2,
        content: "stable".into(),
    };

    proxy.arm(mode);
    let err = store
        .create(
            id,
            identity.clone(),
            &spec,
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .expect_err("an ambiguous create commit must surface as an error");
    assert!(err.is_commit_unknown(), "{err}");
    assert!(err.to_string().contains("commit outcome unknown"), "{err}");
    assert!(!proxy.is_armed());

    let r = store
        .create(
            id,
            identity.clone(),
            &spec,
            &Status::default(),
            CreateOptions::none(),
        )
        .await
        .unwrap();
    assert_eq!(r.id, id, "the client-minted id is the stable identity");

    let (rows,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM nano_conformance.conformance_spec WHERE id = $1")
            .bind(id)
            .fetch_one(h.db.superuser())
            .await
            .unwrap();
    assert_eq!(
        rows, 1,
        "exactly one spec row after the ambiguous create and the replay"
    );

    let obj = store.read(&r).await.unwrap();
    assert_eq!(obj.spec.content, "stable");
    assert_eq!(obj.meta.name, identity);
    pool.close().await;
    drop(proxy);
    h.finish().await;
}

#[tokio::test]
async fn create_is_idempotent_when_the_ambiguous_commit_applied() {
    create_is_idempotent_under_ambiguous_commit(CommitFault::Applied).await;
}

#[tokio::test]
async fn create_is_idempotent_when_the_ambiguous_commit_rolled_back() {
    create_is_idempotent_under_ambiguous_commit(CommitFault::RolledBack).await;
}

#[tokio::test]
async fn a_create_publishes_a_wake_and_the_row_lands_in_the_partition() {
    let Some(h) = Harness::from_env().await else {
        return;
    };
    let mut listener = sqlx::postgres::PgListener::connect_with(h.db.superuser())
        .await
        .unwrap();
    listener.listen("processing_object_wake").await.unwrap();
    let r = h
        .create(Spec {
            widgets: 1,
            content: "w".into(),
        })
        .await
        .unwrap();
    let n = tokio::time::timeout(Duration::from_secs(5), listener.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n.payload(), "conformance");

    let row = sqlx::query(
        "SELECT processing_object_type_key, tableoid::regclass::text AS tbl
         FROM basable.processing_object WHERE id = $1",
    )
    .bind(r.id)
    .fetch_one(h.db.superuser())
    .await
    .unwrap();
    assert_eq!(row.get::<i16, _>("processing_object_type_key"), TYPE_KEY);
    assert_eq!(
        row.get::<String, _>("tbl"),
        "nano_conformance.processing_object_conformance"
    );
    // The listener holds one of the superuser pool's connections; the pool
    // waits for it on close, so it goes first.
    drop(listener);
    h.finish().await;
}
