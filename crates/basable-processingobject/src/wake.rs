//! The in-process wake: how a committed write tells the workers running on
//! its store that an object became due, ahead of their next poll.
//!
//! Every replica runs every worker, and every write to a type happens in a
//! process that runs the type's worker, through the one [`TypedStore`] the
//! nanoservice bound for it. So the store's shared inner holds the wake of
//! each worker currently running on it, and a write that makes an object
//! due signals them all once it has committed. Nothing crosses a process: a
//! write on another replica, or through a store bound separately for the
//! same type (which models one), reaches these workers through their poll.
//! The poll is the correctness path; a wake only shortens latency.
//!
//! A wake is a stored permit, so wakes coalesce like Go's one-slot channel:
//! many signals while a scan runs mean one more scan.
//!
//! [`TypedStore`]: crate::TypedStore

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::Notify;

/// The wakes of the workers running on one store, shared by every clone of
/// it.
#[derive(Default)]
pub(crate) struct Wakes {
    workers: Mutex<Vec<Arc<Notify>>>,
}

impl Wakes {
    /// Registers a new worker wake for as long as the returned guard lives.
    pub(crate) fn register(self: &Arc<Self>) -> Registration {
        let notify = Arc::new(Notify::new());
        self.workers().push(Arc::clone(&notify));
        Registration {
            wakes: Arc::clone(self),
            notify,
        }
    }

    /// Wakes every registered worker.
    pub(crate) fn signal(&self) {
        for notify in self.workers().iter() {
            notify.notify_one();
        }
    }

    /// Carries out what a committed completion calls for. The timer is a
    /// detached task, so it holds neither the attempt nor its parallelism
    /// slot; one that fires after the worker stopped signals nobody.
    pub(crate) fn wake(self: &Arc<Self>, wake: Wake) {
        match wake {
            Wake::None => {}
            Wake::Now => self.signal(),
            Wake::After(delay) => {
                let wakes = Arc::clone(self);
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    wakes.signal();
                });
            }
        }
    }

    fn workers(&self) -> MutexGuard<'_, Vec<Arc<Notify>>> {
        self.workers.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A worker's place on its store: registered by [`Wakes::register`],
/// removed on drop.
pub(crate) struct Registration {
    wakes: Arc<Wakes>,
    notify: Arc<Notify>,
}

impl Registration {
    /// The worker's own wake. The worker also signals it alone when one of
    /// its attempts frees a slot: that news is local to this worker.
    pub(crate) fn notify(&self) -> &Arc<Notify> {
        &self.notify
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.wakes
            .workers()
            .retain(|n| !Arc::ptr_eq(n, &self.notify));
    }
}

/// What a committed completion asks of its store's workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// Nothing: no pass is scheduled, or the poll covers it.
    None,
    /// Signal now.
    Now,
    /// Signal once, this long from now.
    After(Duration),
}

impl Wake {
    /// The wake for a completion's `next_pass` (see
    /// [`Completion::Committed`]), under a worker polling every
    /// `poll_interval`: at once when it is due at once; a one-shot timer
    /// when it is due within one interval, which the poll alone would serve
    /// up to a whole interval late (`after(5s)` under a 30 s poll ran as
    /// late as 35 s); nothing for a longer delay, which the poll serves
    /// within one interval of it as before, and nothing for an object on no
    /// schedule — parked, deleted, or an unknown outcome.
    ///
    /// [`Completion::Committed`]: crate::Completion::Committed
    pub(crate) fn for_next_pass(next_pass: Option<Duration>, poll_interval: Duration) -> Wake {
        match next_pass {
            None => Wake::None,
            Some(d) if d.is_zero() => Wake::Now,
            Some(d) if d <= poll_interval => Wake::After(d),
            Some(_) => Wake::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLL: Duration = Duration::from_secs(30);

    #[test]
    fn only_a_delay_within_one_poll_interval_arms_a_timer() {
        let secs = Duration::from_secs;
        let wake = |next_pass| Wake::for_next_pass(next_pass, POLL);
        assert_eq!(wake(Some(Duration::ZERO)), Wake::Now);
        assert_eq!(
            wake(Some(Duration::from_millis(200))),
            Wake::After(Duration::from_millis(200))
        );
        assert_eq!(wake(Some(secs(5))), Wake::After(secs(5)));
        assert_eq!(wake(Some(POLL)), Wake::After(POLL));
        assert_eq!(
            wake(Some(POLL + Duration::from_millis(1))),
            Wake::None,
            "a delay longer than the poll interval is the poll's"
        );
        assert_eq!(wake(Some(secs(3600))), Wake::None);
        assert_eq!(wake(None), Wake::None);
    }

    #[tokio::test]
    async fn a_signal_reaches_every_registered_worker_and_coalesces() {
        let wakes = Arc::new(Wakes::default());
        let a = wakes.register();
        let b = wakes.register();
        wakes.signal();
        wakes.signal();
        for r in [&a, &b] {
            tokio::time::timeout(Duration::from_secs(1), r.notify().notified())
                .await
                .expect("a stored permit");
            // Two signals left one permit, not two.
            assert!(
                tokio::time::timeout(Duration::from_millis(20), r.notify().notified())
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn a_dropped_registration_is_signalled_no_more() {
        let wakes = Arc::new(Wakes::default());
        let kept = wakes.register();
        let dropped = wakes.register();
        let gone = Arc::clone(dropped.notify());
        drop(dropped);
        assert_eq!(wakes.workers().len(), 1);
        wakes.signal();
        kept.notify().notified().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(20), gone.notified())
                .await
                .is_err(),
            "a stopped worker is no longer woken"
        );
    }

    #[tokio::test]
    async fn the_timer_signals_after_its_delay_and_is_harmless_once_nobody_listens() {
        let wakes = Arc::new(Wakes::default());
        let r = wakes.register();
        let started = std::time::Instant::now();
        wakes.wake(Wake::After(Duration::from_millis(50)));
        r.notify().notified().await;
        assert!(started.elapsed() >= Duration::from_millis(50));

        drop(r);
        wakes.wake(Wake::After(Duration::from_millis(1)));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(wakes.workers().is_empty());
    }
}
