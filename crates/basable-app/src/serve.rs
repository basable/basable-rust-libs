//! Wiring and running: one axum server carrying the application's routes
//! and the probes, the pubsub bus, every processing-object worker and
//! ticker on its own task, and a shutdown that cancels the root context,
//! drains everything within the grace, and names what did not drain.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use basable_auth::Validator;
use basable_connect::ConnectRouter;
use basable_core::Ctx;
use basable_processingobject::{Adapter, AfterComplete, Reconciler, Worker};
use basable_pubsub::Bus;
use sqlx::PgPool;
use tokio::task::JoinHandle;

use crate::boot::App;
use crate::error::Error;
use crate::ticker::Ticker;

/// Bounds the readiness probe's database check.
const READY_DB_TIMEOUT: Duration = Duration::from_secs(2);

type Run = Box<dyn FnOnce(Ctx) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

struct Registered {
    name: String,
    run: Run,
}

/// The wiring step.
pub struct Serve {
    app: App,
    router: Router,
    auth: Option<Validator>,
    loops: Vec<Registered>,
}

impl Serve {
    pub(crate) fn new(app: App) -> Serve {
        Serve {
            app,
            router: Router::new(),
            auth: None,
            loops: Vec::new(),
        }
    }

    /// Mounts the Connect services: every registered procedure path,
    /// beside the raw routes and the probes.
    pub fn connect(self, router: ConnectRouter) -> Serve {
        self.raw(router.into_axum())
    }

    /// Requires a validated session on every route but the validator's
    /// public paths; the probes are always public. A validated request's
    /// identity reaches the handler through `basable_connect::request_ctx`.
    pub fn auth(mut self, validator: Validator) -> Serve {
        self.auth = Some(validator.public_paths(["/healthz", "/readyz"]));
        self
    }

    /// The app it wires.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Mounts routes beside the probes: the Connect router as an axum
    /// service, the raw webhook routes, an SSE handler.
    pub fn raw(mut self, routes: Router) -> Serve {
        self.router = self.router.merge(routes);
        self
    }

    /// Registers a processing-object worker. It runs on its own task under
    /// the root context, woken by writes through the store it was built
    /// with, and is drained on shutdown.
    pub fn worker<S, T, A, R, F>(mut self, name: &str, worker: Worker<S, T, A, R, F>) -> Serve
    where
        S: Clone + Send + Sync + 'static,
        T: Clone + Send + Sync + 'static,
        A: Adapter<S, T>,
        R: Reconciler<S, T, A>,
        F: AfterComplete<S, T>,
    {
        self.loops.push(Registered {
            name: format!("{name}/{}", worker.type_name()),
            run: Box::new(move |ctx| Box::pin(worker.run(ctx))),
        });
        self
    }

    /// Registers a nanoservice's tickers, each on its own task.
    pub fn ticker(mut self, name: &str, tickers: impl IntoIterator<Item = Ticker>) -> Serve {
        for t in tickers {
            self.loops.push(Registered {
                name: format!("{name}/{}", t.name()),
                run: Box::new(move |ctx| Box::pin(t.run(ctx))),
            });
        }
        self
    }

    /// Binds the listen address, starts the server, the pubsub bus and
    /// every loop, and returns once the readiness probe would answer 200.
    pub async fn start(self) -> Result<Running, Error> {
        let Serve {
            app,
            router,
            auth,
            loops,
        } = self;
        let (cfg, pool, bus) = app.into_parts();
        let addr = cfg.listen_addr();
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| Error::Bind { addr, source })?;
        let addr = listener.local_addr().map_err(Error::Server)?;

        let ctx = Ctx::background();
        let health = Arc::new(Health {
            ready: AtomicBool::new(false),
            pool: pool.clone(),
            loops: loops
                .iter()
                .map(|l| (l.name.clone(), Arc::new(AtomicBool::new(true))))
                .collect(),
        });
        let probes = Router::new()
            .route("/healthz", get(healthz))
            .route("/readyz", get(readyz))
            .with_state(health.clone());
        // Layers wrap what is already there: the auth layer sees every
        // route (probes included, public by construction), and the
        // request-id layer is outermost so a 401 carries an id too.
        let mut router = router.merge(probes);
        if let Some(validator) = auth {
            router = Arc::new(validator).apply(router);
        }
        let router = basable_connect::with_request_ids(router);
        let server_ctx = ctx.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { server_ctx.cancelled().await })
                .await
        });

        let bus_task = {
            let bus = bus.clone();
            let ctx = ctx.clone();
            tokio::spawn(async move { bus.run(ctx).await })
        };
        let mut tasks = Vec::with_capacity(loops.len());
        for (l, (_, alive)) in loops.into_iter().zip(health.loops.iter()) {
            let alive = alive.clone();
            let name = l.name.clone();
            let fut = (l.run)(ctx.clone());
            tasks.push((
                l.name,
                tokio::spawn(async move {
                    fut.await;
                    alive.store(false, Ordering::SeqCst);
                    tracing::info!(worker = name, "worker loop ended");
                }),
            ));
        }
        health.ready.store(true, Ordering::SeqCst);
        tracing::info!(%addr, workers = tasks.len(), "serving");
        Ok(Running {
            addr,
            ctx,
            health,
            server,
            bus,
            bus_task,
            tasks,
            pool,
            grace: cfg.shutdown_grace(),
        })
    }

    /// [`Serve::start`], then wait for SIGTERM or SIGINT, then
    /// [`Running::shutdown`].
    pub async fn run(self) -> Result<(), Error> {
        let running = self.start().await?;
        shutdown_signal().await;
        tracing::info!("shutdown signal received");
        running.shutdown().await
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// A started app: the address, the root context, and the handles shutdown
/// joins.
pub struct Running {
    addr: SocketAddr,
    ctx: Ctx,
    health: Arc<Health>,
    server: JoinHandle<Result<(), std::io::Error>>,
    bus: Arc<Bus>,
    bus_task: JoinHandle<()>,
    tasks: Vec<(String, JoinHandle<()>)>,
    pool: PgPool,
    grace: Duration,
}

impl Running {
    /// The bound address (port 0 in the configuration gives an ephemeral
    /// one).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The root context every loop and request runs under. Cancelling it
    /// begins the drain [`Running::shutdown`] completes.
    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    /// The pubsub bus.
    pub fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }

    /// Whether the readiness probe answers 200 right now (wiring done,
    /// database answering, every loop alive).
    pub async fn is_ready(&self) -> bool {
        self.health.check().await.is_ok()
    }

    /// Readiness off, root context cancelled, then every loop joined
    /// within the grace: an in-flight attempt sees its context cancelled
    /// and completes as a retry, a tick ends at its next await, the server
    /// finishes in-flight requests. Loops still running past the grace are
    /// named in `Error::Stuck` and abandoned to lease expiry.
    pub async fn shutdown(self) -> Result<(), Error> {
        self.health.ready.store(false, Ordering::SeqCst);
        self.ctx.cancel();
        let deadline = tokio::time::Instant::now() + self.grace;
        let mut stuck = Vec::new();
        for (name, task) in self.tasks {
            match tokio::time::timeout_at(deadline, task).await {
                Ok(_) => {}
                Err(_) => {
                    tracing::error!(worker = %name, "worker did not drain within the shutdown grace; abandoning it");
                    stuck.push(name);
                }
            }
        }
        let _ = tokio::time::timeout_at(deadline, self.bus_task).await;
        let server = match tokio::time::timeout_at(deadline, self.server).await {
            Ok(Ok(r)) => r.map_err(Error::Server),
            Ok(Err(_)) => Ok(()),
            Err(_) => {
                tracing::error!("the server did not finish its requests within the shutdown grace");
                Ok(())
            }
        };
        self.pool.close().await;
        tracing::info!(stuck = stuck.len(), "shutdown complete");
        if !stuck.is_empty() {
            return Err(Error::Stuck(stuck));
        }
        server
    }
}

struct Health {
    ready: AtomicBool,
    pool: PgPool,
    loops: Vec<(String, Arc<AtomicBool>)>,
}

impl Health {
    async fn check(&self) -> Result<String, String> {
        if !self.ready.load(Ordering::SeqCst) {
            return Err("not ready: wiring".to_string());
        }
        let dead: Vec<&str> = self
            .loops
            .iter()
            .filter(|(_, alive)| !alive.load(Ordering::SeqCst))
            .map(|(n, _)| n.as_str())
            .collect();
        if !dead.is_empty() {
            return Err(format!(
                "not ready: worker loop(s) ended: {}",
                dead.join(", ")
            ));
        }
        match tokio::time::timeout(
            READY_DB_TIMEOUT,
            sqlx::query("SELECT 1").execute(&self.pool),
        )
        .await
        {
            Ok(Ok(_)) => Ok(format!(
                "ready: database ok, {} worker loop(s) alive",
                self.loops.len()
            )),
            Ok(Err(e)) => Err(format!("not ready: database: {e}")),
            Err(_) => Err("not ready: database: probe timed out".to_string()),
        }
    }
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn readyz(State(health): State<Arc<Health>>) -> (StatusCode, String) {
    match health.check().await {
        Ok(msg) => (StatusCode::OK, format!("{msg}\n")),
        Err(msg) => (StatusCode::SERVICE_UNAVAILABLE, format!("{msg}\n")),
    }
}
