//! The boot sequence: `App::new(config).connect().await` opens the
//! framework's own pool with a bounded wait, verifies the migration ledger
//! holds every version the binary embeds, and prepares the pubsub bus. The
//! connected [`App`] then opens one [`NanoPool`] per stateful nanoservice
//! under the connection budget, and [`App::serve`] moves on to wiring.

use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use basable_db::migrate::{self, MigrateError};
use basable_db::{NanoPool, PoolConfig, Stateful};
use basable_pubsub::Bus;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

use crate::config::Config;
use crate::error::Error;
use crate::serve::Serve;

/// The framework's own pool: the ledger check, the pubsub listener and its
/// publishes, the readiness probe. Processing-object wakes hold no
/// connection: they are in process.
pub const APP_POOL_CONNECTIONS: u32 = 3;

/// How often boot retries the database while waiting.
const BOOT_RETRY: Duration = Duration::from_secs(1);

/// An app before `connect`: the configuration and what boot must find.
pub struct Unbooted {
    cfg: Config,
    options: Option<PgConnectOptions>,
    migrations: &'static [&'static str],
}

impl Unbooted {
    /// The migration versions this binary embeds. Boot waits for the
    /// ledger to hold every one of them (the dbmate Job applies them in
    /// an earlier wave) and refuses to serve otherwise, so a binary never
    /// runs against a schema behind it.
    pub fn expect_migrations(mut self, versions: &'static [&'static str]) -> Self {
        self.migrations = versions;
        self
    }

    /// Connect options instead of the configured URL, for a test that has
    /// a database's options rather than a URL.
    pub fn connect_options(mut self, options: PgConnectOptions) -> Self {
        self.options = Some(options);
        self
    }

    /// Opens the framework pool and verifies the ledger, retrying every
    /// second within the boot wait: the database may still be starting or
    /// the migration Job still running. Past the wait, the last error is
    /// the boot error.
    pub async fn connect(self) -> Result<App, Error> {
        let options = match self.options {
            Some(o) => o,
            None => {
                PgConnectOptions::from_str(&self.cfg.database_url).map_err(Error::DatabaseUrl)?
            }
        };
        let deadline = Instant::now() + self.cfg.boot_wait();
        let mut attempts = 0u32;
        let pool = loop {
            attempts += 1;
            // sqlx retries a refused connection until the acquire timeout,
            // so the attempt's own timeout is what bounds the wait.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let acquire = remaining.clamp(Duration::from_secs(1), Duration::from_secs(10));
            match open_and_verify(&options, self.migrations, acquire).await {
                Ok(pool) => break pool,
                Err(e) if Instant::now() < deadline => {
                    tracing::warn!(attempt = attempts, error = %e, "database not ready; retrying");
                    tokio::time::sleep(BOOT_RETRY).await;
                }
                Err(BootStep::Connect(source)) => return Err(Error::Connect { attempts, source }),
                Err(BootStep::Ledger(e)) => return Err(Error::Migrations(e)),
            }
        };
        tracing::info!(
            attempts,
            migrations = self.migrations.len(),
            "database ready"
        );
        Ok(App {
            cfg: self.cfg,
            options,
            bus: Arc::new(Bus::new(pool.clone())),
            pool,
            budget: Mutex::new(APP_POOL_CONNECTIONS),
        })
    }
}

enum BootStep {
    Connect(sqlx::Error),
    Ledger(MigrateError),
}

impl std::fmt::Display for BootStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootStep::Connect(e) => write!(f, "connect: {e}"),
            BootStep::Ledger(MigrateError::Missing(v)) => {
                write!(f, "the ledger lacks {}", v.join(", "))
            }
            BootStep::Ledger(e) => write!(f, "ledger: {e}"),
        }
    }
}

async fn open_and_verify(
    options: &PgConnectOptions,
    migrations: &[&str],
    acquire: Duration,
) -> Result<PgPool, BootStep> {
    let pool = PgPoolOptions::new()
        .max_connections(APP_POOL_CONNECTIONS)
        .min_connections(0)
        .acquire_timeout(acquire)
        .connect_with(options.clone())
        .await
        .map_err(BootStep::Connect)?;
    if let Err(e) = migrate::verify(&pool, migrations).await {
        pool.close().await;
        return Err(BootStep::Ledger(e));
    }
    Ok(pool)
}

/// The connected app: the framework pool, the pubsub bus, and the budget
/// every nanoservice pool is opened under.
pub struct App {
    cfg: Config,
    options: PgConnectOptions,
    pool: PgPool,
    bus: Arc<Bus>,
    budget: Mutex<u32>,
}

impl App {
    /// The first step: configuration in, [`Unbooted::connect`] next. It
    /// returns the unbooted stage, not an `App`: only `connect` makes one.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(cfg: Config) -> Unbooted {
        Unbooted {
            cfg,
            options: None,
            migrations: &[],
        }
    }

    /// The configuration.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Opens nanoservice `N`'s pool: the `app` login switched to
    /// `nano_<name>` with its `search_path` pinned, proved at open. Counts
    /// against the connection budget; the pool that would exceed it is
    /// refused here, at boot.
    pub async fn pool<N: Stateful>(&self) -> Result<NanoPool<N>, Error> {
        let size = self.cfg.pool_max_connections;
        {
            let mut used = self.budget.lock().expect("budget lock");
            let requested = *used + size;
            if requested > self.cfg.connection_budget {
                return Err(Error::ConnectionBudget {
                    requested,
                    budget: self.cfg.connection_budget,
                });
            }
            *used = requested;
        }
        let config = PoolConfig {
            max_connections: size,
            ..PoolConfig::default()
        };
        NanoPool::<N>::connect(self.options.clone(), config)
            .await
            .map_err(|source| Error::Pool {
                nanoservice: N::NAME,
                source,
            })
    }

    /// The connections every pool opened so far would hold together.
    pub fn connections_reserved(&self) -> u32 {
        *self.budget.lock().expect("budget lock")
    }

    /// The pubsub bus: subscribe before [`Serve::start`], publish any
    /// time after.
    pub fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }

    /// The framework's own pool (the `app` login, no role switch). For the
    /// framework and tests; a nanoservice holds its [`NanoPool`].
    pub fn framework_pool(&self) -> &PgPool {
        &self.pool
    }

    /// Moves on to wiring: routes, workers, tickers, then `start` or `run`.
    pub fn serve(self) -> Serve {
        Serve::new(self)
    }

    pub(crate) fn into_parts(self) -> (Config, PgPool, Arc<Bus>) {
        (self.cfg, self.pool, self.bus)
    }
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("listen", &self.cfg.listen_addr())
            .field("connections_reserved", &self.connections_reserved())
            .finish_non_exhaustive()
    }
}
