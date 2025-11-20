//! Type-safe wrapper ensuring only Balanced tier selection
//!
//! The BalancedSelector enforces the architectural invariant that the LLM-based
//! router always uses Balanced tier endpoints for routing decisions.

use crate::config::ModelEndpoint;
use crate::error::{AppError, AppResult};
use crate::models::endpoint_name::ExclusionSet;
use crate::models::selector::ModelSelector;
use crate::router::TargetModel;
use std::sync::Arc;

/// Type-safe selector that can ONLY select from Balanced tier
///
/// This newtype wrapper enforces the critical architectural invariant that the
/// LLM-based router always queries the Balanced tier (30B model) for routing decisions.
///
/// # Why Balanced Tier Only?
///
/// - **FAST (8B)**: Too unreliable for routing decisions - could misroute expensive
///   requests or waste resources. Bad routing decisions cost more than savings.
///
/// - **BALANCED (30B)**: Sweet spot for routing - good reasoning for classification
///   with acceptable latency (~100-500ms). Smart enough for reliable decisions.
///
/// - **DEEP (120B)**: Overkill for routing - the latency overhead (~2-5s) would
///   often exceed the time to just use BALANCED for the user query itself.
///
/// # Type Safety
///
/// By enforcing this via types, it becomes **impossible to violate** this invariant
/// at compile time. A developer cannot accidentally change the router to use Fast
/// or Deep tier without a compiler error.
#[derive(Debug)]
pub struct BalancedSelector {
    inner: Arc<ModelSelector>,
}

impl BalancedSelector {
    /// Create a new BalancedSelector
    ///
    /// Returns an error if the ModelSelector has no Balanced tier endpoints configured.
    /// This validation ensures the router can never be in an invalid state.
    ///
    /// # Errors
    ///
    /// Returns `AppError::Config` if no balanced tier endpoints are configured.
    pub fn new(selector: Arc<ModelSelector>) -> AppResult<Self> {
        if selector.endpoint_count(TargetModel::Balanced) == 0 {
            return Err(AppError::Config(
                "BalancedSelector requires at least one balanced tier endpoint".to_string(),
            ));
        }
        Ok(Self { inner: selector })
    }

    /// Select a Balanced tier endpoint with health filtering and exclusion
    ///
    /// This is the ONLY way to select an endpoint through a BalancedSelector,
    /// and it can ONLY return Balanced tier endpoints.
    ///
    /// # Arguments
    /// * `exclude` - Set of endpoint names to exclude (for retry logic)
    ///
    /// # Returns
    /// - `Some(&ModelEndpoint)` if a healthy, non-excluded Balanced endpoint exists
    /// - `None` if all Balanced endpoints are unhealthy or excluded
    pub async fn select_balanced(&self, exclude: &ExclusionSet) -> Option<&ModelEndpoint> {
        self.inner.select(TargetModel::Balanced, exclude).await
    }

    /// Get the number of configured Balanced tier endpoints
    pub fn endpoint_count(&self) -> usize {
        self.inner.endpoint_count(TargetModel::Balanced)
    }

    /// Get a reference to the health checker for external use (e.g., marking success/failure)
    pub fn health_checker(&self) -> &Arc<crate::models::health::HealthChecker> {
        self.inner.health_checker()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::models::EndpointName;

    fn create_test_config_with_balanced() -> Arc<Config> {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-2"
base_url = "http://localhost:1235/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_model = "balanced"
"#;
        let config: Config = toml::from_str(toml).expect("should parse config");
        Arc::new(config)
    }

    fn create_test_config_without_balanced() -> Arc<Config> {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[models]
balanced = []

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_model = "balanced"
"#;
        let config: Config = toml::from_str(toml).expect("should parse config");
        Arc::new(config)
    }

    #[tokio::test]
    async fn test_balanced_selector_new_with_balanced_endpoints() {
        let config = create_test_config_with_balanced();
        let selector = Arc::new(ModelSelector::new(config));

        let result = BalancedSelector::new(selector);
        assert!(
            result.is_ok(),
            "should create BalancedSelector with balanced endpoints"
        );
    }

    #[tokio::test]
    async fn test_balanced_selector_new_without_balanced_endpoints() {
        let config = create_test_config_without_balanced();
        let selector = Arc::new(ModelSelector::new(config));

        let result = BalancedSelector::new(selector);
        assert!(result.is_err(), "should fail without balanced endpoints");

        let err = result.unwrap_err();
        match err {
            AppError::Config(msg) => {
                assert!(
                    msg.contains("balanced tier endpoint"),
                    "error should mention balanced tier requirement, got: {}",
                    msg
                );
            }
            _ => panic!("expected Config error, got: {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_balanced_selector_selects_balanced_endpoint() {
        let config = create_test_config_with_balanced();
        let selector = Arc::new(ModelSelector::new(config));
        let balanced_selector =
            BalancedSelector::new(selector).expect("should create BalancedSelector");

        let exclude = ExclusionSet::new();
        let endpoint = balanced_selector.select_balanced(&exclude).await;

        assert!(endpoint.is_some(), "should select a balanced endpoint");
        let endpoint = endpoint.unwrap();
        assert!(
            endpoint.name().starts_with("balanced-"),
            "selected endpoint should be from balanced tier, got: {}",
            endpoint.name()
        );
    }

    #[tokio::test]
    async fn test_balanced_selector_respects_exclusion() {
        let config = create_test_config_with_balanced();
        let selector = Arc::new(ModelSelector::new(config));
        let balanced_selector =
            BalancedSelector::new(selector).expect("should create BalancedSelector");

        // First selection should succeed
        let exclude = ExclusionSet::new();
        let first = balanced_selector.select_balanced(&exclude).await;
        assert!(first.is_some());

        // Build exclusion set with both balanced endpoints
        let mut exclude = ExclusionSet::new();
        exclude.insert(EndpointName::from("balanced-1"));
        exclude.insert(EndpointName::from("balanced-2"));

        // Should return None when all balanced endpoints excluded
        let excluded = balanced_selector.select_balanced(&exclude).await;
        assert!(
            excluded.is_none(),
            "should return None when all balanced endpoints excluded"
        );
    }

    #[tokio::test]
    async fn test_balanced_selector_endpoint_count() {
        let config = create_test_config_with_balanced();
        let selector = Arc::new(ModelSelector::new(config));
        let balanced_selector =
            BalancedSelector::new(selector).expect("should create BalancedSelector");

        assert_eq!(
            balanced_selector.endpoint_count(),
            2,
            "should have 2 balanced endpoints"
        );
    }
}
