//! The binary: boot, wire, serve. Runs as several identical replicas.
//!
//! Sequence: typed config from the environment (exit 2 on invalid) → connect
//! (the framework pool with a bounded wait; a migration this binary embeds
//! that the ledger lacks after the wait is fatal, exit 3 naming it) → one
//! pool per stateful nanoservice → build every component → the concrete
//! AppMessenger, leaked as `&'static` → the Connect router, the raw routes,
//! the auth layer, the workers → run, joining every loop on SIGTERM.
//! Migrations are the dbmate Job's (k8s/dbmate); the app never migrates.
//!
//! The scaffolder wires each nanoservice between the markers; edit outside
//! them freely.

mod config;
// The dbmate versions under db/app/migrations, listed by the build
// (app/BUILD.bazel): what boot verifies the ledger holds.
mod migrations;

use std::fmt;
use std::process::ExitCode;

use basable_app::{App, Validator};
use messenger::AppMessenger;

// basable:nanoservice-use-begin
use catalog::Catalog;
use order::Order;
use notifier::Notifier;
// basable:nanoservice-use-end

#[tokio::main]
async fn main() -> ExitCode {
    basable_app::tracing::init_json();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            e.exit_code()
        }
    }
}

async fn run() -> Result<(), BootError> {
    let cfg = config::Config::load()?;
    tracing::info!(public_base_url = %cfg.public_base_url, "booting");
    let app = App::new(cfg.app.clone())
        .expect_migrations(migrations::MIGRATIONS)
        .connect()
        .await
        .map_err(BootError::Database)?;

    let api = api::Api::new();
    // basable:nanoservice-build-begin
    let catalog = Catalog::new(&app, app.pool::<catalog::Schema>().await.map_err(BootError::Database)?);
    let order = Order::new(&app, app.pool::<order::Schema>().await.map_err(BootError::Database)?).await;
    let notifier = Notifier::new(&app);
    // basable:nanoservice-build-end

    // The router is passed by shared reference into every handler and
    // reconciler (static dispatch, no Arc<dyn>); leaking one value per
    // process is the price, and it is negligible. Every component lives in
    // it: the Connect server takes its component from an accessor, and
    // `.components(router)` starts every component's loops (its workers and
    // tickers, which each component lists itself).
    let router: &'static AppMessenger = Box::leak(Box::new(AppMessenger::new(
        api,
        // basable:router-args-begin
        catalog,
        order,
        notifier,
        // basable:router-args-end
    )));
    let api: &'static api::Api = router.api();

    app.serve()
        .connect(api.connect_router(router))
        .raw(api.raw_routes(router))
        .auth(Validator::kratos(&cfg.kratos_public_url).public_prefixes(["/api/webhooks/"]))
        .components(router)
        .run()
        .await
        .map_err(BootError::Serve)
}

/// Why the process could not run, each with its exit code.
#[derive(Debug)]
enum BootError {
    /// The environment is missing or malformed: exit 2, nothing was opened.
    Config(config::ConfigError),
    /// A pool did not answer or a migration is missing: exit 3.
    Database(basable_app::Error),
    /// The server or a worker failed after boot: exit 1.
    Serve(basable_app::Error),
}

impl BootError {
    fn exit_code(&self) -> ExitCode {
        match self {
            BootError::Config(_) => ExitCode::from(2),
            BootError::Database(_) => ExitCode::from(3),
            BootError::Serve(_) => ExitCode::FAILURE,
        }
    }
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootError::Config(e) => write!(f, "invalid configuration: {e}"),
            BootError::Database(e) => write!(f, "database not ready: {e}"),
            BootError::Serve(e) => write!(f, "server failed: {e}"),
        }
    }
}

impl std::error::Error for BootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BootError::Config(e) => Some(e),
            BootError::Database(e) | BootError::Serve(e) => Some(e),
        }
    }
}

impl From<config::ConfigError> for BootError {
    fn from(e: config::ConfigError) -> Self {
        BootError::Config(e)
    }
}
