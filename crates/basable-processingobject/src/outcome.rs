//! The reconciler's verdict for one claimed attempt. An outcome is data, not
//! action: the reconciler returns it and completion (invariant 5) translates
//! it into exactly one fenced transaction. Nothing here touches the database.
//!
//! The Go original is a struct whose zero value is invalid and whose
//! scheduling modifiers degrade the outcome when chained onto the wrong
//! decision. Here the decision is an enum with no zero value, and `after`
//! and `requeue_now` exist only on the `Converged` variant's [`Schedule`],
//! so both contract violations are unrepresentable.

use std::time::Duration;

use basable_core::BoxError;

/// When a converged object runs next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// At the type's resync interval.
    Resync,
    /// A known wait from completion time; must be positive.
    After(Duration),
    /// Immediately: the durable-checkpoint-then-continue pattern.
    Now,
}

/// The verdict. `status` is optional on every variant that carries one:
/// `None` means the pass observed nothing worth writing and completion
/// leaves the typed status row untouched. A `Some` status is written in the
/// completion transaction under exact claim authority (invariant 3).
#[must_use = "an outcome is returned to completion, never dropped"]
#[derive(Debug)]
pub enum Outcome<T> {
    /// Success for the generation this attempt observed.
    Converged {
        /// The status to write, if any.
        status: Option<T>,
        /// When to run again.
        schedule: Schedule,
    },
    /// A transient failure: scheduled by the type's backoff, attempts
    /// advance, `max_attempts` may escalate to `Blocked` at completion.
    Retry {
        /// The status to write, if any.
        status: Option<T>,
        /// Why; recorded in `last_error`.
        cause: BoxError,
    },
    /// The current generation is unsatisfiable without intervention: parked
    /// until a new generation or a nudge. Completion refuses it for a
    /// deleting object (a failed teardown must not park).
    Blocked {
        /// The status to write, if any.
        status: Option<T>,
        /// Why; recorded in `last_error`.
        cause: BoxError,
    },
    /// External absence is confirmed for a deleting object: completion
    /// removes the envelope, typed rows cascade. Carries no status.
    Delete,
    /// Successful permanent convergence: parked until a spec write or a
    /// nudge. With `deleted_at` set it is a retained soft-delete tombstone.
    Settled {
        /// The status to write, if any.
        status: Option<T>,
    },
}

impl<T> Outcome<T> {
    /// Success, scheduled at the type's resync interval.
    pub fn converged(status: Option<T>) -> Outcome<T> {
        Outcome::Converged {
            status,
            schedule: Schedule::Resync,
        }
    }

    /// Success, with the next pass `d` from completion time. A zero `d` is
    /// [`Schedule::Now`].
    pub fn converged_after(status: Option<T>, d: Duration) -> Outcome<T> {
        let schedule = if d.is_zero() {
            Schedule::Now
        } else {
            Schedule::After(d)
        };
        Outcome::Converged { status, schedule }
    }

    /// Success, and run again immediately.
    pub fn requeue_now(status: Option<T>) -> Outcome<T> {
        Outcome::Converged {
            status,
            schedule: Schedule::Now,
        }
    }

    /// A transient failure. A `cause` is required: `last_error` is never
    /// empty after a retry.
    pub fn retry(status: Option<T>, cause: impl Into<BoxError>) -> Outcome<T> {
        Outcome::Retry {
            status,
            cause: cause.into(),
        }
    }

    /// Parked until intervention.
    pub fn blocked(status: Option<T>, cause: impl Into<BoxError>) -> Outcome<T> {
        Outcome::Blocked {
            status,
            cause: cause.into(),
        }
    }

    /// Confirmed physical deletion.
    pub fn delete() -> Outcome<T> {
        Outcome::Delete
    }

    /// Successful permanent convergence.
    pub fn settled(status: Option<T>) -> Outcome<T> {
        Outcome::Settled { status }
    }

    /// The status this outcome carries, `None` when the attempt observed
    /// nothing.
    pub fn status(&self) -> Option<&T> {
        match self {
            Outcome::Converged { status, .. }
            | Outcome::Retry { status, .. }
            | Outcome::Blocked { status, .. }
            | Outcome::Settled { status } => status.as_ref(),
            Outcome::Delete => None,
        }
    }

    /// The recorded failure, `None` for a clean `Converged`, `Settled` or
    /// `Delete`.
    pub fn cause(&self) -> Option<&BoxError> {
        match self {
            Outcome::Retry { cause, .. } | Outcome::Blocked { cause, .. } => Some(cause),
            _ => None,
        }
    }

    /// Whether this is `Converged`.
    pub fn is_converged(&self) -> bool {
        matches!(self, Outcome::Converged { .. })
    }

    /// Whether this is `Retry`.
    pub fn is_retry(&self) -> bool {
        matches!(self, Outcome::Retry { .. })
    }

    /// Whether this is `Blocked`.
    pub fn is_blocked(&self) -> bool {
        matches!(self, Outcome::Blocked { .. })
    }

    /// Whether this is `Delete`.
    pub fn is_delete(&self) -> bool {
        matches!(self, Outcome::Delete)
    }

    /// Whether this is `Settled`.
    pub fn is_settled(&self) -> bool {
        matches!(self, Outcome::Settled { .. })
    }

    /// The decision's name, for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Outcome::Converged { .. } => "converged",
            Outcome::Retry { .. } => "retry",
            Outcome::Blocked { .. } => "blocked",
            Outcome::Delete => "delete",
            Outcome::Settled { .. } => "settled",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_carry_what_they_say() {
        let c = Outcome::converged(Some("observed"));
        assert!(c.is_converged());
        assert_eq!(c.status(), Some(&"observed"));
        assert!(c.cause().is_none());
        assert!(matches!(
            c,
            Outcome::Converged {
                schedule: Schedule::Resync,
                ..
            }
        ));

        let r = Outcome::<&str>::retry(None, "boom");
        assert!(r.is_retry());
        assert_eq!(r.cause().unwrap().to_string(), "boom");

        let b = Outcome::<&str>::blocked(None, "stuck");
        assert!(b.is_blocked());
        assert!(b.status().is_none());

        let d = Outcome::<&str>::delete();
        assert!(d.is_delete());
        assert!(d.status().is_none());
        assert_eq!(d.name(), "delete");

        let s = Outcome::settled(Some("s"));
        assert!(s.is_settled());
        assert_eq!(s.status(), Some(&"s"));
    }

    #[test]
    fn schedules_exist_only_on_converged() {
        assert!(matches!(
            Outcome::converged_after(Some(1), Duration::from_secs(5)),
            Outcome::Converged { schedule: Schedule::After(d), .. } if d == Duration::from_secs(5)
        ));
        assert!(matches!(
            Outcome::converged_after(Some(1), Duration::ZERO),
            Outcome::Converged {
                schedule: Schedule::Now,
                ..
            }
        ));
        assert!(matches!(
            Outcome::requeue_now(Some(1)),
            Outcome::Converged {
                schedule: Schedule::Now,
                ..
            }
        ));
        // The other variants have no schedule field: a modifier on them is
        // not a runtime error but a type error, which is the point.
    }
}
