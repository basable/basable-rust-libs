//! The framework's own configuration: what every nanoservice binary needs
//! before any component exists. Read from the environment the Deployment
//! sets; a tenant's `app/src/config.rs` flattens it into its own `Config`
//! and adds the application's variables.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde::Deserialize;

/// The variables, by name, so an error says which one is wrong.
pub mod var {
    /// The Postgres URL of the `app` login (the CNPG-minted Secret).
    pub const DATABASE_URL: &str = "DATABASE_URL";
    /// The listen port; default 8080.
    pub const PORT: &str = "PORT";
    /// The listen address; default `0.0.0.0`.
    pub const HOST: &str = "HOST";
    /// Connections per nanoservice pool; default 8.
    pub const POOL_MAX_CONNECTIONS: &str = "DATABASE_POOL_MAX_CONNECTIONS";
    /// The process's connection budget across every pool; default 100.
    pub const CONNECTION_BUDGET: &str = "DATABASE_CONNECTION_BUDGET";
    /// How long boot waits for the database and the migration ledger;
    /// default 60 seconds.
    pub const BOOT_WAIT_SECS: &str = "DATABASE_BOOT_WAIT_SECS";
    /// How long shutdown waits for workers to drain; default 30 seconds.
    pub const SHUTDOWN_GRACE_SECS: &str = "SHUTDOWN_GRACE_SECS";
}

/// The framework configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// The Postgres URL of the `app` login.
    pub database_url: String,
    /// The listen address.
    #[serde(default = "default_host")]
    pub host: IpAddr,
    /// The listen port.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Connections per nanoservice pool.
    #[serde(default = "default_pool_max")]
    pub pool_max_connections: u32,
    /// The connection budget across every pool this process opens (the
    /// nanoservice pools plus the framework's own). Exceeding it at boot
    /// is an error, not a surprise at the database's `max_connections`.
    #[serde(default = "default_budget")]
    pub connection_budget: u32,
    /// How long boot waits for the database to answer and the ledger to
    /// hold every expected migration.
    #[serde(default = "default_boot_wait")]
    pub boot_wait_secs: u64,
    /// How long shutdown waits for workers to drain before naming the
    /// stuck ones.
    #[serde(default = "default_shutdown_grace")]
    pub shutdown_grace_secs: u64,
}

fn default_host() -> IpAddr {
    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
}
fn default_port() -> u16 {
    8080
}
fn default_pool_max() -> u32 {
    8
}
fn default_budget() -> u32 {
    100
}
fn default_boot_wait() -> u64 {
    60
}
fn default_shutdown_grace() -> u64 {
    30
}

impl Config {
    /// A configuration with the defaults and this database URL.
    pub fn new(database_url: impl Into<String>) -> Config {
        Config {
            database_url: database_url.into(),
            host: default_host(),
            port: default_port(),
            pool_max_connections: default_pool_max(),
            connection_budget: default_budget(),
            boot_wait_secs: default_boot_wait(),
            shutdown_grace_secs: default_shutdown_grace(),
        }
    }

    /// Reads the configuration from the environment (see [`var`]).
    pub fn from_env() -> Result<Config, ConfigError> {
        let mut cfg = Config::new(required(var::DATABASE_URL)?);
        if let Some(v) = optional(var::HOST)? {
            cfg.host = parse(var::HOST, &v)?;
        }
        if let Some(v) = optional(var::PORT)? {
            cfg.port = parse(var::PORT, &v)?;
        }
        if let Some(v) = optional(var::POOL_MAX_CONNECTIONS)? {
            cfg.pool_max_connections = parse(var::POOL_MAX_CONNECTIONS, &v)?;
        }
        if let Some(v) = optional(var::CONNECTION_BUDGET)? {
            cfg.connection_budget = parse(var::CONNECTION_BUDGET, &v)?;
        }
        if let Some(v) = optional(var::BOOT_WAIT_SECS)? {
            cfg.boot_wait_secs = parse(var::BOOT_WAIT_SECS, &v)?;
        }
        if let Some(v) = optional(var::SHUTDOWN_GRACE_SECS)? {
            cfg.shutdown_grace_secs = parse(var::SHUTDOWN_GRACE_SECS, &v)?;
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// The listen address.
    pub fn listen_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }

    /// The boot wait.
    pub fn boot_wait(&self) -> Duration {
        Duration::from_secs(self.boot_wait_secs)
    }

    /// The shutdown grace.
    pub fn shutdown_grace(&self) -> Duration {
        Duration::from_secs(self.shutdown_grace_secs)
    }

    /// The rules a value must meet beyond parsing.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.database_url.is_empty() {
            return Err(ConfigError::Missing(var::DATABASE_URL));
        }
        if self.pool_max_connections == 0 {
            return Err(ConfigError::Invalid {
                name: var::POOL_MAX_CONNECTIONS,
                value: "0".into(),
                reason: "a pool needs at least one connection".into(),
            });
        }
        if self.connection_budget < self.pool_max_connections {
            return Err(ConfigError::Invalid {
                name: var::CONNECTION_BUDGET,
                value: self.connection_budget.to_string(),
                reason: format!(
                    "smaller than one pool ({} = {})",
                    var::POOL_MAX_CONNECTIONS,
                    self.pool_max_connections
                ),
            });
        }
        Ok(())
    }
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    optional(name)?.ok_or(ConfigError::Missing(name))
}

fn optional(name: &'static str) -> Result<Option<String>, ConfigError> {
    match std::env::var(name) {
        Ok(v) if v.is_empty() => Ok(None),
        Ok(v) => Ok(Some(v)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::Invalid {
            name,
            value: "<not unicode>".into(),
            reason: "not valid Unicode".into(),
        }),
    }
}

fn parse<T: std::str::FromStr>(name: &'static str, value: &str) -> Result<T, ConfigError>
where
    T::Err: fmt::Display,
{
    value.parse().map_err(|e: T::Err| ConfigError::Invalid {
        name,
        value: value.to_string(),
        reason: e.to_string(),
    })
}

/// Why the configuration could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// A required variable is not set.
    Missing(&'static str),
    /// A variable does not parse or breaks a rule.
    Invalid {
        /// The variable.
        name: &'static str,
        /// Its value.
        value: String,
        /// What is wrong with it.
        reason: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Missing(name) => write!(f, "{name} is not set"),
            ConfigError::Invalid {
                name,
                value,
                reason,
            } => write!(f, "{name}={value:?}: {reason}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_and_the_rules() {
        let cfg = Config::new("postgres://app@db/app");
        assert_eq!(cfg.listen_addr().to_string(), "0.0.0.0:8080");
        assert_eq!(cfg.boot_wait(), Duration::from_secs(60));
        assert!(cfg.validate().is_ok());
        let mut small = cfg.clone();
        small.connection_budget = 4;
        assert!(
            matches!(small.validate(), Err(ConfigError::Invalid { name, .. }) if name == var::CONNECTION_BUDGET)
        );
        let mut zero = cfg;
        zero.pool_max_connections = 0;
        assert!(zero.validate().is_err());
        assert_eq!(
            ConfigError::Missing(var::DATABASE_URL).to_string(),
            "DATABASE_URL is not set"
        );
    }

    #[test]
    fn a_value_that_does_not_parse_names_itself() {
        let err = parse::<u16>(var::PORT, "eighty").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { name: "PORT", .. }));
        assert!(err.to_string().starts_with("PORT=\"eighty\": "));
    }
}
