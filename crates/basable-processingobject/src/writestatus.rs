//! The mid-attempt half of invariant 3: a fenced status write that does NOT
//! end the attempt. Completion remains the only path that settles the
//! envelope; this one writes typed status under the same authority and
//! touches nothing else.
//!
//! It exists for exactly one pattern completion cannot express: a write
//! that must be durable BEFORE a remote effect goes out, made by the SAME
//! attempt that then sends it — declare-before-I/O. Split across two passes
//! the sender cannot tell "declared, nobody has sent" from "a predecessor
//! sent and died". Written mid-attempt, the writer knows in memory that
//! nothing has gone out yet, and every OTHER attempt that finds the marker
//! on claim treats the effect as possibly sent and resolves instead of
//! re-sending. Everything else — durable phase checkpoints,
//! persist-then-act — stays `Outcome::requeue_now(status)`.

use basable_core::Deadline;
use sqlx::PgConnection;

use crate::claim::Claim;
use crate::decl::Adapter;
use crate::error::Error;
use crate::tx::Tx;

impl<S, T, A> Claim<S, T, A>
where
    S: Send + Sync + 'static,
    T: Send + Sync + 'static,
    A: Adapter<S, T>,
{
    /// Writes typed status now, under exact claim authority, without
    /// completing the attempt: locks the envelope, re-proves local
    /// ownership, compares the claim token, writes the whole status row
    /// through the adapter, extends the lease (a heartbeat for free) and
    /// commits. Scheduling, observed generation, attempts, phase and
    /// `last_error` are untouched.
    ///
    /// `Ok`: the status is durable and `object.status` now holds it, so a
    /// completion derived from it carries the write. [`Error::Fenced`]:
    /// permanent, nothing written, the claim closed — a successor holds the
    /// object (or the envelope is gone). [`Error::CommitUnknown`]: the write
    /// may or may not have landed; the claim stays open and the caller
    /// decides (for the declare pattern the safe verdict is the same either
    /// way: do not send, return `Retry`). Any other error: nothing landed
    /// and the claim stays open.
    ///
    /// A superseded or woken attempt still writes: an outstanding-effect
    /// marker is orthogonal to intent, and the completion will leave the
    /// object due now as it always does.
    pub async fn write_status(&mut self, status: T) -> Result<(), Error> {
        self.lease.require_proof()?;
        let r = self.r#ref();
        let op = format!("write status {r}");
        let partition = self.store.inner.partition.clone();
        let pre_send = Deadline::after(self.lease.duration);

        let mut tx = self
            .store
            .inner
            .pool
            .begin()
            .await
            .map_err(|e| Error::sql(format!("{op}: begin"), e))?;
        let token: Option<(Option<uuid::Uuid>,)> = sqlx::query_as(&format!(
            "SELECT claim_token FROM {partition} WHERE id = $1 FOR UPDATE"
        ))
        .bind(self.object.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| Error::sql(format!("{op}: lock"), e))?;
        let Some((token,)) = token else {
            // Only a completed teardown removes rows, and this attempt is
            // still running: a successor adopted the lease and finished one.
            self.lease.close();
            return Err(Error::Fenced);
        };
        // Re-prove AFTER the lock: a lock-delayed write must not land after
        // self-fencing (the same ordering as completion).
        self.lease.require_proof()?;
        if token != Some(self.lease.token) {
            self.lease.close();
            return Err(Error::Fenced);
        }

        {
            let conn: &mut PgConnection = &mut tx;
            let mut rtx = Tx::new(conn);
            // A rejected row (constraint or otherwise) writes nothing: the
            // whole transaction rolls back and the claim stays open for the
            // caller's own verdict.
            self.store
                .inner
                .decl
                .adapter
                .write_status(&mut rtx, &r, &status)
                .await
                .map_err(|e| Error::sql(op.clone(), e))?;
        }
        sqlx::query(&format!(
            "UPDATE {partition} SET lease_expires_at = clock_timestamp() + make_interval(secs => $2)
             WHERE id = $1"
        ))
        .bind(self.object.id)
        .bind(self.lease.duration.as_secs_f64())
        .execute(&mut *tx)
        .await
        .map_err(|e| Error::sql(format!("{op}: extend lease"), e))?;
        tx.commit()
            .await
            .map_err(|e| Error::commit(op.clone(), e))?;

        // Durable. Advance the attempt's view so a completion derived from
        // it carries this write, and extend the proof from the pre-send
        // reading (never past the database lease).
        self.object.status = status;
        self.lease.extend(pre_send);
        Ok(())
    }
}
