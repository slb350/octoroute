//! Model selection logic for choosing from multiple endpoints
//!
//! Phase 2c: Priority + weighted selection with health checking

use crate::config::{Config, ModelEndpoint};
use crate::models::health::HealthChecker;
use crate::router::TargetModel;
use rand::Rng;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Selects appropriate model endpoint from multi-model configuration
///
/// Phase 2c: Priority-based selection with health filtering and weighted distribution.
/// - Filters out unhealthy endpoints
/// - Selects from highest available priority tier
/// - Uses weighted random selection within priority tier
pub struct ModelSelector {
    config: Arc<Config>,
    health_checker: Arc<HealthChecker>,
    // Selection counters for metrics tracking
    fast_counter: AtomicUsize,
    balanced_counter: AtomicUsize,
    deep_counter: AtomicUsize,
}

impl ModelSelector {
    /// Create a new ModelSelector from configuration
    ///
    /// Automatically creates and starts background health checking.
    pub fn new(config: Arc<Config>) -> Self {
        let health_checker = Arc::new(HealthChecker::new(config.clone()));

        // Start background health checking
        health_checker.clone().start_background_checks();

        Self {
            config,
            health_checker,
            fast_counter: AtomicUsize::new(0),
            balanced_counter: AtomicUsize::new(0),
            deep_counter: AtomicUsize::new(0),
        }
    }

    /// Get a reference to the health checker for external use (e.g., retry logic)
    pub fn health_checker(&self) -> &Arc<HealthChecker> {
        &self.health_checker
    }

    /// Select an endpoint for the given target model tier using priority + weighted random selection
    ///
    /// Phase 2c: Priority-based selection with health filtering, exclusion, and weighted distribution.
    /// - Filters out unhealthy endpoints first
    /// - Filters out endpoints in the exclusion set (for retry logic)
    /// - Filters to only the highest available priority tier
    /// - Within that priority tier, uses weighted random selection
    /// - Higher priority = tried first, higher weight = more traffic within priority tier
    ///
    /// # Arguments
    /// * `target` - The model tier to select from (Fast, Balanced, Deep)
    /// * `exclude` - Set of endpoint names to exclude from selection (e.g., endpoints that failed in current request)
    ///
    /// Returns None if the requested tier has no healthy, non-excluded endpoints available.
    pub async fn select(
        &self,
        target: TargetModel,
        exclude: &HashSet<String>,
    ) -> Option<&ModelEndpoint> {
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

        // Phase 2c: Filter to only healthy and non-excluded endpoints
        let mut available_endpoints = Vec::new();
        for endpoint in endpoints.iter() {
            // Skip unhealthy endpoints
            if !self.health_checker.is_healthy(&endpoint.name).await {
                continue;
            }

            // Skip excluded endpoints (e.g., already failed in this request)
            if exclude.contains(&endpoint.name) {
                tracing::debug!(
                    tier = ?target,
                    endpoint_name = %endpoint.name,
                    "Skipping excluded endpoint"
                );
                continue;
            }

            available_endpoints.push(endpoint);
        }

        if available_endpoints.is_empty() {
            tracing::error!(
                tier = ?target,
                total_endpoints = endpoints.len(),
                excluded_count = exclude.len(),
                "No available endpoints for tier - all endpoints either unhealthy or excluded"
            );
            return None;
        }

        tracing::debug!(
            tier = ?target,
            total_endpoints = endpoints.len(),
            excluded_count = exclude.len(),
            available_endpoints = available_endpoints.len(),
            "Filtered to healthy and non-excluded endpoints"
        );

        // Phase 2c: Find highest priority among available endpoints and filter to only that tier
        let max_priority = available_endpoints
            .iter()
            .map(|e| e.priority)
            .max()
            .unwrap(); // Safe because we already checked available_endpoints is not empty

        let highest_priority_endpoints: Vec<&ModelEndpoint> = available_endpoints
            .iter()
            .filter(|e| e.priority == max_priority)
            .copied()
            .collect();

        tracing::debug!(
            tier = ?target,
            max_priority = max_priority,
            available_endpoints = available_endpoints.len(),
            priority_tier_endpoints = highest_priority_endpoints.len(),
            "Filtered to highest priority tier among available endpoints"
        );

        // Increment selection counter for metrics (atomic operation)
        counter.fetch_add(1, Ordering::Relaxed);

        // Calculate total weight of endpoints in highest priority tier
        let total_weight: f64 = highest_priority_endpoints.iter().map(|e| e.weight).sum();

        // Handle zero or negative total weight (all endpoints disabled/misconfigured)
        if total_weight <= 0.0 {
            tracing::warn!(
                tier = ?target,
                priority = max_priority,
                total_weight = total_weight,
                endpoints_count = highest_priority_endpoints.len(),
                "All endpoints in priority tier have zero/negative weight, falling back to uniform selection"
            );

            // Fall back to uniform random selection within priority tier
            let mut rng = rand::thread_rng();
            let index = rng.gen_range(0..highest_priority_endpoints.len());
            let endpoint = highest_priority_endpoints[index];

            tracing::info!(
                tier = ?target,
                priority = max_priority,
                endpoint_name = %endpoint.name,
                endpoint_index = index,
                "Selected endpoint via uniform fallback (zero total weight)"
            );

            return Some(endpoint);
        }

        // Generate random number in range [0, total_weight)
        let mut rng = rand::thread_rng();
        let random_weight = rng.gen_range(0.0..total_weight);

        // Select endpoint using cumulative weight distribution within priority tier
        let mut cumulative_weight = 0.0;
        for (index, endpoint) in highest_priority_endpoints.iter().enumerate() {
            cumulative_weight += endpoint.weight;
            if random_weight < cumulative_weight {
                tracing::debug!(
                    tier = ?target,
                    priority = max_priority,
                    endpoint_name = %endpoint.name,
                    endpoint_index = index,
                    endpoint_priority = endpoint.priority,
                    endpoint_weight = endpoint.weight,
                    priority_tier_endpoints = highest_priority_endpoints.len(),
                    total_weight = total_weight,
                    "Selected endpoint via priority + weighted selection"
                );
                return Some(endpoint);
            }
        }

        // Fallback to last endpoint in priority tier (should never happen due to float precision)
        let last_endpoint = highest_priority_endpoints[highest_priority_endpoints.len() - 1];
        tracing::debug!(
            tier = ?target,
            priority = max_priority,
            endpoint_name = %last_endpoint.name,
            "Selected last endpoint in priority tier as fallback (floating point edge case)"
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

    #[tokio::test]
    async fn test_selector_new_creates_selector() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // Verify we can create a selector
        assert_eq!(selector.endpoint_count(TargetModel::Fast), 2);
        assert_eq!(selector.endpoint_count(TargetModel::Balanced), 1);
        assert_eq!(selector.endpoint_count(TargetModel::Deep), 1);
    }

    #[tokio::test]
    async fn test_selector_select_returns_endpoint() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // Should return some endpoint for each tier (no exclusions)
        let no_exclude = HashSet::new();
        assert!(
            selector
                .select(TargetModel::Fast, &no_exclude)
                .await
                .is_some()
        );
        assert!(
            selector
                .select(TargetModel::Balanced, &no_exclude)
                .await
                .is_some()
        );
        assert!(
            selector
                .select(TargetModel::Deep, &no_exclude)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn test_selector_weighted_fast_tier_both_endpoints_selectable() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // With equal weights (1.0 each), both endpoints should be selectable
        // Sample 100 times to verify both can be selected
        let mut fast1_seen = false;
        let mut fast2_seen = false;
        let no_exclude = HashSet::new();

        for _ in 0..100 {
            let selected = selector
                .select(TargetModel::Fast, &no_exclude)
                .await
                .unwrap();
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

    #[tokio::test]
    async fn test_selector_single_endpoint_tier() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // Balanced tier has only one endpoint, should return same one
        let no_exclude = HashSet::new();
        let first = selector
            .select(TargetModel::Balanced, &no_exclude)
            .await
            .unwrap();
        let second = selector
            .select(TargetModel::Balanced, &no_exclude)
            .await
            .unwrap();

        assert_eq!(first.name, "balanced-1");
        assert_eq!(second.name, "balanced-1");
    }

    #[tokio::test]
    async fn test_selector_endpoint_count() {
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        assert_eq!(selector.endpoint_count(TargetModel::Fast), 2);
        assert_eq!(selector.endpoint_count(TargetModel::Balanced), 1);
        assert_eq!(selector.endpoint_count(TargetModel::Deep), 1);
    }

    #[tokio::test]
    async fn test_selector_returns_none_for_empty_tier() {
        let mut config = create_test_config();
        config.models.fast = vec![]; // Empty tier
        let selector = ModelSelector::new(Arc::new(config));

        let no_exclude = HashSet::new();
        let result = selector.select(TargetModel::Fast, &no_exclude).await;
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
                let no_exclude = HashSet::new();
                sel.select(TargetModel::Fast, &no_exclude)
                    .await
                    .map(|e| e.name.clone())
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

    #[tokio::test]
    async fn test_selector_zero_weight_fallback() {
        // Create config where all endpoints have zero weight
        let mut config = create_test_config();
        config.models.fast[0].weight = 0.0;
        config.models.fast[1].weight = 0.0;

        let selector = ModelSelector::new(Arc::new(config));

        // Should fall back to uniform random selection, not panic
        let no_exclude = HashSet::new();
        for _ in 0..10 {
            let result = selector.select(TargetModel::Fast, &no_exclude).await;
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

    #[tokio::test]
    async fn test_selector_negative_weight_fallback() {
        // Create config with negative weights (misconfiguration)
        let mut config = create_test_config();
        config.models.fast[0].weight = -1.0;
        config.models.fast[1].weight = -2.0;

        let selector = ModelSelector::new(Arc::new(config));

        // Should fall back to uniform random selection, not panic
        let no_exclude = HashSet::new();
        let result = selector.select(TargetModel::Fast, &no_exclude).await;
        assert!(
            result.is_some(),
            "should return endpoint even with negative weights"
        );
    }

    #[tokio::test]
    async fn test_weighted_selection_distribution() {
        // Create config with different weights: 2.0 vs 1.0 (2:1 ratio)
        let mut config = create_test_config();
        config.models.fast[0].weight = 2.0;
        config.models.fast[1].weight = 1.0;

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 3000 times to get statistically significant distribution
        let no_exclude = HashSet::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..3000 {
            let endpoint = selector
                .select(TargetModel::Fast, &no_exclude)
                .await
                .unwrap();
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

    #[tokio::test]
    async fn test_weighted_selection_heavily_skewed() {
        // Create config with heavily skewed weights: 9.0 vs 1.0 (9:1 ratio)
        let mut config = create_test_config();
        config.models.fast[0].weight = 9.0;
        config.models.fast[1].weight = 1.0;

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 1000 times
        let no_exclude = HashSet::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..1000 {
            let endpoint = selector
                .select(TargetModel::Fast, &no_exclude)
                .await
                .unwrap();
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

    #[tokio::test]
    async fn test_weighted_selection_all_equal_weights() {
        // When all weights are equal, should behave like uniform distribution
        let config = create_test_config(); // Both have weight 1.0

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 2000 times
        let no_exclude = HashSet::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..2000 {
            let endpoint = selector
                .select(TargetModel::Fast, &no_exclude)
                .await
                .unwrap();
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

    #[tokio::test]
    async fn test_weighted_selection_three_endpoints() {
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
        let no_exclude = HashSet::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..6000 {
            let endpoint = selector
                .select(TargetModel::Fast, &no_exclude)
                .await
                .unwrap();
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

    // Phase 2c: Priority-based selection tests

    #[tokio::test]
    async fn test_priority_selection_highest_chosen() {
        // Config with three priority levels: 10, 5, 1
        // All endpoints healthy, should always select priority 10
        let mut config = create_test_config();
        config.models.fast = vec![
            ModelEndpoint {
                name: "fast-priority-10".to_string(),
                base_url: "http://localhost:1234/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 1.0,
                priority: 10,
            },
            ModelEndpoint {
                name: "fast-priority-5".to_string(),
                base_url: "http://localhost:1235/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 1.0,
                priority: 5,
            },
            ModelEndpoint {
                name: "fast-priority-1".to_string(),
                base_url: "http://localhost:1236/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 1.0,
                priority: 1,
            },
        ];

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 100 times - should ALWAYS select priority 10 endpoint
        let no_exclude = HashSet::new();
        for _ in 0..100 {
            let endpoint = selector
                .select(TargetModel::Fast, &no_exclude)
                .await
                .unwrap();
            assert_eq!(
                endpoint.name, "fast-priority-10",
                "Should always select highest priority (10) endpoint"
            );
        }
    }

    #[tokio::test]
    async fn test_priority_with_weighted_distribution() {
        // Config: Two priority 5 endpoints with 2:1 weight ratio, one priority 1
        // Should only select from priority 5 tier with weighted distribution
        let mut config = create_test_config();
        config.models.fast = vec![
            ModelEndpoint {
                name: "fast-priority-5-heavy".to_string(),
                base_url: "http://localhost:1234/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 2.0,
                priority: 5,
            },
            ModelEndpoint {
                name: "fast-priority-5-light".to_string(),
                base_url: "http://localhost:1235/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 1.0,
                priority: 5,
            },
            ModelEndpoint {
                name: "fast-priority-1".to_string(),
                base_url: "http://localhost:1236/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 10.0, // High weight but lower priority - should never be selected
                priority: 1,
            },
        ];

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 3000 times
        let no_exclude = HashSet::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..3000 {
            let endpoint = selector
                .select(TargetModel::Fast, &no_exclude)
                .await
                .unwrap();
            *counts.entry(endpoint.name.clone()).or_insert(0) += 1;
        }

        let heavy_count = counts.get("fast-priority-5-heavy").unwrap_or(&0);
        let light_count = counts.get("fast-priority-5-light").unwrap_or(&0);
        let low_priority_count = counts.get("fast-priority-1").unwrap_or(&0);

        // Priority 1 should NEVER be selected (priority 5 available)
        assert_eq!(
            *low_priority_count, 0,
            "Lower priority endpoint should never be selected when higher priority available"
        );

        // Within priority 5 tier: expect ~2000:1000 distribution (2:1 ratio)
        // Allow 10% deviation
        assert!(
            *heavy_count >= 1800 && *heavy_count <= 2200,
            "Priority 5 heavy (weight 2.0) should get ~2000/3000 selections, got {}",
            heavy_count
        );
        assert!(
            *light_count >= 800 && *light_count <= 1200,
            "Priority 5 light (weight 1.0) should get ~1000/3000 selections, got {}",
            light_count
        );
    }

    #[tokio::test]
    async fn test_priority_all_same_uses_weighted() {
        // When all endpoints have same priority, should use weighted selection
        let mut config = create_test_config();
        config.models.fast = vec![
            ModelEndpoint {
                name: "fast-heavy".to_string(),
                base_url: "http://localhost:1234/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 3.0,
                priority: 1,
            },
            ModelEndpoint {
                name: "fast-light".to_string(),
                base_url: "http://localhost:1235/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 1.0,
                priority: 1,
            },
        ];

        let selector = ModelSelector::new(Arc::new(config));

        // Sample 4000 times
        let no_exclude = HashSet::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..4000 {
            let endpoint = selector
                .select(TargetModel::Fast, &no_exclude)
                .await
                .unwrap();
            *counts.entry(endpoint.name.clone()).or_insert(0) += 1;
        }

        let heavy_count = counts.get("fast-heavy").unwrap_or(&0);
        let light_count = counts.get("fast-light").unwrap_or(&0);

        // With 3:1 weight ratio, expect ~3000:1000 distribution
        // Allow 10% deviation
        assert!(
            *heavy_count >= 2700 && *heavy_count <= 3300,
            "Heavy weight (3.0) should get ~3000/4000 selections, got {}",
            heavy_count
        );
        assert!(
            *light_count >= 700 && *light_count <= 1300,
            "Light weight (1.0) should get ~1000/4000 selections, got {}",
            light_count
        );
    }

    // Exclusion tests

    #[tokio::test]
    async fn test_exclusion_filters_endpoints() {
        // Test that excluded endpoints are not selected
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // Exclude fast-1, should only select fast-2
        let mut exclude = HashSet::new();
        exclude.insert("fast-1".to_string());

        // Sample 100 times - should NEVER select fast-1
        for _ in 0..100 {
            let endpoint = selector.select(TargetModel::Fast, &exclude).await.unwrap();
            assert_eq!(
                endpoint.name, "fast-2",
                "Should only select fast-2 when fast-1 is excluded"
            );
        }
    }

    #[tokio::test]
    async fn test_exclusion_all_endpoints_returns_none() {
        // Test that excluding all endpoints returns None
        let config = Arc::new(create_test_config());
        let selector = ModelSelector::new(config);

        // Exclude both fast endpoints
        let mut exclude = HashSet::new();
        exclude.insert("fast-1".to_string());
        exclude.insert("fast-2".to_string());

        let result = selector.select(TargetModel::Fast, &exclude).await;
        assert!(
            result.is_none(),
            "Should return None when all endpoints are excluded"
        );
    }

    #[tokio::test]
    async fn test_exclusion_preserves_priority_and_weight() {
        // Test that exclusion works with priority and weighted selection
        let mut config = create_test_config();
        config.models.fast = vec![
            ModelEndpoint {
                name: "fast-priority-10-heavy".to_string(),
                base_url: "http://localhost:1234/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 3.0,
                priority: 10,
            },
            ModelEndpoint {
                name: "fast-priority-10-light".to_string(),
                base_url: "http://localhost:1235/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 1.0,
                priority: 10,
            },
            ModelEndpoint {
                name: "fast-priority-5".to_string(),
                base_url: "http://localhost:1236/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 10.0, // High weight but lower priority
                priority: 5,
            },
        ];

        let selector = ModelSelector::new(Arc::new(config));

        // Exclude the heavy priority-10 endpoint
        let mut exclude = HashSet::new();
        exclude.insert("fast-priority-10-heavy".to_string());

        // Should only select fast-priority-10-light (same priority tier, not excluded)
        // Should NEVER select fast-priority-5 (lower priority, even though not excluded)
        for _ in 0..100 {
            let endpoint = selector.select(TargetModel::Fast, &exclude).await.unwrap();
            assert_eq!(
                endpoint.name, "fast-priority-10-light",
                "Should select remaining priority-10 endpoint, not lower priority"
            );
        }

        // Now exclude both priority-10 endpoints
        exclude.insert("fast-priority-10-light".to_string());

        // Now should fall back to priority-5
        for _ in 0..100 {
            let endpoint = selector.select(TargetModel::Fast, &exclude).await.unwrap();
            assert_eq!(
                endpoint.name, "fast-priority-5",
                "Should fall back to lower priority when higher priority excluded"
            );
        }
    }
}
