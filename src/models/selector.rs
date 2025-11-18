//! Model selection logic for choosing from multiple endpoints
//!
//! Implements simple round-robin selection (Phase 2a)

use crate::config::{Config, ModelEndpoint};
use crate::router::TargetModel;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Selects appropriate model endpoint from multi-model configuration
///
/// Phase 2a: Simple round-robin selection among available endpoints.
/// Phase 2b/2c will add weighted load balancing and priority selection.
pub struct ModelSelector {
    config: Arc<Config>,
    // Round-robin counters for each tier
    fast_counter: AtomicUsize,
    balanced_counter: AtomicUsize,
    deep_counter: AtomicUsize,
}

impl ModelSelector {
    /// Create a new ModelSelector from configuration
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            fast_counter: AtomicUsize::new(0),
            balanced_counter: AtomicUsize::new(0),
            deep_counter: AtomicUsize::new(0),
        }
    }

    /// Select an endpoint for the given target model tier using round-robin
    ///
    /// Returns None if the requested tier has no available endpoints.
    pub fn select(&self, target: TargetModel) -> Option<&ModelEndpoint> {
        let (endpoints, counter) = match target {
            TargetModel::Fast => (&self.config.models.fast, &self.fast_counter),
            TargetModel::Balanced => (&self.config.models.balanced, &self.balanced_counter),
            TargetModel::Deep => (&self.config.models.deep, &self.deep_counter),
        };

        if endpoints.is_empty() {
            tracing::error!(
                tier = ?target,
                "No endpoints configured for tier - check config.toml"
            );
            return None;
        }

        // Get current counter value and increment atomically with wrapping to prevent overflow
        // Using fetch_update instead of fetch_add to handle potential overflow gracefully
        let index = counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| {
                Some(val.wrapping_add(1))
            })
            .unwrap_or(0)
            % endpoints.len();
        let selected_endpoint = &endpoints[index];

        tracing::debug!(
            tier = ?target,
            endpoint_name = %selected_endpoint.name,
            endpoint_index = index,
            total_endpoints = endpoints.len(),
            "Selected endpoint via round-robin"
        );

        Some(selected_endpoint)
    }

    /// Get the number of available endpoints for a target tier
    pub fn endpoint_count(&self, target: TargetModel) -> usize {
        match target {
            TargetModel::Fast => self.config.models.fast.len(),
            TargetModel::Balanced => self.config.models.balanced.len(),
            TargetModel::Deep => self.config.models.deep.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ModelsConfig, ObservabilityConfig, RoutingConfig, RoutingStrategy, ServerConfig,
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
                fast: vec![
                    ModelEndpoint {
                        name: "fast-1".to_string(),
                        base_url: "http://localhost:1234/v1".to_string(),
                        max_tokens: 2048,
                        temperature: 0.7,
                        weight: 1.0,
                        priority: 1,
                    },
                    ModelEndpoint {
                        name: "fast-2".to_string(),
                        base_url: "http://localhost:1235/v1".to_string(),
                        max_tokens: 2048,
                        temperature: 0.7,
                        weight: 1.0,
                        priority: 1,
                    },
                ],
                balanced: vec![ModelEndpoint {
                    name: "balanced-1".to_string(),
                    base_url: "http://localhost:1236/v1".to_string(),
                    max_tokens: 4096,
                    temperature: 0.7,
                    weight: 1.0,
                    priority: 1,
                }],
                deep: vec![ModelEndpoint {
                    name: "deep-1".to_string(),
                    base_url: "http://localhost:1237/v1".to_string(),
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
    fn test_selector_new_creates_selector() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // Verify we can create a selector
        assert_eq!(selector.endpoint_count(TargetModel::Fast), 2);
        assert_eq!(selector.endpoint_count(TargetModel::Balanced), 1);
        assert_eq!(selector.endpoint_count(TargetModel::Deep), 1);
    }

    #[test]
    fn test_selector_select_returns_endpoint() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // Should return some endpoint for each tier
        assert!(selector.select(TargetModel::Fast).is_some());
        assert!(selector.select(TargetModel::Balanced).is_some());
        assert!(selector.select(TargetModel::Deep).is_some());
    }

    #[test]
    fn test_selector_round_robin_fast_tier() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // First call should return fast-1
        let first = selector.select(TargetModel::Fast).unwrap();
        assert_eq!(first.name, "fast-1");

        // Second call should return fast-2
        let second = selector.select(TargetModel::Fast).unwrap();
        assert_eq!(second.name, "fast-2");

        // Third call should wrap around to fast-1
        let third = selector.select(TargetModel::Fast).unwrap();
        assert_eq!(third.name, "fast-1");
    }

    #[test]
    fn test_selector_single_endpoint_tier() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // Balanced tier has only one endpoint, should return same one
        let first = selector.select(TargetModel::Balanced).unwrap();
        let second = selector.select(TargetModel::Balanced).unwrap();

        assert_eq!(first.name, "balanced-1");
        assert_eq!(second.name, "balanced-1");
    }

    #[test]
    fn test_selector_endpoint_count() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        assert_eq!(selector.endpoint_count(TargetModel::Fast), 2);
        assert_eq!(selector.endpoint_count(TargetModel::Balanced), 1);
        assert_eq!(selector.endpoint_count(TargetModel::Deep), 1);
    }

    #[test]
    fn test_selector_returns_none_for_empty_tier() {
        let mut config = create_test_config();
        config.models.fast = vec![]; // Empty tier
        let selector = ModelSelector::new(Arc::new(config));

        let result = selector.select(TargetModel::Fast);
        assert!(result.is_none(), "should return None for empty tier");
    }

    #[tokio::test]
    async fn test_selector_concurrent_round_robin() {
        let config = Arc::new(create_test_config());
        let selector = Arc::new(ModelSelector::new(config));

        // Spawn 10 concurrent tasks selecting from Fast tier (which has 2 endpoints)
        let mut handles = vec![];
        for _ in 0..10 {
            let sel = selector.clone();
            handles.push(tokio::spawn(async move {
                sel.select(TargetModel::Fast).map(|e| e.name.clone())
            }));
        }

        // Collect results
        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        // Verify all selections succeeded
        assert_eq!(results.len(), 10);
        for result in &results {
            assert!(result.is_some(), "all selections should succeed");
        }

        // Verify both endpoints were selected (round-robin rotated)
        let endpoint_names: Vec<String> = results.into_iter().flatten().collect();
        let has_fast1 = endpoint_names.iter().any(|n| n == "fast-1");
        let has_fast2 = endpoint_names.iter().any(|n| n == "fast-2");

        assert!(
            has_fast1 && has_fast2,
            "both endpoints should be selected during concurrent access, got: {:?}",
            endpoint_names
        );
    }
}
