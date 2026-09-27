//! Typed configuration. Read from the environment the Deployment sets
//! (k8s/app/deployment.yaml): the database URL from the CNPG-minted Secret,
//! the public base URL, the Kratos public URL. Per-nanoservice settings (a
//! schedule's interval, a provider's endpoint) are added here as fields and
//! passed to the nanoservice's constructor in main.rs; a variable nothing
//! reads is not declared (the Kratos admin URL joins when an identity
//! lookup needs it).

use std::fmt;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub app: basable_app::Config,
    /// The public origin of this deployment (https://<host>).
    pub public_base_url: String,
    /// Kratos public API, for session validation (whoami).
    pub kratos_public_url: String,
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        Ok(Self {
            app: basable_app::Config::from_env()?,
            public_base_url: env("PUBLIC_BASE_URL")?,
            kratos_public_url: env("KRATOS_PUBLIC_URL")?,
        })
    }
}

/// One required variable, by name, so the error says which one is missing.
fn env(name: &'static str) -> Result<String, ConfigError> {
    std::env::var(name).map_err(|_| ConfigError::Missing(name))
}

/// Why the configuration could not be loaded.
#[derive(Debug)]
pub enum ConfigError {
    /// A required variable is not set (or is not valid Unicode).
    Missing(&'static str),
    /// The framework's own variables (the database URL, the listen address).
    App(basable_app::ConfigError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Missing(name) => write!(f, "{name} is not set"),
            ConfigError::App(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Missing(_) => None,
            ConfigError::App(e) => Some(e),
        }
    }
}

impl From<basable_app::ConfigError> for ConfigError {
    fn from(e: basable_app::ConfigError) -> Self {
        ConfigError::App(e)
    }
}
