//! Fenced completion (invariant 5): one transaction that verifies exact
//! claim authority under the envelope lock, writes typed status through the
//! single-provenance adapter path (invariant 3), settles the envelope —
//! observed generation, phase, retry state, schedule, claim cleared — and,
//! for a confirmed teardown, removes the object (invariant 6). It commits
//! whole or not at all.
//!
//! Three deliberate behaviours:
//!
//! - A superseded or woken completion (the claim-time generation or wake
//!   sequence is no longer current) still commits its status observation
//!   but touches nothing else and leaves the object due now. A wake can
//!   never be consumed by an attempt that predates it.
//! - Status and deletion writes run under savepoints. A status row rejected
//!   by a constraint is deterministic: without the savepoint the completion
//!   would abort forever, a silent livelock. Rolling back to the savepoint
//!   converts it into a loud classified `Retry` that backs off.
//! - An ambiguous COMMIT is retried once. If the retry finds the idempotent
//!   signature of a landed completion — token cleared, our generation
//!   observed, AND the local ownership proof still live — it adopts it but
//!   reports [`Completion::Unknown`]: SOME completion of ours landed, but
//!   not WHICH normalized form, so no post-completion callback runs. A retry
//!   whose proof has lapsed is fenced instead.

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use basable_core::BoxError;
use basable_db::sqlstate;
use chrono::{DateTime, Utc};
use sqlx::{Executor, PgConnection, Row as _};

use crate::claim::Claim;
use crate::decl::Adapter;
use crate::error::Error;
use crate::model::{Meta, Object, Phase, SCHEDULE_PARKED};
use crate::outcome::{Outcome, Schedule};
use crate::store::publish_wake;
use crate::store_read::{META_COLUMNS, scan_meta};
use crate::tx::Tx;

/// What a completion transaction durably committed.
#[derive(Debug)]
pub enum Completion<T> {
    /// The completion landed with this normalized outcome — what actually
    /// committed, never merely what the reconciler requested.
    Committed {
        /// The committed outcome.
        outcome: Outcome<T>,
        /// The claim-time generation was stale: the observation committed
        /// and the object was left due now.
        superseded: bool,
        /// The claim-time wake sequence was stale: likewise.
        woken: bool,
    },
    /// An ambiguity adoption: a completion of this attempt provably landed,
    /// but its normalized form is unknown. Run no callbacks.
    Unknown,
}

impl<T> Completion<T> {
    /// The committed outcome, `None` for [`Completion::Unknown`].
    pub fn outcome(&self) -> Option<&Outcome<T>> {
        match self {
            Completion::Committed { outcome, .. } => Some(outcome),
            Completion::Unknown => None,
        }
    }

    /// Whether the claim-time generation was stale.
    pub fn superseded(&self) -> bool {
        matches!(
            self,
            Completion::Committed {
                superseded: true,
                ..
            }
        )
    }

    /// Whether the claim-time wake sequence was stale.
    pub fn woken(&self) -> bool {
        matches!(self, Completion::Committed { woken: true, .. })
    }

    /// Whether this is [`Completion::Unknown`].
    pub fn is_unknown(&self) -> bool {
        matches!(self, Completion::Unknown)
    }
}

/// A cause every retry of a completion can share.
type SharedError = Arc<dyn StdError + Send + Sync + 'static>;

/// A cause wrapped with context, keeping the inner one as `source`.
#[derive(Debug)]
struct Contextual {
    context: String,
    inner: SharedError,
}

impl fmt::Display for Contextual {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.inner)
    }
}

impl StdError for Contextual {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&*self.inner)
    }
}

/// A leaf cause.
#[derive(Debug)]
struct Message(String);

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl StdError for Message {}

/// A shared cause handed back out as a `BoxError`.
#[derive(Debug)]
struct Shared(SharedError);

impl fmt::Display for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl StdError for Shared {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0.source()
    }
}

fn wrap(context: impl Into<String>, inner: SharedError) -> SharedError {
    Arc::new(Contextual {
        context: context.into(),
        inner,
    })
}

fn message(text: impl Into<String>) -> SharedError {
    Arc::new(Message(text.into()))
}

/// The decision a completion commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Converged,
    Retry,
    Blocked,
    Delete,
    Settled,
}

/// The reconciler's verdict in a form every completion retry can reuse:
/// the status is cloned per try, the cause is shared.
pub(crate) struct Verdict<T> {
    decision: Decision,
    status: Option<T>,
    cause: Option<SharedError>,
    schedule: Schedule,
}

impl<T> Verdict<T> {
    /// From the attempt's result. An error from the reconciler (or the
    /// attempt runtime — a timeout, a panic) resolves to `Retry` with no
    /// status: the attempt observed nothing it could vouch for.
    pub(crate) fn from_result(out: Result<Outcome<T>, BoxError>) -> Verdict<T> {
        match out {
            Ok(Outcome::Converged { status, schedule }) => Verdict {
                decision: Decision::Converged,
                status,
                cause: None,
                schedule,
            },
            Ok(Outcome::Retry { status, cause }) => Verdict {
                decision: Decision::Retry,
                status,
                cause: Some(Arc::from(cause)),
                schedule: Schedule::Resync,
            },
            Ok(Outcome::Blocked { status, cause }) => Verdict {
                decision: Decision::Blocked,
                status,
                cause: Some(Arc::from(cause)),
                schedule: Schedule::Resync,
            },
            Ok(Outcome::Delete) => Verdict {
                decision: Decision::Delete,
                status: None,
                cause: None,
                schedule: Schedule::Resync,
            },
            Ok(Outcome::Settled { status }) => Verdict {
                decision: Decision::Settled,
                status,
                cause: None,
                schedule: Schedule::Resync,
            },
            Err(e) => Verdict {
                decision: Decision::Retry,
                status: None,
                cause: Some(Arc::from(e)),
                schedule: Schedule::Resync,
            },
        }
    }
}

/// The normalized completion: what will actually be committed, after
/// contract violations and escalation are applied.
pub(crate) struct Resolved<T> {
    pub(crate) decision: Decision,
    pub(crate) status: Option<T>,
    pub(crate) cause: Option<SharedError>,
    pub(crate) schedule: Schedule,
}

impl<T> Resolved<T> {
    /// Projects the resolved form back into the public [`Outcome`] for
    /// post-completion callbacks.
    fn into_outcome(self) -> Outcome<T> {
        let cause = |c: Option<SharedError>| -> BoxError {
            Box::new(Shared(c.unwrap_or_else(|| message("unspecified cause"))))
        };
        match self.decision {
            Decision::Converged => Outcome::Converged {
                status: self.status,
                schedule: self.schedule,
            },
            Decision::Retry => Outcome::Retry {
                status: self.status,
                cause: cause(self.cause),
            },
            Decision::Blocked => Outcome::Blocked {
                status: self.status,
                cause: cause(self.cause),
            },
            Decision::Delete => Outcome::Delete,
            Decision::Settled => Outcome::Settled {
                status: self.status,
            },
        }
    }
}

fn non_nil(cause: Option<SharedError>) -> SharedError {
    cause.unwrap_or_else(|| message("unspecified cause"))
}

/// Normalizes the verdict against the claim and policy:
///
/// - `Delete` on a non-deleting claim is a loud `Retry` (invariant 6);
/// - `Blocked` on a deleting claim is a loud `Retry` — a failed teardown
///   must not park; `Settled` is allowed as an explicit soft-delete
///   tombstone; `Converged`/`Retry` pass through because multi-pass
///   provider deletion waits;
/// - `Retry` escalates to `Blocked` when `max_attempts` is exhausted, never
///   for a deleting claim.
pub(crate) fn resolve<T: Clone>(
    v: &Verdict<T>,
    deleting: bool,
    current_attempts: i32,
    max_attempts: u32,
) -> Resolved<T> {
    let mut res = Resolved {
        decision: v.decision,
        status: v.status.clone(),
        cause: v.cause.clone(),
        schedule: v.schedule,
    };
    if res.decision == Decision::Delete && !deleting {
        res = Resolved {
            decision: Decision::Retry,
            status: None,
            cause: Some(message(
                "invalid reconcile outcome: Delete on non-deleting object",
            )),
            schedule: Schedule::Resync,
        };
    }
    if deleting && res.decision == Decision::Blocked {
        res.decision = Decision::Retry;
        res.cause = Some(wrap("teardown must not park", non_nil(res.cause)));
    }
    if res.decision == Decision::Retry
        && !deleting
        && max_attempts > 0
        && u64::try_from(current_attempts).unwrap_or(0) + 1 >= u64::from(max_attempts)
    {
        res.decision = Decision::Blocked;
        res.cause = Some(wrap(
            format!("maximum attempts {max_attempts} exhausted"),
            non_nil(res.cause),
        ));
    }
    res
}

/// The envelope scheduling a resolved completion maps onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fields {
    pub(crate) phase: Phase,
    pub(crate) attempts: i32,
    pub(crate) delay: Duration,
    pub(crate) parked: bool,
}

/// Bounds a cause for the envelope's `last_error` column (schema CHECK: at
/// most 4096 bytes). The cut backs up to a char boundary, so the value is
/// always valid UTF-8 and never a class-22 error outside every savepoint.
pub(crate) fn error_text(cause: Option<&SharedError>) -> Option<String> {
    let cause = cause?;
    let mut s = cause.to_string();
    const MAX_BYTES: usize = 4000;
    if s.len() > MAX_BYTES {
        let mut cut = MAX_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str(" …(truncated)");
    }
    Some(s)
}

/// The envelope bookkeeping a completion commits: retry state, schedule
/// shape, and optionally phase + `last_error` — `None` leaves both
/// untouched (the superseded/woken shape).
struct Settle {
    phase: Option<Phase>,
    attempts: i32,
    cause: Option<SharedError>,
    due_now: bool,
    parked: bool,
    delay: Duration,
}

impl<S, T, A> Claim<S, T, A>
where
    S: Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    A: Adapter<S, T>,
{
    /// Ends the attempt with one fenced transaction and closes the claim.
    /// `out` is the reconciler's verdict, or the error the reconciler (or
    /// the attempt runtime — a timeout, a panic) produced, which resolves to
    /// `Retry`.
    ///
    /// [`Error::Fenced`] means nothing of this attempt landed: a successor
    /// holds or held the object. Any other error left no committed
    /// completion either (rolled back, or doubly ambiguous) — the caller may
    /// retry while the ownership proof lasts; past it, lease expiry hands
    /// the object to a successor.
    pub async fn complete(self, out: Result<Outcome<T>, BoxError>) -> Result<Completion<T>, Error> {
        self.lease.require_proof()?;
        let verdict = Verdict::from_result(out);
        let mut first: Option<Error> = None;
        for retry in 0..2 {
            match self.complete_once(&verdict, retry > 0).await {
                Ok(done) => {
                    self.lease.close();
                    return Ok(done);
                }
                Err(e) if e.is_commit_unknown() => first = Some(e),
                Err(e) => {
                    if matches!(e, Error::Fenced) {
                        self.lease.close();
                    }
                    return Err(e);
                }
            }
        }
        match first {
            Some(Error::CommitUnknown { op, source }) => Err(Error::CommitUnknown {
                op: format!("complete {}: ambiguous twice ({op})", self.r#ref()),
                source,
            }),
            Some(e) => Err(e),
            None => unreachable!("two tries always set the first error"),
        }
    }

    async fn complete_once(
        &self,
        verdict: &Verdict<T>,
        commit_retry: bool,
    ) -> Result<Completion<T>, Error> {
        let r = self.r#ref();
        let op = format!("complete {r}");
        let partition = self.store.inner.partition.clone();
        let mut tx = self
            .store
            .inner
            .pool
            .begin()
            .await
            .map_err(|e| Error::sql(format!("{op}: begin"), e))?;

        let row = sqlx::query(&format!(
            "SELECT {META_COLUMNS}, claim_token FROM {partition} WHERE id = $1 FOR UPDATE"
        ))
        .bind(self.object.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| Error::sql(format!("{op}: lock"), e))?;
        let Some(row) = row else {
            return self.complete_absent(tx, verdict, commit_retry).await;
        };
        let (_, meta) = scan_meta(&row).map_err(|e| Error::sql(format!("{op}: lock"), e))?;
        let token: Option<uuid::Uuid> = row
            .try_get("claim_token")
            .map_err(|e| Error::sql(format!("{op}: lock"), e))?;

        // Re-prove local ownership BEFORE interpreting what the lock
        // revealed: a lock-delayed completion must not write after
        // self-fencing, and the ambiguity-adoption signature below is only
        // sound while the proof holds (the proof never outlives the lease,
        // so no successor can have adopted).
        self.lease.require_proof()?;

        if token != Some(self.lease.token) {
            // A cleared token with our generation observed is the idempotent
            // signature of our own completion whose COMMIT acknowledgement
            // was lost: only we ever held this token, and completion is the
            // only path that clears a token while advancing
            // observed_generation.
            if commit_retry
                && token.is_none()
                && meta.observed_generation >= self.object.meta.generation
            {
                return Ok(Completion::Unknown);
            }
            return Err(Error::Fenced);
        }

        let superseded = meta.generation != self.object.meta.generation;
        let woken = meta.wake_seq != self.object.meta.wake_seq;
        let mut res = resolve(
            verdict,
            self.object.deleting(),
            meta.attempts,
            self.cfg.max_attempts,
        );

        // Confirmed teardown: durable evidence, then the row itself
        // (invariant 6). Deletion is exempt from superseded/woken handling —
        // deleted_at is one-way and the reconciler confirmed external
        // absence.
        if res.decision == Decision::Delete {
            return self.complete_delete(tx, res, superseded, woken).await;
        }

        if res.status.is_some() {
            res = self.write_status_savepoint(&mut tx, res, &meta).await?;
        }

        let fields = self.completion_fields(&res, meta.attempts);

        if superseded || woken {
            // Record the observation, release the claim, leave the object
            // due now. Phase and last_error belong to the newer intent or
            // the wake's fresh pass; attempts is settled normally.
            self.settle_envelope(
                &mut tx,
                Settle {
                    phase: None,
                    attempts: fields.attempts,
                    cause: None,
                    due_now: true,
                    parked: false,
                    delay: Duration::ZERO,
                },
            )
            .await?;
            publish_wake(&mut tx, self.store.inner.decl.name).await?;
            tx.commit()
                .await
                .map_err(|e| Error::commit(op.clone(), e))?;
            return Ok(Completion::Committed {
                outcome: res.into_outcome(),
                superseded,
                woken,
            });
        }

        let due_now = !fields.parked && fields.delay.is_zero();
        self.settle_envelope(
            &mut tx,
            Settle {
                phase: Some(fields.phase),
                attempts: fields.attempts,
                cause: res.cause.clone(),
                due_now,
                parked: fields.parked,
                delay: fields.delay,
            },
        )
        .await?;
        if due_now {
            publish_wake(&mut tx, self.store.inner.decl.name).await?;
        }
        tx.commit()
            .await
            .map_err(|e| Error::commit(op.clone(), e))?;
        if res.decision == Decision::Blocked {
            // Parking is the most consequential transition the framework
            // can commit, so it is never silent.
            tracing::error!(
                processing_object_type = self.store.inner.decl.name,
                id = %self.object.id,
                attempts = fields.attempts,
                cause = %res.cause.as_ref().map(|c| c.to_string()).unwrap_or_default(),
                "processing object blocked — parked until new intent or a nudge"
            );
        }
        Ok(Completion::Committed {
            outcome: res.into_outcome(),
            superseded: false,
            woken: false,
        })
    }

    /// The one statement every completion shape settles through: observed
    /// generation, retry state, schedule, claim release.
    async fn settle_envelope(&self, conn: &mut PgConnection, s: Settle) -> Result<(), Error> {
        let phase: Option<&str> = s.phase.map(Phase::as_str);
        let last_error = error_text(s.cause.as_ref());
        let parked_at: DateTime<Utc> = *SCHEDULE_PARKED;
        sqlx::query(&format!(
            "UPDATE {}
             SET observed_generation = GREATEST(observed_generation, $2),
                 attempts = $3,
                 phase = COALESCE($4, phase),
                 last_error = CASE WHEN $4 IS NULL THEN last_error ELSE $5 END,
                 next_reconcile_at = CASE
                     WHEN $6::boolean THEN clock_timestamp()
                     WHEN $7::boolean THEN $8::timestamptz
                     ELSE clock_timestamp() + make_interval(secs => $9)
                 END,
                 last_reconciled_at = clock_timestamp(),
                 claim_token = NULL, claimed_at = NULL, lease_expires_at = NULL
             WHERE id = $1",
            self.store.inner.partition
        ))
        .bind(self.object.id)
        .bind(self.object.meta.generation)
        .bind(s.attempts)
        .bind(phase)
        .bind(last_error)
        .bind(s.due_now)
        .bind(s.parked)
        .bind(parked_at)
        .bind(s.delay.as_secs_f64())
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .map_err(|e| Error::sql(format!("complete {}: settle envelope", self.r#ref()), e))
    }

    /// A completion that finds no envelope row. Only a completed deletion
    /// removes rows, and only the token holder completes — so either a
    /// successor adopted our expired lease and finished a teardown (fenced),
    /// or our own earlier `Delete` commit landed and its acknowledgement was
    /// lost.
    async fn complete_absent(
        &self,
        mut tx: sqlx::Transaction<'static, sqlx::Postgres>,
        verdict: &Verdict<T>,
        commit_retry: bool,
    ) -> Result<Completion<T>, Error> {
        if !commit_retry || verdict.decision != Decision::Delete {
            return Err(Error::Fenced);
        }
        // Same soundness gate as the in-row adoption: only a live proof rules
        // out a successor having adopted our expired lease and completed the
        // teardown itself.
        self.lease.require_proof()?;
        // Our Delete landed, and the only transaction that removes a row runs
        // the finalizer atomically with it. The idempotent finalizer is re-run
        // as redundancy, but nothing about that errand may obscure the proven
        // result: a failure here is ignored.
        {
            let conn: &mut PgConnection = &mut tx;
            let mut rtx = Tx::new(conn);
            if self
                .store
                .inner
                .decl
                .adapter
                .finalize_delete(&mut rtx, &self.object)
                .await
                .is_ok()
            {
                let _ = tx.commit().await;
            }
        }
        Ok(Completion::Committed {
            outcome: Outcome::Delete,
            superseded: false,
            woken: false,
        })
    }

    /// Finalizes a confirmed teardown under a savepoint: durable evidence
    /// via `finalize_delete` on the CURRENT row, then the envelope row
    /// (typed rows cascade). A failure rolls back to the savepoint and
    /// commits a loud `Retry` instead, so a wedged finalizer surfaces in
    /// `last_error` and backs off rather than aborting completion forever.
    async fn complete_delete(
        &self,
        mut tx: sqlx::Transaction<'static, sqlx::Postgres>,
        mut res: Resolved<T>,
        superseded: bool,
        woken: bool,
    ) -> Result<Completion<T>, Error> {
        let r = self.r#ref();
        let op = format!("complete delete {r}");
        let name = self.store.inner.decl.name;
        tx.execute("SAVEPOINT complete_delete")
            .await
            .map_err(|e| Error::sql(format!("{op}: savepoint"), e))?;
        let delete_err: Option<String> = async {
            let row = match self.store.read_row_tx(&mut tx, &r).await {
                Ok(row) => row,
                Err(e) => return Some(e.to_string()),
            };
            let current = Object {
                meta: self.object.meta.clone(),
                id: self.object.id,
                spec: row.spec,
                status: row.status,
            };
            {
                let conn: &mut PgConnection = &mut tx;
                let mut rtx = Tx::new(conn);
                if let Err(e) = self
                    .store
                    .inner
                    .decl
                    .adapter
                    .finalize_delete(&mut rtx, &current)
                    .await
                {
                    return Some(e.to_string());
                }
            }
            sqlx::query(&format!(
                "DELETE FROM {} WHERE id = $1",
                self.store.inner.partition
            ))
            .bind(self.object.id)
            .execute(&mut *tx)
            .await
            .err()
            .map(|e| e.to_string())
        }
        .await;

        let Some(delete_err) = delete_err else {
            tx.execute("RELEASE SAVEPOINT complete_delete")
                .await
                .map_err(|e| Error::sql(format!("{op}: release savepoint"), e))?;
            tx.commit()
                .await
                .map_err(|e| Error::commit(op.clone(), e))?;
            return Ok(Completion::Committed {
                outcome: Outcome::Delete,
                superseded,
                woken,
            });
        };
        tx.execute("ROLLBACK TO SAVEPOINT complete_delete")
            .await
            .map_err(|e| Error::sql(format!("{op}: {delete_err} (rollback savepoint)"), e))?;
        res.decision = Decision::Retry;
        res.status = None;
        res.cause = Some(wrap("delete finalization failed", message(delete_err)));
        let fields = self.completion_fields(&res, self.object.meta.attempts);
        // The deletion exemption from superseded/woken handling is earned
        // only by the success path, where the row is removed. This retry
        // keeps the row, so it honours the fence like any other completion.
        let due_now = superseded || woken;
        self.settle_envelope(
            &mut tx,
            Settle {
                phase: Some(fields.phase),
                attempts: fields.attempts,
                cause: res.cause.clone(),
                due_now,
                parked: false,
                delay: fields.delay,
            },
        )
        .await?;
        if due_now {
            publish_wake(&mut tx, name).await?;
        }
        tx.commit()
            .await
            .map_err(|e| Error::commit(op.clone(), e))?;
        Ok(Completion::Committed {
            outcome: res.into_outcome(),
            superseded,
            woken,
        })
    }

    /// Writes typed status under a savepoint. A constraint-rejected status
    /// converts the completion into a loud `Retry`; any other error aborts
    /// the completion.
    async fn write_status_savepoint(
        &self,
        tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
        res: Resolved<T>,
        meta: &Meta,
    ) -> Result<Resolved<T>, Error> {
        let r = self.r#ref();
        let op = format!("complete {r}");
        tx.execute("SAVEPOINT complete_status")
            .await
            .map_err(|e| Error::sql(format!("{op}: status savepoint"), e))?;
        let status = res.status.as_ref().expect("called with a status");
        let written = {
            let conn: &mut PgConnection = &mut *tx;
            let mut rtx = Tx::new(conn);
            self.store
                .inner
                .decl
                .adapter
                .write_status(&mut rtx, &r, status)
                .await
        };
        match written {
            Ok(()) => {
                tx.execute("RELEASE SAVEPOINT complete_status")
                    .await
                    .map_err(|e| Error::sql(format!("{op}: release status savepoint"), e))?;
                Ok(res)
            }
            Err(e) if sqlstate::is_integrity_violation(&e) => {
                tx.execute("ROLLBACK TO SAVEPOINT complete_status")
                    .await
                    .map_err(|re| {
                        Error::sql(format!("{op}: write status: {e} (rollback savepoint)"), re)
                    })?;
                let converted = Verdict {
                    decision: Decision::Retry,
                    status: None,
                    cause: Some(wrap("status rejected by constraint", Arc::new(e))),
                    schedule: Schedule::Resync,
                };
                Ok(resolve(
                    &converted,
                    self.object.deleting(),
                    meta.attempts,
                    self.cfg.max_attempts,
                ))
            }
            Err(e) => Err(Error::sql(format!("{op}: write status"), e)),
        }
    }

    /// Maps a resolved completion onto envelope scheduling: phase, the
    /// attempts counter, and either a parked schedule or a delay from
    /// completion time.
    pub(crate) fn completion_fields(&self, res: &Resolved<T>, current_attempts: i32) -> Fields {
        match res.decision {
            Decision::Converged => Fields {
                phase: Phase::Converged,
                attempts: 0,
                delay: match res.schedule {
                    Schedule::Now => Duration::ZERO,
                    Schedule::After(d) => d,
                    Schedule::Resync => self.cfg.resync,
                },
                parked: false,
            },
            Decision::Retry => {
                let attempts = current_attempts + 1;
                Fields {
                    phase: Phase::Retrying,
                    attempts,
                    delay: self.cfg.backoff.delay(
                        self.object.id,
                        self.object.meta.generation,
                        attempts,
                    ),
                    parked: false,
                }
            }
            Decision::Blocked => Fields {
                phase: Phase::Blocked,
                attempts: current_attempts + 1,
                delay: Duration::ZERO,
                parked: true,
            },
            Decision::Settled => Fields {
                phase: Phase::Converged,
                attempts: 0,
                delay: Duration::ZERO,
                parked: true,
            },
            Decision::Delete => Fields {
                phase: Phase::Converged,
                attempts: 0,
                delay: Duration::ZERO,
                parked: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(out: Result<Outcome<&'static str>, BoxError>) -> Verdict<&'static str> {
        Verdict::from_result(out)
    }

    fn cause_text<T>(r: &Resolved<T>) -> String {
        r.cause.as_ref().map(|c| c.to_string()).unwrap_or_default()
    }

    #[test]
    fn resolution_applies_the_contract_and_the_budget() {
        // An attempt error overrides everything and carries no status.
        let r = resolve(&verdict(Err("boom".into())), false, 0, 0);
        assert_eq!((r.decision, r.status), (Decision::Retry, None));
        assert_eq!(cause_text(&r), "boom");

        // Delete on a non-deleting claim is a loud retry.
        let r = resolve(&verdict(Ok(Outcome::delete())), false, 0, 0);
        assert_eq!(r.decision, Decision::Retry);
        assert!(cause_text(&r).contains("Delete on non-deleting object"));
        // Delete on a deleting claim passes through.
        assert_eq!(
            resolve(&verdict(Ok(Outcome::delete())), true, 0, 0).decision,
            Decision::Delete
        );

        // Blocked on a deleting claim is a loud retry that keeps the status.
        let r = resolve(
            &verdict(Ok(Outcome::blocked(Some("s"), "boom"))),
            true,
            0,
            0,
        );
        assert_eq!((r.decision, r.status), (Decision::Retry, Some("s")));
        assert_eq!(cause_text(&r), "teardown must not park: boom");

        // Converged and Settled on a deleting claim pass through.
        assert_eq!(
            resolve(&verdict(Ok(Outcome::converged(Some("s")))), true, 0, 0).decision,
            Decision::Converged
        );
        assert_eq!(
            resolve(&verdict(Ok(Outcome::settled(Some("s")))), true, 0, 0).decision,
            Decision::Settled
        );

        // Retry below the budget stays; exhausting it escalates.
        let r = resolve(&verdict(Ok(Outcome::retry(Some("s"), "boom"))), false, 1, 5);
        assert_eq!(r.decision, Decision::Retry);
        let r = resolve(&verdict(Ok(Outcome::retry(Some("s"), "boom"))), false, 4, 5);
        assert_eq!((r.decision, r.status), (Decision::Blocked, Some("s")));
        assert_eq!(cause_text(&r), "maximum attempts 5 exhausted: boom");
        // Zero means unbounded; deleting claims never escalate.
        assert_eq!(
            resolve(
                &verdict(Ok(Outcome::retry(Some("s"), "boom"))),
                false,
                1000,
                0
            )
            .decision,
            Decision::Retry
        );
        assert_eq!(
            resolve(
                &verdict(Ok(Outcome::retry(Some("s"), "boom"))),
                true,
                1000,
                3
            )
            .decision,
            Decision::Retry
        );
    }

    #[test]
    fn error_text_is_bounded_and_valid() {
        assert_eq!(error_text(None), None);
        assert_eq!(
            error_text(Some(&message("short"))).as_deref(),
            Some("short")
        );
        let long = message("é".repeat(5000));
        let text = error_text(Some(&long)).unwrap();
        assert!(
            text.len() <= 4096,
            "must satisfy the schema's octet_length CHECK"
        );
        assert!(text.ends_with(" …(truncated)"));
        assert!(std::str::from_utf8(text.as_bytes()).is_ok());
    }

    #[test]
    fn resolved_projects_back_to_an_outcome() {
        let r = resolve(&verdict(Ok(Outcome::retry(Some("s"), "boom"))), false, 0, 0);
        let out = r.into_outcome();
        assert!(out.is_retry());
        assert_eq!(out.status(), Some(&"s"));
        assert_eq!(out.cause().unwrap().to_string(), "boom");
        assert!(
            resolve(&verdict(Ok(Outcome::delete())), true, 0, 0)
                .into_outcome()
                .is_delete()
        );
    }
}
