//! Telemetry and observability setup.
//!
//! Configures structured logging with tracing and tracing-subscriber.

use std::sync::Once;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

static INIT: Once = Once::new();

/// Initialize the process-wide structured logging subscriber.
///
/// This can only be called once per process. Subsequent calls are silently ignored.
///
/// Reads log level from RUST_LOG environment variable, defaulting to the
/// level specified in config (or "info" if not set).
///
/// # Examples
///
/// ```no_run
/// octoroute::telemetry::init("info");
/// tracing::info!("Application started");
/// ```
pub fn init(default_level: &str) {
    INIT.call_once(|| {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("octoroute={default_level}")));

        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    });
}
