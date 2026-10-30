//! The binary: boot, wire, serve. Runs as several identical replicas.
//!
//! Sequence: typed config from the environment (exit 2 on invalid) → connect
//! (the framework pool with a bounded wait; a migration this binary embeds
//! that the ledger lacks after the wait is fatal, exit 3 naming it) → the
//! declarative catalogs (every config type's binder registered, the seed
//! under config/base applied in one transaction, exit 3 when it cannot be)
//! → one pool per stateful nanoservice → build every component → the
//! concrete AppMessenger, leaked as `&'static` → the Connect router, the raw
//! routes, the auth layer, the workers → run, joining every loop on SIGTERM.
//! Migrations are the dbmate Job's (k8s/dbmate); the app never migrates.
//!
//! The scaffolder wires each nanoservice between the markers; edit outside
//! them freely.

mod config;
// The dbmate versions under db/app/migrations, listed by the build
// (app/BUILD.bazel): what boot verifies the ledger holds.
mod migrations;

use std::fmt;
use std::path::Path;
use std::process::ExitCode;

use basable_app::{App, Validator};
use basable_config::{ConfigTypesBuilder, Repository as ConfigRepository, load_seed};
use messenger::AppMessenger;

// basable:nanoservice-use-begin
use catalog::Catalog;
use order::Order;
use notifier::Notifier;
use accounting::Accounting;
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

    // The declarative catalogs (config/base): every nanoservice owning
    // config types registers its binders between the markers, the seed is
    // applied in one transaction (a dangling reference fails before any
    // write; the files are picked by CONFIG_ENV), and `config` is the
    // repository those nanoservices read the catalogs through, handed to
    // their constructors below.
    let mut config_types = ConfigTypesBuilder::new();
    // basable:config-register-begin
    catalog::config::register(&mut config_types);
    // basable:config-register-end
    let config_types = config_types.build().map_err(BootError::ConfigTypes)?.shared();
    load_seed(app.framework_pool(), config_types.clone(), Path::new(&cfg.config_seed_dir), cfg.config_env)
        .await
        .map_err(BootError::Seed)?;
    #[allow(unused_variables)] // taken by the constructors of the nanoservices that own config types
    let config = ConfigRepository::new(app.framework_pool().clone(), config_types);

    let api = api::Api::new();
    // basable:nanoservice-build-begin
    let catalog = Catalog::new(&app, app.pool::<catalog::Schema>().await.map_err(BootError::Database)?, config.clone());
    let order = Order::new(&app, app.pool::<order::Schema>().await.map_err(BootError::Database)?).await;
    let notifier = Notifier::new(&app);
    let accounting = Accounting::new(&app, app.pool::<accounting::Schema>().await.map_err(BootError::Database)?).await;
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
        accounting,
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
    /// Two config types collide on an id, a name or a prefix: exit 2.
    ConfigTypes(basable_config::RegistryError),
    /// The config seed could not be applied (a malformed file, a dangling
    /// reference, the database): exit 3.
    Seed(basable_config::ConfigError),
    /// The server or a worker failed after boot: exit 1.
    Serve(basable_app::Error),
}

impl BootError {
    fn exit_code(&self) -> ExitCode {
        match self {
            BootError::Config(_) | BootError::ConfigTypes(_) => ExitCode::from(2),
            BootError::Database(_) | BootError::Seed(_) => ExitCode::from(3),
            BootError::Serve(_) => ExitCode::FAILURE,
        }
    }
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootError::Config(e) => write!(f, "invalid configuration: {e}"),
            BootError::Database(e) => write!(f, "database not ready: {e}"),
            BootError::ConfigTypes(e) => write!(f, "config types: {e}"),
            BootError::Seed(e) => write!(f, "config seed not applied: {e}"),
            BootError::Serve(e) => write!(f, "server failed: {e}"),
        }
    }
}

impl std::error::Error for BootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BootError::Config(e) => Some(e),
            BootError::Database(e) | BootError::Serve(e) => Some(e),
            BootError::ConfigTypes(e) => Some(e),
            BootError::Seed(e) => Some(e),
        }
    }
}

impl From<config::ConfigError> for BootError {
    fn from(e: config::ConfigError) -> Self {
        BootError::Config(e)
    }
}
