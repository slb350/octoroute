//! Telemetry and observability setup
//!
//! Configures structured logging with tracing and tracing-subscriber.

use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize tracing subscriber for structured logging
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
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!("octoroute={},tower_http=debug", default_level))
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_telemetry_module_exists() {
        // Note: We can't actually test init() fully because it can only be called once
        // per process. This test just verifies the module compiles.
        // Real testing would be done via integration tests.
    }
}
