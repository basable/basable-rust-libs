//! The intent-write path: `update_spec` (invariant 2), `mark_deleted`
//! (invariant 6) and `nudge` — the three ways an object becomes due outside
//! its own schedule. None of them touch typed status (invariant 3), and none
//! touch `next_reconcile_at` on a generation change: `due_at` is generated
//! from `observed_generation < generation`, so advancing the generation
//! re-arms scheduling by construction, including un-parking a blocked
//! object.
//!
//! `wake_seq` is the row's mutation clock, monotonic for its whole lifetime:
//! every accepted mutation of any kind advances it. It never resets.
//!
//! All three writers lock the envelope row first, so intent writes serialize
//! with each other and with completion. They WAIT on the row lock, which
//! every framework path holds only for a few I/O-free statements.
//!
//! `update_spec` is deliberately read-modify-write: the lock serializes
//! writers, but a writer that computes its row from a read taken BEFORE the
//! lock resurrects stale fields. Reading under the lock makes every mutation
//! a delta against the committed row.

use basable_core::BoxError;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::decl::Adapter;
use crate::error::Error;
use crate::model::{Ref, Row};
use crate::store::{TypedStore, publish_wake};
use crate::tx::Tx;

impl<S, T, A> TypedStore<S, T, A>
where
    S: Send + Sync + 'static,
    T: Send + Sync + 'static,
    A: Adapter<S, T>,
{
    /// Locks the envelope for an intent write and returns `deleted_at`:
    /// `update_spec` rejects a deleting object, `mark_deleted` uses it for
    /// idempotency, `nudge` locks only.
    async fn lock_for_intent(
        &self,
        conn: &mut PgConnection,
        r: &Ref,
        op: &str,
    ) -> Result<Option<DateTime<Utc>>, Error> {
        let row: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as(&format!(
            "SELECT deleted_at FROM {} WHERE id = $1 FOR UPDATE",
            self.inner.partition
        ))
        .bind(r.id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| Error::sql(format!("{op} {r}: lock envelope"), e))?;
        match row {
            Some((deleted_at,)) => Ok(deleted_at),
            None => Err(Error::NotFound(r.clone())),
        }
    }

    /// Loads the typed row for one object inside a framework transaction. A
    /// missing row under an existing envelope is corruption (invariant 1).
    pub(crate) async fn read_row_tx(
        &self,
        conn: &mut PgConnection,
        r: &Ref,
    ) -> Result<Row<S, T>, Error> {
        let mut rtx = Tx::new(conn);
        let mut rows = self
            .inner
            .decl
            .adapter
            .read_rows(&mut rtx, std::slice::from_ref(&r.id))
            .await
            .map_err(|e| Error::sql(format!("read typed row {r}"), e))?;
        if rows.len() != 1 || rows[0].id != r.id {
            return Err(Error::Invariant(format!("envelope {r} has no typed row")));
        }
        Ok(rows.swap_remove(0))
    }

    /// Accepts new desired state: locks the envelope, loads the current typed
    /// row, applies `mutate` to the spec, writes the result through the
    /// adapter, and advances the envelope in the same transaction —
    /// generation, `generation_changed_at`, wake fence, retry state reset —
    /// then publishes a wake (invariant 2).
    ///
    /// `mutate` also sees the last committed status, by shared reference: a
    /// status-gated mutation (a CAS admitting an object only in a claimable
    /// phase) must decide under the same envelope lock as its write. It is
    /// synchronous on purpose — no I/O under the lock — and its error aborts
    /// the update with nothing written ([`Error::Mutate`]). The closure is
    /// serialized against completions, not against a running attempt: reject
    /// on status, but never assume it still holds after commit.
    ///
    /// Every call is new intent and unconditionally advances the generation
    /// (which also grants a fresh retry budget and re-arms a blocked object).
    /// A caller with nothing to change should `nudge` instead. Phase and
    /// `last_error` are left alone: they describe what was observed.
    pub async fn update_spec<F>(&self, r: &Ref, mutate: F) -> Result<(), Error>
    where
        F: FnOnce(&mut S, &T) -> Result<(), BoxError> + Send,
    {
        self.check_ref(r)?;
        let op = "update spec";
        let mut tx = self
            .inner
            .pool
            .begin()
            .await
            .map_err(|e| Error::sql(format!("{op} {r}: begin"), e))?;
        if self.lock_for_intent(&mut tx, r, op).await?.is_some() {
            return Err(Error::Deleting(r.clone()));
        }
        let mut row = self.read_row_tx(&mut tx, r).await?;
        mutate(&mut row.spec, &row.status).map_err(Error::Mutate)?;
        {
            let conn: &mut PgConnection = &mut tx;
            let mut rtx = Tx::new(conn);
            self.inner
                .decl
                .adapter
                .write_spec(&mut rtx, r, &row.spec)
                .await
                .map_err(|e| Error::sql(format!("{op} {r}: write typed spec"), e))?;
        }
        sqlx::query(&format!(
            "UPDATE {}
             SET generation = generation + 1,
                 generation_changed_at = clock_timestamp(),
                 wake_seq = wake_seq + 1,
                 attempts = 0
             WHERE id = $1",
            self.inner.partition
        ))
        .bind(r.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| Error::sql(format!("{op} {r}: advance envelope"), e))?;
        publish_wake(&mut tx, self.inner.decl.name).await?;
        tx.commit()
            .await
            .map_err(|e| Error::commit(format!("{op} {r}"), e))?;
        Ok(())
    }

    /// Requests deletion: one-way, idempotent (invariant 6). The first call
    /// stamps `deleted_at` and advances the generation — teardown is new
    /// intent, immediately due, with a fresh retry budget — and returns
    /// `true`. Repeat calls change nothing and return `false`; the request
    /// already stands. [`Error::NotFound`] means the envelope is gone, which
    /// for a deletion caller usually reads as "teardown already finished".
    pub async fn mark_deleted(&self, r: &Ref) -> Result<bool, Error> {
        self.check_ref(r)?;
        let op = "mark deleted";
        let mut tx = self
            .inner
            .pool
            .begin()
            .await
            .map_err(|e| Error::sql(format!("{op} {r}: begin"), e))?;
        if self.lock_for_intent(&mut tx, r, op).await?.is_some() {
            return Ok(false);
        }
        sqlx::query(&format!(
            "UPDATE {}
             SET deleted_at = clock_timestamp(),
                 generation = generation + 1,
                 generation_changed_at = clock_timestamp(),
                 wake_seq = wake_seq + 1,
                 attempts = 0
             WHERE id = $1",
            self.inner.partition
        ))
        .bind(r.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| Error::sql(format!("{op} {r}: stamp deletion"), e))?;
        publish_wake(&mut tx, self.inner.decl.name).await?;
        tx.commit()
            .await
            .map_err(|e| Error::commit(format!("{op} {r}"), e))?;
        Ok(true)
    }

    /// Makes an object due now without inventing intent: wake fence
    /// advanced, schedule pulled forward, generation untouched. A nudged
    /// blocked object gets exactly one fresh attempt (its exhausted retry
    /// budget stands), so nudging cannot turn a poison object into a hot
    /// loop. A deleting object may be nudged: teardown is live work.
    pub async fn nudge(&self, r: &Ref) -> Result<(), Error> {
        self.check_ref(r)?;
        let op = "nudge";
        let mut tx = self
            .inner
            .pool
            .begin()
            .await
            .map_err(|e| Error::sql(format!("{op} {r}: begin"), e))?;
        self.lock_for_intent(&mut tx, r, op).await?;
        sqlx::query(&format!(
            "UPDATE {}
             SET wake_seq = wake_seq + 1,
                 next_reconcile_at = LEAST(next_reconcile_at, clock_timestamp())
             WHERE id = $1",
            self.inner.partition
        ))
        .bind(r.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| Error::sql(format!("{op} {r}: advance wake"), e))?;
        publish_wake(&mut tx, self.inner.decl.name).await?;
        tx.commit()
            .await
            .map_err(|e| Error::commit(format!("{op} {r}"), e))?;
        Ok(())
    }
}
