//! HTTP request handlers for Octoroute API

use crate::config::Config;
use crate::models::ModelSelector;
use crate::router::RuleBasedRouter;
use std::sync::Arc;

pub mod chat;
pub mod health;

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
    pub fn new(config: Config) -> Self {
        let config_arc = Arc::new(config);
        let selector = Arc::new(ModelSelector::new(config_arc.clone()));
        let router = Arc::new(RuleBasedRouter::new());

        Self {
            config: config_arc,
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
    use crate::config::{
        ModelEndpoint, ModelsConfig, ObservabilityConfig, RoutingConfig, RoutingStrategy,
        ServerConfig,
    };
    use crate::router::Importance;

    fn create_test_config() -> Config {
        Config {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 3000,
                request_timeout_seconds: 30,
            },
            models: ModelsConfig {
                fast: vec![ModelEndpoint {
                    name: "fast-1".to_string(),
                    base_url: "http://localhost:1234/v1".to_string(),
                    max_tokens: 2048,
                    temperature: 0.7,
                    weight: 1.0,
                    priority: 1,
                }],
                balanced: vec![ModelEndpoint {
                    name: "balanced-1".to_string(),
                    base_url: "http://localhost:1235/v1".to_string(),
                    max_tokens: 4096,
                    temperature: 0.7,
                    weight: 1.0,
                    priority: 1,
                }],
                deep: vec![ModelEndpoint {
                    name: "deep-1".to_string(),
                    base_url: "http://localhost:1236/v1".to_string(),
                    max_tokens: 8192,
                    temperature: 0.7,
                    weight: 1.0,
                    priority: 1,
                }],
            },
            routing: RoutingConfig {
                strategy: RoutingStrategy::Rule,
                default_importance: Importance::Normal,
                router_model: "balanced".to_string(),
            },
            observability: ObservabilityConfig::default(),
        }
    }

    #[test]
    fn test_appstate_new_creates_state() {
        let config = create_test_config();
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

    #[test]
    fn test_appstate_is_clonable() {
        let config = create_test_config();
        let state = AppState::new(config);

        // Clone should work (cheap Arc clone)
        let state2 = state.clone();
        assert_eq!(state2.config().server.port, 3000);
    }

    #[test]
    fn test_appstate_provides_access_to_components() {
        let config = create_test_config();
        let state = AppState::new(config);

        // Should be able to access all components
        let _ = state.config();
        let _ = state.selector();
        let _ = state.router();
    }
}
