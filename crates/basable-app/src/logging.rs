//! Tracing to stdout as JSON lines, filtered by `RUST_LOG` (default
//! `info`). Exposed as `basable_app::tracing`.

use tracing_subscriber::EnvFilter;

/// Installs the JSON subscriber. Idempotent: a second call (a test that
/// boots two apps) is a no-op.
pub fn init_json() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(false)
        .try_init();
}

/// Installs a human-readable subscriber, for local runs and tests.
pub fn init_pretty() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
