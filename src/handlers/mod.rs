//! HTTP request handlers for Octoroute API

use crate::config::{Config, RoutingStrategy};
use crate::error::{AppError, AppResult};
use crate::models::ModelSelector;
use crate::router::{HybridRouter, LlmBasedRouter, Router, RuleBasedRouter, TargetModel};
use std::sync::Arc;

type MetricsHandle = Arc<crate::metrics::Metrics>;

pub mod chat;
pub mod health;
pub mod metrics;
pub mod models;

/// Application state shared across all handlers
///
/// Contains configuration, model selector, and router instances.
///
/// All fields are wrapped in `Arc` for two reasons:
/// 1. **Thread safety**: Axum handlers run concurrently on separate threads and require
///    `Send + Sync` bounds. Arc provides atomic reference counting for safe sharing across threads.
/// 2. **Cheap cloning**: Axum clones state per request. Arc makes this O(1) instead of deep copying.
///
/// Do NOT replace Arc with Rc - it's not Send+Sync and will fail to compile with Axum.
///
/// The router type is determined by `config.routing.strategy`:
/// - `Rule`: Only rule-based routing (no balanced tier required)
/// - `Llm`: Only LLM-based routing (balanced tier required)
/// - `Hybrid`: Rule-based with LLM fallback (balanced tier required)
///
/// Also contains a Prometheus metrics collector for observability.
#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    selector: Arc<ModelSelector>,
    router: Arc<Router>,
    metrics: Arc<crate::metrics::Metrics>,
}

impl AppState {
    /// Create a new AppState from configuration
    ///
    /// Accepts `Arc<Config>` to avoid unnecessary cloning when the configuration
    /// is already wrapped in an Arc.
    ///
    /// # Errors
    /// Returns an error if:
    /// - Llm/Hybrid strategy is selected but no balanced tier endpoints are configured
    /// - Router construction fails for any other reason
    pub fn new(config: Arc<Config>) -> AppResult<Self> {
        let selector = Arc::new(ModelSelector::new(config.clone()));

        // Construct router based on config.routing.strategy
        let router = match config.routing.strategy {
            RoutingStrategy::Rule => {
                // Rule-only routing: no balanced tier required
                tracing::info!("Initializing rule-based router (no LLM routing)");
                Arc::new(Router::Rule(RuleBasedRouter::new()))
            }
            RoutingStrategy::Llm => {
                // LLM-only routing: router tier required
                let router_tier = match config.routing.router_model.as_str() {
                    "fast" => TargetModel::Fast,
                    "balanced" => TargetModel::Balanced,
                    "deep" => TargetModel::Deep,
                    invalid => {
                        return Err(AppError::Config(format!(
                            "Invalid router_model '{}'. Expected 'fast', 'balanced', or 'deep'. \
                             This indicates a bug - config validation should have caught this earlier.",
                            invalid
                        )));
                    }
                };

                tracing::info!(
                    "Initializing LLM-based router with {:?} tier for routing decisions",
                    router_tier
                );

                let llm_router = LlmBasedRouter::new(selector.clone(), router_tier)?;
                Arc::new(Router::Llm(llm_router))
            }
            RoutingStrategy::Hybrid => {
                // Hybrid routing: router tier required for LLM fallback
                let router_tier = match config.routing.router_model.as_str() {
                    "fast" => TargetModel::Fast,
                    "balanced" => TargetModel::Balanced,
                    "deep" => TargetModel::Deep,
                    invalid => {
                        return Err(AppError::Config(format!(
                            "Invalid router_model '{}'. Expected 'fast', 'balanced', or 'deep'. \
                             This indicates a bug - config validation should have caught this earlier.",
                            invalid
                        )));
                    }
                };

                tracing::info!(
                    "Initializing hybrid router (rule-based with LLM fallback using {:?} tier)",
                    router_tier
                );

                let hybrid_router = HybridRouter::new(config.clone(), selector.clone())?;
                Arc::new(Router::Hybrid(hybrid_router))
            }
            RoutingStrategy::Tool => {
                return Err(AppError::Config(
                    "Tool-based routing is not yet implemented. Use 'rule', 'llm', or 'hybrid'."
                        .to_string(),
                ));
            }
        };

        let metrics = {
            let m = crate::metrics::Metrics::new()
                .map_err(|e| AppError::Internal(format!("Failed to initialize metrics: {}", e)))?;
            tracing::info!("Metrics collection enabled");
            Arc::new(m)
        };

        Ok(Self {
            config,
            selector,
            router,
            metrics,
        })
    }

    /// Get reference to the configuration
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Get reference to the model selector
    pub fn selector(&self) -> &ModelSelector {
        &self.selector
    }

    /// Get reference to the router
    pub fn router(&self) -> &Router {
        &self.router
    }

    /// Get reference to the metrics collector
    ///
    /// Metrics are always enabled for observability.
    pub fn metrics(&self) -> MetricsHandle {
        self.metrics.clone()
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
        let state = AppState::new(config).expect("AppState::new should succeed with balanced tier");

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
        let state = AppState::new(config).expect("AppState::new should succeed with balanced tier");

        // Clone should work (cheap Arc clone)
        let state2 = state.clone();
        assert_eq!(state2.config().server.port, 3000);
    }

    #[tokio::test]
    async fn test_appstate_provides_access_to_components() {
        let config = Arc::new(create_test_config());
        let state = AppState::new(config).expect("AppState::new should succeed with balanced tier");

        // Should be able to access all components
        let _ = state.config();
        let _ = state.selector();
        let _ = state.router();
    }
}
