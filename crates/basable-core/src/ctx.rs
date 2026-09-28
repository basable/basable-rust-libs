//! `Ctx`: the request context every handler and reconciler receives — the
//! port of what Go passes as `context.Context`: a request id for the logs, a
//! [`Deadline`], cancellation, and typed values (the authenticated identity
//! `basable-auth` attaches, the tenant a gateway resolved).
//!
//! It is a plain value, cheap to clone: the shared parts live behind `Arc`s.
//! A child context narrows its parent — a shorter deadline, one more value —
//! and is cancelled whenever the parent is. Nothing in here spawns or
//! schedules; timers belong to the runtime that owns the request.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use crate::Deadline;

/// The request context.
#[derive(Clone)]
pub struct Ctx {
    request_id: Arc<str>,
    deadline: Option<Deadline>,
    cancel: Arc<CancelFlag>,
    values: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl Ctx {
    /// A root context with a request id and no deadline. A server creates one
    /// per request (from the incoming request id header or a fresh one); a
    /// worker creates one per reconcile attempt.
    pub fn new(request_id: impl Into<String>) -> Ctx {
        let id: String = request_id.into();
        Ctx {
            request_id: Arc::from(id.as_str()),
            deadline: None,
            cancel: Arc::new(CancelFlag::default()),
            values: Arc::new(HashMap::new()),
        }
    }

    /// A root context for code that has no request: tests, boot, tickers.
    pub fn background() -> Ctx {
        Ctx::new("")
    }

    /// The request id, empty for a background context.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// The deadline, if any.
    pub fn deadline(&self) -> Option<Deadline> {
        self.deadline
    }

    /// A child with a deadline no later than `d` from now: an existing
    /// earlier deadline wins.
    pub fn with_timeout(&self, d: Duration) -> Ctx {
        self.with_deadline(Deadline::after(d))
    }

    /// A child with a deadline no later than `deadline`: an existing earlier
    /// deadline wins.
    pub fn with_deadline(&self, deadline: Deadline) -> Ctx {
        let mut child = self.child();
        child.deadline = Some(match self.deadline {
            Some(cur) if cur.mono() <= deadline.mono() => cur,
            _ => deadline,
        });
        child
    }

    /// A child carrying one more typed value; a value of the same type in
    /// the parent is shadowed for the child.
    pub fn with_value<T: Any + Send + Sync>(&self, value: T) -> Ctx {
        let mut child = self.child();
        let mut values: HashMap<_, _> = (*self.values).clone();
        values.insert(TypeId::of::<T>(), Arc::new(value));
        child.values = Arc::new(values);
        child
    }

    /// The value of type `T` attached to this context or an ancestor.
    pub fn value<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.values
            .get(&TypeId::of::<T>())
            .and_then(|v| v.downcast_ref::<T>())
    }

    /// A child that can be cancelled on its own; cancelling the parent
    /// cancels it too.
    pub fn child(&self) -> Ctx {
        let cancel = Arc::new(CancelFlag::child_of(&self.cancel));
        self.cancel.adopt(&cancel);
        Ctx {
            request_id: Arc::clone(&self.request_id),
            deadline: self.deadline,
            cancel,
            values: Arc::clone(&self.values),
        }
    }

    /// Cancels this context and every child derived from it.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Whether this context was cancelled or its deadline has passed.
    pub fn is_done(&self) -> bool {
        self.cancel.is_cancelled() || self.deadline.is_some_and(|d| d.has_passed())
    }

    /// Whether this context was cancelled (deadlines aside).
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Resolves when the context is cancelled. A deadline is not a timer:
    /// callers that must wake at the deadline race this against their
    /// runtime's sleep for [`Deadline::remaining`].
    pub fn cancelled(&self) -> Cancelled {
        Cancelled {
            flag: Arc::clone(&self.cancel),
        }
    }
}

impl fmt::Debug for Ctx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ctx")
            .field("request_id", &self.request_id)
            .field("deadline", &self.deadline)
            .field("cancelled", &self.is_cancelled())
            .field("values", &self.values.len())
            .finish()
    }
}

/// The cancellation flag a context tree shares downward: a parent's flag
/// knows its children so one `cancel` reaches all of them.
#[derive(Default)]
struct CancelFlag {
    cancelled: AtomicBool,
    inner: Mutex<CancelInner>,
}

#[derive(Default)]
struct CancelInner {
    wakers: Vec<Waker>,
    children: Vec<std::sync::Weak<CancelFlag>>,
}

impl CancelFlag {
    fn child_of(parent: &Arc<CancelFlag>) -> CancelFlag {
        let child = CancelFlag::default();
        if parent.is_cancelled() {
            child.cancelled.store(true, Ordering::SeqCst);
        }
        child
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let (wakers, children) = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            (
                std::mem::take(&mut inner.wakers),
                std::mem::take(&mut inner.children),
            )
        };
        for w in wakers {
            w.wake();
        }
        for c in children.iter().filter_map(std::sync::Weak::upgrade) {
            c.cancel();
        }
    }

    fn register(&self, waker: &Waker) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !inner.wakers.iter().any(|w| w.will_wake(waker)) {
            inner.wakers.push(waker.clone());
        }
    }

    fn adopt(&self, child: &Arc<CancelFlag>) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.children.retain(|c| c.strong_count() > 0);
        inner.children.push(Arc::downgrade(child));
    }
}

/// The future returned by [`Ctx::cancelled`].
pub struct Cancelled {
    flag: Arc<CancelFlag>,
}

impl Future for Cancelled {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.flag.is_cancelled() {
            return Poll::Ready(());
        }
        self.flag.register(cx.waker());
        // Re-check after registering: a cancel between the first check and
        // the registration would otherwise be missed.
        if self.flag.is_cancelled() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_typed_and_inherited() {
        #[derive(Debug, PartialEq)]
        struct UserId(u32);
        #[derive(Debug, PartialEq)]
        struct TenantId(&'static str);

        let root = Ctx::new("req-1").with_value(UserId(7));
        let child = root.with_value(TenantId("t1"));
        assert_eq!(child.value::<UserId>(), Some(&UserId(7)));
        assert_eq!(child.value::<TenantId>(), Some(&TenantId("t1")));
        assert_eq!(root.value::<TenantId>(), None);
        assert_eq!(child.request_id(), "req-1");
    }

    #[test]
    fn a_child_deadline_never_extends_the_parent() {
        let parent = Ctx::background().with_timeout(Duration::from_secs(1));
        let child = parent.with_timeout(Duration::from_secs(60));
        assert_eq!(child.deadline(), parent.deadline());
        let shorter = parent.with_timeout(Duration::from_millis(1));
        assert!(shorter.deadline().unwrap().mono() < parent.deadline().unwrap().mono());
    }

    #[test]
    fn cancel_flows_down_and_wakes_waiters() {
        let root = Ctx::new("r");
        let child = root.child();
        let mut fut = child.cancelled();
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert_eq!(Pin::new(&mut fut).poll(&mut cx), Poll::Pending);
        root.cancel();
        assert!(child.is_cancelled());
        assert!(child.is_done());
        assert_eq!(Pin::new(&mut fut).poll(&mut cx), Poll::Ready(()));
        // A child made after the cancel is born cancelled.
        assert!(root.child().is_cancelled());
    }

    #[test]
    fn a_passed_deadline_is_done_without_a_cancel() {
        let ctx = Ctx::background().with_timeout(Duration::ZERO);
        std::thread::sleep(Duration::from_millis(2));
        assert!(ctx.is_done());
        assert!(!ctx.is_cancelled());
    }
}
