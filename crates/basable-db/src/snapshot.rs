//! A consistent read-only snapshot.

use sqlx::{Executor, PgPool, Postgres, Transaction};

/// Begins a `REPEATABLE READ READ ONLY` transaction: every statement in it
/// sees the same snapshot, and Postgres refuses a write in it. The
/// processing-object read model reads a batch of objects this way so their
/// spec and status rows agree with the envelope rows read a statement
/// earlier.
pub async fn begin_snapshot(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await?;
    Ok(tx)
}
