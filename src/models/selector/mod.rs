//! Model selection logic for choosing from multiple endpoints
//!
//! Implements priority-based selection with weighted distribution and health checking.
//!
//! Production code is in this file, tests are organized in sibling modules:
//! - tests_basic: Basic selection, endpoint counting, empty tiers
//! - tests_priority: Priority-based filtering
//! - tests_weighted: Weighted random distribution
//! - tests_exclusion: Exclusion set handling for retry logic

mod balanced;

pub use balanced::TierSelector;

use crate::config::{Config, ModelEndpoint};
use crate::models::endpoint_name::{EndpointName, ExclusionSet};
use crate::models::health::HealthChecker;
use crate::router::TargetModel;
use rand::Rng;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Selects appropriate model endpoint from multi-model configuration
///
/// Implements priority-based selection with health filtering and weighted distribution:
/// - Filters out unhealthy endpoints
/// - Selects from highest available priority tier
/// - Uses weighted random selection within priority tier
#[derive(Debug)]
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
    /// Automatically creates and starts background health checking with metrics integration.
    ///
    /// # Arguments
    /// * `config` - Application configuration
    /// * `metrics` - Prometheus metrics for surfacing health tracking failures
    pub fn new(config: Arc<Config>, metrics: Arc<crate::metrics::Metrics>) -> Self {
        let health_checker = Arc::new(HealthChecker::new_with_metrics(config.clone(), metrics));

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
    /// Implements priority-based selection with health filtering, exclusion, and weighted distribution:
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
    /// # Performance
    /// - **Time complexity**: O(n) where n is the number of configured endpoints in the tier
    /// - **Space complexity**: O(n) for temporary endpoint vector during filtering
    /// - **Async**: Single RwLock read for health status (non-blocking if no writers)
    /// - **Expected latency**: <1ms for typical configurations (1-10 endpoints per tier)
    ///
    /// Returns None if the requested tier has no healthy, non-excluded endpoints available.
    pub async fn select(
        &self,
        target: TargetModel,
        exclude: &ExclusionSet,
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

        // Filter to only healthy and non-excluded endpoints
        let mut available_endpoints = Vec::new();
        for endpoint in endpoints.iter() {
            // Skip unhealthy endpoints
            if !self.health_checker.is_healthy(endpoint.name()).await {
                continue;
            }

            // Skip excluded endpoints (e.g., already failed in this request)
            if exclude.contains(&EndpointName::from(endpoint)) {
                tracing::debug!(
                    tier = ?target,
                    endpoint_name = %endpoint.name(),
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

        // Find highest priority among available endpoints and filter to only that tier
        let max_priority = available_endpoints
            .iter()
            .map(|e| e.priority())
            .max()
            .expect(
                "Defensive check: available_endpoints cannot be empty due to early return above",
            );

        let highest_priority_endpoints: Vec<&ModelEndpoint> = available_endpoints
            .iter()
            .filter(|e| e.priority() == max_priority)
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
        let total_weight: f64 = highest_priority_endpoints.iter().map(|e| e.weight()).sum();

        // Defensive check: Config validation guarantees all weights are positive.
        // This can only occur due to memory corruption.
        if total_weight <= 0.0 {
            tracing::error!(
                tier = ?target,
                priority = max_priority,
                total_weight = total_weight,
                endpoints_count = highest_priority_endpoints.len(),
                "MEMORY CORRUPTION DETECTED: All endpoints in priority tier {} have total weight {}. \
                Config validation prevents this at startup, so this indicates memory corruption. \
                Refusing to select endpoint - failing request to expose issue.",
                max_priority, total_weight
            );
            return None;
        }

        // Generate random number in range [0, total_weight)
        let mut rng = rand::thread_rng();
        let random_weight = rng.gen_range(0.0..total_weight);

        // Select endpoint using cumulative weight distribution within priority tier
        let mut cumulative_weight = 0.0;
        for (index, endpoint) in highest_priority_endpoints.iter().enumerate() {
            cumulative_weight += endpoint.weight();
            if random_weight < cumulative_weight {
                tracing::debug!(
                    tier = ?target,
                    priority = max_priority,
                    endpoint_name = %endpoint.name(),
                    endpoint_url = %endpoint.base_url(),
                    weight = endpoint.weight(),
                    index = index,
                    total_weight = total_weight,
                    "Selected endpoint via weighted random selection"
                );
                return Some(endpoint);
            }
        }

        // Fallback: return last endpoint if rounding errors prevent selection
        let last_endpoint = highest_priority_endpoints
            .last()
            .expect("Defensive check: highest_priority_endpoints cannot be empty");
        tracing::warn!(
            tier = ?target,
            priority = max_priority,
            endpoint_name = %last_endpoint.name(),
            "Fallback to last endpoint (likely floating-point rounding)"
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

    /// Get the default tier when no routing rule matches
    ///
    /// Selects the tier with the highest priority endpoint across ALL tiers
    /// (fast, balanced, deep). This is used as a fallback when rule-based routing
    /// returns None and LLM routing is not available.
    ///
    /// # Selection Logic
    /// 1. Find the maximum priority value across all configured endpoints in all tiers
    /// 2. Return the first tier (in order: Fast, Balanced, Deep) that has an endpoint with that priority
    ///
    /// # Returns
    /// Returns `Some(TargetModel)` with the tier of the highest-priority endpoint,
    /// or `None` if no endpoints are configured at all.
    ///
    /// # Example
    /// ```text
    /// Config:
    ///   Fast tier: priority 2
    ///   Balanced tier: (empty)
    ///   Deep tier: priority 3
    ///
    /// default_tier() returns Deep (priority 3 is highest)
    /// ```
    pub fn default_tier(&self) -> Option<TargetModel> {
        // Find max priority across all tiers
        let all_endpoints = self
            .config
            .models
            .fast
            .iter()
            .chain(self.config.models.balanced.iter())
            .chain(self.config.models.deep.iter());

        let max_priority = all_endpoints.map(|e| e.priority()).max()?;

        // Return first tier with that priority (check in order: Fast, Balanced, Deep)
        if self
            .config
            .models
            .fast
            .iter()
            .any(|e| e.priority() == max_priority)
        {
            return Some(TargetModel::Fast);
        }

        if self
            .config
            .models
            .balanced
            .iter()
            .any(|e| e.priority() == max_priority)
        {
            return Some(TargetModel::Balanced);
        }

        if self
            .config
            .models
            .deep
            .iter()
            .any(|e| e.priority() == max_priority)
        {
            return Some(TargetModel::Deep);
        }

        // Should never reach here if max_priority exists
        None
    }
}

// Test modules
#[cfg(test)]
mod tests_basic;
#[cfg(test)]
mod tests_exclusion;
#[cfg(test)]
mod tests_priority;
#[cfg(test)]
mod tests_weighted;

/// Shared test helper: Create standard test configuration
///
/// Configuration includes:
/// - Fast tier: 2 endpoints (fast-1, fast-2) with equal weight/priority
/// - Balanced tier: 1 endpoint (balanced-1)
/// - Deep tier: 1 endpoint (deep-1)
#[cfg(test)]
pub(crate) fn create_test_config() -> Config {
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

[[models.fast]]
name = "fast-2"
base_url = "http://localhost:1235/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1236/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1237/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_tier = "balanced"
"#;
    toml::from_str(toml).expect("should parse TOML config")
}
