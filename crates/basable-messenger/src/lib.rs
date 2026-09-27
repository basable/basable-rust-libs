//! The runtime half of the basable messenger. It is two items; everything
//! else about a messenger is generated at build from `routing.yaml` by
//! `basable-messenger-gen` (the `interfaces` and `messenger` crates of a
//! tenant project).
//!
//! - [`Route`]: the dispatch trait. The generated router implements it once
//!   per declared `(source, message)` pair; the generated sender of each
//!   nanoservice calls it through the router's `*Routes` bound. Static
//!   dispatch: no vtable, no boxing on the common path.
//! - [`boxed`]: the one place a future is boxed. With static dispatch every
//!   route's future is part of the caller's future type, so a cycle in the
//!   route graph is an infinitely sized future; the generator boxes a
//!   feedback set of routes and every other call stays zero-cost.
//!
//! The design (the basable plan, B2/B2b): handlers take `&self`, the
//! concrete router is passed by shared reference into every handler and
//! reconciler, no `Arc<dyn>`; a handler's future is `Send`, so a
//! `std::sync::MutexGuard` held across a send is a compile error.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::future::Future;
use std::pin::Pin;

/// One routed message: how the router dispatches `M` sent by `Source` and
/// answers with `Resp`.
///
/// `Source` is the generated marker type of the sending nanoservice
/// (`interfaces::source::Order`), so the same message sent by two
/// nanoservices is two impls, and a nanoservice sending an undeclared
/// message has no impl to call. `Ctx` is the request context type the
/// project names in `routing.yaml` (`messenger.rust.ctx_type`).
///
/// `Resp` is the response shape the declaration fixes: `Result<T, E>` for a
/// typed 1:1 request, `Result<(), E>` for a fail-fast error fan-out, `()`
/// for a void fan-out.
///
/// The one lifetime `'a` ties the router and the context borrows together
/// so that a boxed route's hidden type, `Pin<Box<dyn Future + Send + 'a>>`,
/// is nameable (two independent borrows would leave the box's lifetime an
/// intersection the opaque type cannot express, E0700). A caller with two
/// different lifetimes coerces both to the shorter.
pub trait Route<M, Resp, Source, Ctx> {
    /// Dispatches `msg` to its handler(s) and returns the response. The
    /// future is `Send`: it captures `&self` and `&ctx`, so the router and
    /// the context are `Sync`.
    fn route<'a>(&'a self, ctx: &'a Ctx, msg: M) -> impl Future<Output = Resp> + Send + 'a;
}

/// A pinned, boxed, `Send` future: what a boxed route returns.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxes a route's future. The generator emits this on the routes that close
/// a cycle in `routing.yaml` (reported as `W_ROUTE_CYCLE` at build), which
/// erases the inner future's type and breaks the recursion; nothing else is
/// boxed.
pub fn boxed<'a, F>(future: F) -> BoxFuture<'a, F::Output>
where
    F: Future + Send + 'a,
{
    Box::pin(future)
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll, Waker};

    use super::*;

    struct Ctx;
    struct Src;
    struct Router;

    impl Route<u32, u64, Src, Ctx> for Router {
        fn route<'a>(&'a self, _ctx: &'a Ctx, msg: u32) -> impl Future<Output = u64> + Send + 'a {
            boxed(async move { u64::from(msg) * 2 })
        }
    }

    fn assert_send<T: Send>(_: &T) {}

    fn block_on<F: Future>(fut: F) -> F::Output {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut fut = std::pin::pin!(fut);
        loop {
            if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
                return out;
            }
        }
    }

    #[test]
    fn a_boxed_route_is_a_send_future_with_the_declared_output() {
        let router = Router;
        let ctx = Ctx;
        let fut = <Router as Route<u32, u64, Src, Ctx>>::route(&router, &ctx, 21);
        assert_send(&fut);
        assert_eq!(block_on(fut), 42);
    }
}
