//! Model selection logic for choosing from multiple endpoints
//!
//! Implements weighted random selection (Phase 2b)

use crate::config::{Config, ModelEndpoint};
use crate::router::TargetModel;
use rand::Rng;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Selects appropriate model endpoint from multi-model configuration
///
/// Phase 2b: Weighted random selection respecting endpoint weight configuration.
/// Higher weight values receive proportionally more traffic.
pub struct ModelSelector {
    config: Arc<Config>,
    // Selection counters for metrics tracking (not used for round-robin)
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

    /// Select an endpoint for the given target model tier using weighted random selection
    ///
    /// Uses the `weight` field from ModelEndpoint configuration to distribute load.
    /// Higher weights receive proportionally more traffic.
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

        // Increment selection counter for metrics (atomic operation)
        counter.fetch_add(1, Ordering::Relaxed);

        // Calculate total weight of all endpoints
        let total_weight: f64 = endpoints.iter().map(|e| e.weight).sum();

        // Handle zero or negative total weight (all endpoints disabled/misconfigured)
        if total_weight <= 0.0 {
            tracing::warn!(
                tier = ?target,
                total_weight = total_weight,
                endpoints_count = endpoints.len(),
                "All endpoints in tier have zero/negative weight, falling back to uniform selection"
            );

            // Fall back to uniform random selection
            let mut rng = rand::thread_rng();
            let index = rng.gen_range(0..endpoints.len());
            let endpoint = &endpoints[index];

            tracing::info!(
                tier = ?target,
                endpoint_name = %endpoint.name,
                endpoint_index = index,
                "Selected endpoint via uniform fallback (zero total weight)"
            );

            return Some(endpoint);
        }

        // Generate random number in range [0, total_weight)
        let mut rng = rand::thread_rng();
        let random_weight = rng.gen_range(0.0..total_weight);

        // Select endpoint using cumulative weight distribution
        let mut cumulative_weight = 0.0;
        for (index, endpoint) in endpoints.iter().enumerate() {
            cumulative_weight += endpoint.weight;
            if random_weight < cumulative_weight {
                tracing::debug!(
                    tier = ?target,
                    endpoint_name = %endpoint.name,
                    endpoint_index = index,
                    endpoint_weight = endpoint.weight,
                    total_endpoints = endpoints.len(),
                    total_weight = total_weight,
                    "Selected endpoint via weighted random selection"
                );
                return Some(endpoint);
            }
        }

        // Fallback to last endpoint (should never happen due to float precision)
        let last_endpoint = &endpoints[endpoints.len() - 1];
        tracing::debug!(
            tier = ?target,
            endpoint_name = %last_endpoint.name,
            "Selected last endpoint as fallback (floating point edge case)"
        );
        Some(last_endpoint)
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
    fn test_selector_weighted_fast_tier_both_endpoints_selectable() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // With equal weights (1.0 each), both endpoints should be selectable
        // Sample 100 times to verify both can be selected
        let mut fast1_seen = false;
        let mut fast2_seen = false;

        for _ in 0..100 {
            let selected = selector.select(TargetModel::Fast).unwrap();
            if selected.name == "fast-1" {
                fast1_seen = true;
            }
            if selected.name == "fast-2" {
                fast2_seen = true;
            }

            if fast1_seen && fast2_seen {
                break; // Both have been selected, test passes
            }
        }

        assert!(
            fast1_seen,
            "fast-1 should be selected at least once in 100 attempts"
        );
        assert!(
            fast2_seen,
            "fast-2 should be selected at least once in 100 attempts"
        );
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
    async fn test_selector_concurrent_weighted_selection() {
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

        // Verify all selections succeeded (concurrency safety)
        assert_eq!(
            results.len(),
            10,
            "all concurrent selections should complete"
        );
        for result in &results {
            assert!(result.is_some(), "all selections should succeed");
        }

        // Verify all selected endpoints are valid (from the configured endpoints)
        let endpoint_names: Vec<String> = results.into_iter().flatten().collect();
        for name in &endpoint_names {
            assert!(
                name == "fast-1" || name == "fast-2",
                "selected endpoint should be valid, got: {}",
                name
            );
        }

        // Note: With weighted random selection, we cannot deterministically assert
        // that both endpoints are always selected in just 10 draws. With equal weights,
        // there's ~0.2% chance all 10 selections hit the same endpoint.
        // This test focuses on concurrency safety, not distribution.
    }

    #[test]
    fn test_selector_zero_weight_fallback() {
        // Create config where all endpoints have zero weight
        let mut config = create_test_config();
        config.models.fast[0].weight = 0.0;
        config.models.fast[1].weight = 0.0;

        let selector = ModelSelector::new(Arc::new(config));

        // Should fall back to uniform random selection, not panic
        for _ in 0..10 {
            let result = selector.select(TargetModel::Fast);
            assert!(
                result.is_some(),
                "should return endpoint even with zero total weight"
            );

            let endpoint = result.unwrap();
            assert!(
                endpoint.name == "fast-1" || endpoint.name == "fast-2",
                "should select valid endpoint"
            );
        }
    }

    #[test]
    fn test_selector_negative_weight_fallback() {
        // Create config with negative weights (misconfiguration)
        let mut config = create_test_config();
        config.models.fast[0].weight = -1.0;
        config.models.fast[1].weight = -2.0;

        let selector = ModelSelector::new(Arc::new(config));

        // Should fall back to uniform random selection, not panic
        let result = selector.select(TargetModel::Fast);
        assert!(
            result.is_some(),
            "should return endpoint even with negative weights"
        );
    }

    #[test]
    fn test_weighted_selection_distribution() {
        // Create config with different weights: 2.0 vs 1.0 (2:1 ratio)
        let mut config = create_test_config();
        config.models.fast[0].weight = 2.0;
        config.models.fast[1].weight = 1.0;

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 3000 times to get statistically significant distribution
        let mut counts = std::collections::HashMap::new();
        for _ in 0..3000 {
            let endpoint = selector.select(TargetModel::Fast).unwrap();
            *counts.entry(endpoint.name.clone()).or_insert(0) += 1;
        }

        let fast1_count = counts.get("fast-1").unwrap_or(&0);
        let fast2_count = counts.get("fast-2").unwrap_or(&0);

        // With 2:1 weight ratio, expect ~2000:1000 distribution
        // Allow 10% deviation for randomness (1800-2200 for fast-1, 800-1200 for fast-2)
        assert!(
            *fast1_count >= 1800 && *fast1_count <= 2200,
            "fast-1 (weight 2.0) should get ~2000/3000 selections, got {}",
            fast1_count
        );
        assert!(
            *fast2_count >= 800 && *fast2_count <= 1200,
            "fast-2 (weight 1.0) should get ~1000/3000 selections, got {}",
            fast2_count
        );
    }

    #[test]
    fn test_weighted_selection_heavily_skewed() {
        // Create config with heavily skewed weights: 9.0 vs 1.0 (9:1 ratio)
        let mut config = create_test_config();
        config.models.fast[0].weight = 9.0;
        config.models.fast[1].weight = 1.0;

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 1000 times
        let mut counts = std::collections::HashMap::new();
        for _ in 0..1000 {
            let endpoint = selector.select(TargetModel::Fast).unwrap();
            *counts.entry(endpoint.name.clone()).or_insert(0) += 1;
        }

        let fast1_count = counts.get("fast-1").unwrap_or(&0);
        let fast2_count = counts.get("fast-2").unwrap_or(&0);

        // With 9:1 weight ratio, expect ~900:100 distribution
        // Allow 15% deviation (765-1035 for fast-1, 35-165 for fast-2)
        assert!(
            *fast1_count >= 765 && *fast1_count <= 1035,
            "fast-1 (weight 9.0) should get ~900/1000 selections, got {}",
            fast1_count
        );
        assert!(
            *fast2_count >= 35 && *fast2_count <= 165,
            "fast-2 (weight 1.0) should get ~100/1000 selections, got {}",
            fast2_count
        );
    }

    #[test]
    fn test_weighted_selection_all_equal_weights() {
        // When all weights are equal, should behave like uniform distribution
        let config = create_test_config(); // Both have weight 1.0

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 2000 times
        let mut counts = std::collections::HashMap::new();
        for _ in 0..2000 {
            let endpoint = selector.select(TargetModel::Fast).unwrap();
            *counts.entry(endpoint.name.clone()).or_insert(0) += 1;
        }

        let fast1_count = counts.get("fast-1").unwrap_or(&0);
        let fast2_count = counts.get("fast-2").unwrap_or(&0);

        // With equal weights, expect ~1000:1000 distribution
        // Allow 15% deviation for randomness (850-1150 for each)
        assert!(
            *fast1_count >= 850 && *fast1_count <= 1150,
            "fast-1 (weight 1.0) should get ~1000/2000 selections, got {}",
            fast1_count
        );
        assert!(
            *fast2_count >= 850 && *fast2_count <= 1150,
            "fast-2 (weight 1.0) should get ~1000/2000 selections, got {}",
            fast2_count
        );
    }

    #[test]
    fn test_weighted_selection_three_endpoints() {
        // Test with three endpoints with weights 3.0, 2.0, 1.0 (3:2:1 ratio)
        let mut config = create_test_config();
        config.models.fast = vec![
            ModelEndpoint {
                name: "fast-1".to_string(),
                base_url: "http://localhost:1234/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 3.0,
                priority: 1,
            },
            ModelEndpoint {
                name: "fast-2".to_string(),
                base_url: "http://localhost:1235/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 2.0,
                priority: 1,
            },
            ModelEndpoint {
                name: "fast-3".to_string(),
                base_url: "http://localhost:1236/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 1.0,
                priority: 1,
            },
        ];

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 6000 times (divisible by 6 for clean expected values)
        let mut counts = std::collections::HashMap::new();
        for _ in 0..6000 {
            let endpoint = selector.select(TargetModel::Fast).unwrap();
            *counts.entry(endpoint.name.clone()).or_insert(0) += 1;
        }

        let fast1_count = counts.get("fast-1").unwrap_or(&0);
        let fast2_count = counts.get("fast-2").unwrap_or(&0);
        let fast3_count = counts.get("fast-3").unwrap_or(&0);

        // Total weight = 6.0, so expect: fast-1: 3000, fast-2: 2000, fast-3: 1000
        // Allow 10% deviation
        assert!(
            *fast1_count >= 2700 && *fast1_count <= 3300,
            "fast-1 (weight 3.0) should get ~3000/6000 selections, got {}",
            fast1_count
        );
        assert!(
            *fast2_count >= 1800 && *fast2_count <= 2200,
            "fast-2 (weight 2.0) should get ~2000/6000 selections, got {}",
            fast2_count
        );
        assert!(
            *fast3_count >= 900 && *fast3_count <= 1100,
            "fast-3 (weight 1.0) should get ~1000/6000 selections, got {}",
            fast3_count
        );
    }
}
