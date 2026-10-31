//! The directory: `basable.pubsub_replica` and `basable.pubsub_subscription`
//! (the framework migration `20260101000003_basable_pubsub_directory.sql`).
//! Every time comparison is the database's `now()`, so the replicas' clocks
//! never have to agree.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{Error, Inner};

/// A replica to deliver to, as the directory names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) id: Uuid,
    pub(crate) address: String,
    pub(crate) fingerprint: [u8; 32],
}

fn pool(inner: &Inner) -> &PgPool {
    inner
        .pool
        .as_ref()
        .expect("pubsub: a directory call on a local bus")
}

/// Writes this replica's row and a row for every channel it subscribes to,
/// in one transaction. Called with the `dir` lock held: at `Bus::new`, and
/// again whenever the replica finds itself swept.
pub(crate) async fn register(inner: &Inner) -> Result<(), Error> {
    let fingerprint = inner
        .identity
        .as_ref()
        .expect("pubsub: a registered replica has an identity")
        .fingerprint;
    let mut tx = pool(inner).begin().await?;
    sqlx::query(
        "INSERT INTO basable.pubsub_replica (id, address, certificate_sha256)
         VALUES ($1, $2, $3)
         ON CONFLICT (id) DO UPDATE
         SET address = EXCLUDED.address,
             certificate_sha256 = EXCLUDED.certificate_sha256,
             heartbeat_at = now()",
    )
    .bind(inner.id)
    .bind(&inner.address)
    .bind(&fingerprint[..])
    .execute(&mut *tx)
    .await?;
    let channels: Vec<String> = inner
        .subs
        .read()
        .expect("subs lock")
        .keys()
        .cloned()
        .collect();
    for channel in channels {
        sqlx::query(
            "INSERT INTO basable.pubsub_subscription (channel, replica_id)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(channel)
        .bind(inner.id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn insert(inner: &Inner, channel: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO basable.pubsub_subscription (channel, replica_id)
         VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(channel)
    .bind(inner.id)
    .execute(pool(inner))
    .await?;
    Ok(())
}

/// Writes this replica's row for `channel`. When the replica's own row is
/// gone (swept while its heartbeat was late), it registers again with the
/// channel included and reports that it did. Called with the `dir` lock
/// held.
pub(crate) async fn insert_subscription(inner: &Inner, channel: &str) -> Result<bool, Error> {
    match insert(inner, channel).await {
        Ok(()) => Ok(false),
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23503") => {
            // foreign_key_violation: this replica's row is gone.
            register(inner).await?;
            insert(inner, channel).await?;
            Ok(true)
        }
        Err(e) => Err(e.into()),
    }
}

pub(crate) async fn delete_subscription(inner: &Inner, channel: &str) -> Result<(), Error> {
    sqlx::query("DELETE FROM basable.pubsub_subscription WHERE channel = $1 AND replica_id = $2")
        .bind(channel)
        .bind(inner.id)
        .execute(pool(inner))
        .await?;
    Ok(())
}

/// Stamps this replica's row. A row that is gone means it was swept: it
/// registers again and reports that it did.
pub(crate) async fn heartbeat(inner: &Inner) -> Result<bool, Error> {
    let stamped =
        sqlx::query("UPDATE basable.pubsub_replica SET heartbeat_at = now() WHERE id = $1")
            .bind(inner.id)
            .execute(pool(inner))
            .await?
            .rows_affected();
    if stamped > 0 {
        return Ok(false);
    }
    let _dir = inner.dir.lock().await;
    register(inner).await?;
    Ok(true)
}

/// Deletes every replica whose heartbeat is older than the expiry; their
/// subscriptions go with them (`ON DELETE CASCADE`). Every replica runs it;
/// it is one idempotent statement.
pub(crate) async fn sweep(inner: &Inner) -> Result<u64, Error> {
    let swept = sqlx::query(
        "DELETE FROM basable.pubsub_replica
         WHERE heartbeat_at < now() - make_interval(secs => $1)",
    )
    .bind(inner.cfg.expiry.as_secs_f64())
    .execute(pool(inner))
    .await?
    .rows_affected();
    Ok(swept)
}

pub(crate) async fn deregister(inner: &Inner) -> Result<(), Error> {
    sqlx::query("DELETE FROM basable.pubsub_replica WHERE id = $1")
        .bind(inner.id)
        .execute(pool(inner))
        .await?;
    Ok(())
}

fn fingerprint(raw: &[u8], id: Uuid) -> Result<[u8; 32], Error> {
    raw.try_into().map_err(|_| {
        Error::Config(format!(
            "replica {id} has a {}-byte certificate fingerprint",
            raw.len()
        ))
    })
}

/// The other live replicas subscribed to `channel`. One whose heartbeat is
/// past the expiry is left out even before the sweep deletes it.
pub(crate) async fn subscribers(inner: &Inner, channel: &str) -> Result<Vec<Target>, Error> {
    let rows = sqlx::query(
        "SELECT r.id, r.address, r.certificate_sha256
         FROM basable.pubsub_subscription s
         JOIN basable.pubsub_replica r ON r.id = s.replica_id
         WHERE s.channel = $1
           AND r.id <> $2
           AND r.heartbeat_at >= now() - make_interval(secs => $3)",
    )
    .bind(channel)
    .bind(inner.id)
    .bind(inner.cfg.expiry.as_secs_f64())
    .fetch_all(pool(inner))
    .await?;
    rows.iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            let raw: Vec<u8> = row.try_get("certificate_sha256")?;
            Ok(Target {
                id,
                address: row.try_get("address")?,
                fingerprint: fingerprint(&raw, id)?,
            })
        })
        .collect()
}

/// The certificate fingerprint the directory lists for a live replica, the
/// only certificate accepted from a peer claiming its id.
pub(crate) async fn live_fingerprint(inner: &Inner, id: Uuid) -> Result<Option<[u8; 32]>, Error> {
    let row = sqlx::query(
        "SELECT certificate_sha256 FROM basable.pubsub_replica
         WHERE id = $1 AND heartbeat_at >= now() - make_interval(secs => $2)",
    )
    .bind(id)
    .bind(inner.cfg.expiry.as_secs_f64())
    .fetch_optional(pool(inner))
    .await?;
    match row {
        None => Ok(None),
        Some(row) => {
            let raw: Vec<u8> = row.try_get("certificate_sha256")?;
            Ok(Some(fingerprint(&raw, id)?))
        }
    }
}
