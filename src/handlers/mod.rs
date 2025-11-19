//! HTTP request handlers for Octoroute API

use crate::config::Config;
use crate::models::ModelSelector;
use crate::router::RuleBasedRouter;
use std::sync::Arc;

pub mod chat;
pub mod health;
pub mod models;

/// Application state shared across all handlers
///
/// Contains configuration, model selector, and router instances.
/// All fields are Arc'd for cheap cloning across Axum handlers.
#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    selector: Arc<ModelSelector>,
    router: Arc<RuleBasedRouter>,
}

impl AppState {
    /// Create a new AppState from configuration
    ///
    /// Accepts `Arc<Config>` to avoid unnecessary cloning when the configuration
    /// is already wrapped in an Arc.
    pub fn new(config: Arc<Config>) -> Self {
        let selector = Arc::new(ModelSelector::new(config.clone()));
        let router = Arc::new(RuleBasedRouter::new());

        Self {
            config,
            selector,
            router,
        }
    }

    /// Get reference to the configuration
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Get reference to the model selector
    pub fn selector(&self) -> &ModelSelector {
        &self.selector
    }

    /// Get reference to the rule-based router
    pub fn router(&self) -> &RuleBasedRouter {
        &self.router
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config() -> Config {
        // ModelEndpoint fields are private - use TOML deserialization
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1235/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1236/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_model = "balanced"
"#;
        toml::from_str(toml).expect("should parse TOML config")
    }

    #[tokio::test]
    async fn test_appstate_new_creates_state() {
        let config = Arc::new(create_test_config());
        let state = AppState::new(config);

        // Verify we can create state and access components
        assert_eq!(state.config().server.port, 3000);
        assert_eq!(
            state
                .selector()
                .endpoint_count(crate::router::TargetModel::Fast),
            1
        );
    }

    #[tokio::test]
    async fn test_appstate_is_clonable() {
        let config = Arc::new(create_test_config());
        let state = AppState::new(config);

        // Clone should work (cheap Arc clone)
        let state2 = state.clone();
        assert_eq!(state2.config().server.port, 3000);
    }

    #[tokio::test]
    async fn test_appstate_provides_access_to_components() {
        let config = Arc::new(create_test_config());
        let state = AppState::new(config);

        // Should be able to access all components
        let _ = state.config();
        let _ = state.selector();
        let _ = state.router();
    }
}
