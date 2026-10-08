//! What a component runs: its processing-object workers and tickers,
//! collected as [`Loops`] and handed to the app through the generated
//! messenger. The shape of the monorepo's `server.CollectWorkers`: `main`
//! hands over the component list, each component builds and joins its own
//! loops, and `main` never names a component's types.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use basable_core::Ctx;
use basable_processingobject::{Adapter, AfterComplete, Reconciler, Worker};

use crate::ticker::Ticker;

pub(crate) type Run = Box<dyn FnOnce(Ctx) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// One loop: its name and the future it runs under the root context.
pub(crate) struct Loop {
    pub(crate) name: String,
    pub(crate) run: Run,
}

/// A component's loops, named and type-erased: workers over any processing
/// object type and tickers, side by side. Built in [`Component::loops`].
#[derive(Default)]
pub struct Loops {
    loops: Vec<Loop>,
}

impl Loops {
    /// No loops.
    pub fn new() -> Loops {
        Loops::default()
    }

    /// Adds a processing-object worker, named by its type. It runs on its
    /// own task under the root context, woken by writes through the store
    /// it was built with, and is drained on shutdown.
    pub fn worker<S, T, A, R, F>(mut self, worker: Worker<S, T, A, R, F>) -> Loops
    where
        S: Clone + Send + Sync + 'static,
        T: Clone + Send + Sync + 'static,
        A: Adapter<S, T>,
        R: Reconciler<S, T, A>,
        F: AfterComplete<S, T>,
    {
        self.loops.push(Loop {
            name: worker.type_name().to_string(),
            run: Box::new(move |ctx| Box::pin(worker.run(ctx))),
        });
        self
    }

    /// Adds a ticker, named by its name, on its own task.
    pub fn ticker(mut self, ticker: Ticker) -> Loops {
        self.loops.push(Loop {
            name: ticker.name().to_string(),
            run: Box::new(move |ctx| Box::pin(ticker.run(ctx))),
        });
        self
    }

    pub(crate) fn into_inner(self) -> Vec<Loop> {
        self.loops
    }
}

impl fmt::Debug for Loops {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.loops.iter().map(|l| &l.name))
            .finish()
    }
}

/// A component that may own loops: the analogue of a Go component with a
/// `Run` method. Rust cannot ask at run time whether a component has one,
/// so every component implements this trait; a plain executor (a
/// sends-only boundary, a handler with nothing to schedule) takes the
/// default and runs nothing.
///
/// `R` is the router the component sends through. The app asks once, at
/// wiring, with the leaked component and router, so a worker's reconciler
/// and a ticker's closure can hold both for the life of the process:
///
/// ```text
/// impl<R: CatalogRoutes> basable_app::Component<R> for Catalog {
///     fn loops(&'static self, router: &'static R) -> Loops {
///         Loops::new()
///             .worker(types::product::worker(router, self))
///             .ticker(worker::sweep(router, self))
///     }
/// }
/// ```
pub trait Component<R>: Send + Sync + 'static {
    /// The component's workers and tickers. The default is none.
    fn loops(&'static self, router: &'static R) -> Loops {
        let _ = router;
        Loops::new()
    }
}

/// Every component of the app with its loops, one entry per component,
/// named as in `routing.yaml`. The generated messenger implements it over
/// the components it holds; [`Serve::components`](crate::Serve::components)
/// registers what it returns.
pub trait Components: Send + Sync + 'static {
    /// Each component's name and loops, in `routing.yaml` order.
    fn loops(&'static self) -> Vec<(&'static str, Loops)>;
}
