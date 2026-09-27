//! The runtime a basable nanoservice binary is assembled on. A tenant's
//! `app/src/main.rs` is the whole story:
//!
//! ```text
//! basable_app::tracing::init_json();
//! let cfg = config::Config::load()?;                       // flattens basable_app::Config
//! let app = App::new(cfg.app.clone()).expect_migrations(MIGRATIONS).connect().await?;
//! let catalog = Catalog::new(&app, app.pool::<catalog::Schema>().await?).await;
//! let router: &'static AppMessenger = Box::leak(Box::new(AppMessenger::new(api, catalog, ..)));
//! app.serve()
//!     .connect(api.connect_router(router))
//!     .raw(api.raw_routes(router))
//!     .auth(Validator::kratos(&cfg.kratos_public_url))
//!     .worker("catalog", catalog::types::product::worker(router, router.catalog()))
//!     .ticker("catalog", catalog::worker::tickers(router, router.catalog()))
//!     .run()
//!     .await
//! ```
//!
//! - [`Config`]: the framework's variables from the environment.
//! - [`App::new`] → [`Unbooted::connect`]: the framework pool with a bounded
//!   wait, the migration-ledger gate, the buses. [`App::pool`] opens one
//!   [`basable_db::NanoPool`] per stateful nanoservice under the connection
//!   budget.
//! - [`App::serve`] → [`Serve`]: routes, processing-object workers (wakes
//!   from the one [`WakeBus`] listener per process), [`Ticker`]s;
//!   [`Serve::start`] returns a [`Running`] app, [`Serve::run`] waits for
//!   SIGTERM and drains.
//! - `/healthz` answers while the process lives; `/readyz` once wiring is
//!   done, the database answers and every loop is alive.
//! - [`basable_pubsub::Bus`] through [`App::bus`], for cross-replica
//!   broadcast.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod boot;
mod config;
mod error;
mod serve;
mod ticker;
/// Tracing to stdout: `init_json` for production, `init_pretty` for a
/// terminal.
#[path = "logging.rs"]
pub mod tracing;
mod wake;

pub use boot::{APP_POOL_CONNECTIONS, App, Unbooted};
pub use config::{Config, ConfigError, var};
pub use error::Error;
pub use serve::{Running, Serve};
pub use ticker::{TickFuture, Ticker};
pub use wake::WakeBus;

pub use basable_auth::{AuthCtx, Identity, Validator};
pub use basable_connect::{ConnectRouter, request_ctx};
pub use basable_pubsub::Bus;
