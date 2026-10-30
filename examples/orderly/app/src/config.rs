//! Typed configuration. Read from the environment the Deployment sets
//! (k8s/app/deployment.yaml): the database URL from the CNPG-minted Secret,
//! the public base URL, the Kratos public URL, where the config seed is and
//! which of its files apply. Per-nanoservice settings (a
//! schedule's interval, a provider's endpoint) are added here as fields and
//! passed to the nanoservice's constructor in main.rs; a variable nothing
//! reads is not declared (the Kratos admin URL joins when an identity
//! lookup needs it).

use std::fmt;

use basable_config::Environment;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub app: basable_app::Config,
    /// The public origin of this deployment (https://<host>).
    pub public_base_url: String,
    /// Kratos public API, for session validation (whoami).
    pub kratos_public_url: String,
    /// The declarative configuration seed: `config/base` in the repository,
    /// `/config/base` in the image (CONFIG_SEED_DIR).
    #[serde(default = "default_seed_dir")]
    pub config_seed_dir: String,
    /// Which seed files apply (CONFIG_ENV): `prod` in a deployment, `dev` on
    /// a laptop, `test` in the tests. The type carries no serde impl, so it
    /// is read by hand in `load`.
    #[serde(skip, default = "default_config_env")]
    pub config_env: Environment,
}

fn default_seed_dir() -> String {
    "config/base".to_owned()
}

fn default_config_env() -> Environment {
    Environment::Prod
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        Ok(Self {
            app: basable_app::Config::from_env()?,
            public_base_url: env("PUBLIC_BASE_URL")?,
            kratos_public_url: env("KRATOS_PUBLIC_URL")?,
            config_seed_dir: std::env::var("CONFIG_SEED_DIR").ok().filter(|s| !s.is_empty()).unwrap_or_else(default_seed_dir),
            config_env: match std::env::var("CONFIG_ENV") {
                Ok(v) if !v.is_empty() => v.parse().map_err(|_| ConfigError::Invalid("CONFIG_ENV", v))?,
                _ => default_config_env(),
            },
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
    /// A variable holds a value outside its vocabulary.
    Invalid(&'static str, String),
    /// The framework's own variables (the database URL, the listen address).
    App(basable_app::ConfigError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Missing(name) => write!(f, "{name} is not set"),
            ConfigError::Invalid(name, value) => write!(f, "{name}: {value:?} is not a valid value"),
            ConfigError::App(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Missing(_) | ConfigError::Invalid(..) => None,
            ConfigError::App(e) => Some(e),
        }
    }
}

impl From<basable_app::ConfigError> for ConfigError {
    fn from(e: basable_app::ConfigError) -> Self {
        ConfigError::App(e)
    }
}
