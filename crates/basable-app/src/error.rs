//! What can go wrong booting and running the app, each variant one exit
//! path of the tenant's `main`.

use std::fmt;
use std::net::SocketAddr;

use basable_db::migrate::MigrateError;

/// A boot or serve failure.
#[derive(Debug)]
pub enum Error {
    /// The database URL does not parse.
    DatabaseUrl(sqlx::Error),
    /// The database did not answer within the boot wait.
    Connect {
        /// How many attempts were made.
        attempts: u32,
        /// The last error.
        source: sqlx::Error,
    },
    /// The migration ledger lacks an expected version after the boot wait,
    /// or could not be read.
    Migrations(MigrateError),
    /// A nanoservice pool could not be opened (a missing role, a login that
    /// is not a member of it).
    Pool {
        /// The nanoservice.
        nanoservice: &'static str,
        /// The error.
        source: sqlx::Error,
    },
    /// Opening one more pool would exceed the connection budget.
    ConnectionBudget {
        /// The connections every pool would hold together.
        requested: u32,
        /// The budget.
        budget: u32,
    },
    /// The listen address could not be bound.
    Bind {
        /// The address.
        addr: SocketAddr,
        /// The error.
        source: std::io::Error,
    },
    /// The server failed while running.
    Server(std::io::Error),
    /// Two loops were registered under one `<component>/<loop>` name (the
    /// components registered twice, or one component with two loops of one
    /// name). Nothing was started.
    DuplicateLoop(String),
    /// Workers still running when the shutdown grace ran out, by name.
    /// They were abandoned; the framework's lease expiry recovers their
    /// claims.
    Stuck(Vec<String>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::DatabaseUrl(e) => write!(f, "DATABASE_URL does not parse: {e}"),
            Error::Connect { attempts, source } => {
                write!(
                    f,
                    "the database did not answer after {attempts} attempt(s): {source}"
                )
            }
            Error::Migrations(MigrateError::Missing(versions)) => write!(
                f,
                "the migration ledger lacks {} version(s) this binary embeds: {}",
                versions.len(),
                versions.join(", ")
            ),
            Error::Migrations(e) => write!(f, "the migration ledger could not be read: {e}"),
            Error::Pool {
                nanoservice,
                source,
            } => {
                write!(
                    f,
                    "the pool of nanoservice {nanoservice} could not be opened: {source}"
                )
            }
            Error::ConnectionBudget { requested, budget } => write!(
                f,
                "the pools would hold {requested} connections, over the budget of {budget}"
            ),
            Error::Bind { addr, source } => write!(f, "binding {addr}: {source}"),
            Error::Server(e) => write!(f, "the server failed: {e}"),
            Error::DuplicateLoop(name) => write!(
                f,
                "two loops are registered as {name}; a loop name must be unique"
            ),
            Error::Stuck(names) => write!(
                f,
                "{} worker(s) did not drain within the shutdown grace: {}",
                names.len(),
                names.join(", ")
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::DatabaseUrl(e)
            | Error::Connect { source: e, .. }
            | Error::Pool { source: e, .. } => Some(e),
            Error::Migrations(e) => Some(e),
            Error::Bind { source, .. } => Some(source),
            Error::Server(e) => Some(e),
            Error::ConnectionBudget { .. } | Error::DuplicateLoop(_) | Error::Stuck(_) => None,
        }
    }
}
