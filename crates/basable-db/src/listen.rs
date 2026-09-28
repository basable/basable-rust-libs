//! Returning a connection that ran `LISTEN`.

use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::{Connection, Executor, Postgres};

/// Bounds the `UNLISTEN` on a connection whose listen loop has just ended,
/// often because that connection broke.
const UNLISTEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Returns a connection that ran `LISTEN` to its pool with its subscriptions
/// cleared, or discards it when that cannot be confirmed.
///
/// A pool does not reset session state on release and `LISTEN` is
/// session-scoped: a plain release hands the connection back still
/// subscribed, so every later `NOTIFY` wakes a backend that is now serving
/// unrelated queries and drops the notification, and every reconnect of a
/// listen loop leaves one more such connection behind. If the `UNLISTEN`
/// fails or times out the connection is detached from the pool and closed
/// instead: a connection whose subscriptions are unknown is worse than a
/// fresh one, and listen loops reconnect rarely enough that the cost is
/// nothing. The port of the Go `db.ReleaseListenConn`.
pub async fn release_listen_conn(mut conn: PoolConnection<Postgres>) {
    match tokio::time::timeout(UNLISTEN_TIMEOUT, conn.execute("UNLISTEN *")).await {
        Ok(Ok(_)) => drop(conn),
        outcome => {
            match outcome {
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "UNLISTEN failed on a listen connection; discarding it")
                }
                _ => tracing::warn!("UNLISTEN timed out on a listen connection; discarding it"),
            }
            let detached = conn.detach();
            if let Err(e) = detached.close().await {
                tracing::warn!(error = %e, "closing a discarded listen connection failed; its backend may linger");
            }
        }
    }
}
