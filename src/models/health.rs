//! Health checking for model endpoints
//!
//! Provides periodic health checks for model endpoints with state tracking.
//! Endpoints that fail consecutive checks are marked unhealthy and excluded from selection.

use crate::config::{Config, ModelEndpoint};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::RwLock;

/// Errors that can occur during health checking operations
#[derive(Error, Debug)]
pub enum HealthError {
    #[error("Unknown endpoint: {0}")]
    UnknownEndpoint(String),
}

/// Health status for a single endpoint
///
/// Encapsulates health state to prevent invalid state transitions.
/// All fields are private to ensure state invariants are maintained.
#[derive(Clone, Debug)]
pub struct EndpointHealth {
    name: String,
    base_url: String,
    healthy: bool,
    last_check: Instant,
    consecutive_failures: u32,
}

impl EndpointHealth {
    /// Create a new EndpointHealth starting in healthy state
    pub fn new(name: String, base_url: String) -> Self {
        Self {
            name,
            base_url,
            healthy: true,
            last_check: Instant::now(),
            consecutive_failures: 0,
        }
    }

    /// Get the endpoint name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the endpoint base URL
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Check if the endpoint is currently healthy
    pub fn is_healthy(&self) -> bool {
        self.healthy
    }

    /// Get the last health check time
    pub fn last_check(&self) -> Instant {
        self.last_check
    }

    /// Get the consecutive failure count
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

/// Health checker for model endpoints
///
/// Tracks health status of all endpoints and provides background checking.
/// - 3 consecutive failures → mark unhealthy
/// - 1 successful check → recover to healthy
pub struct HealthChecker {
    health_status: Arc<RwLock<HashMap<String, EndpointHealth>>>,
    config: Arc<Config>,
}

impl HealthChecker {
    /// Create a new HealthChecker with all endpoints starting as healthy
    pub fn new(config: Arc<Config>) -> Self {
        let mut health_status = HashMap::new();

        // Initialize all fast endpoints
        for endpoint in &config.models.fast {
            health_status.insert(
                endpoint.name.clone(),
                EndpointHealth::new(endpoint.name.clone(), endpoint.base_url.clone()),
            );
        }

        // Initialize all balanced endpoints
        for endpoint in &config.models.balanced {
            health_status.insert(
                endpoint.name.clone(),
                EndpointHealth::new(endpoint.name.clone(), endpoint.base_url.clone()),
            );
        }

        // Initialize all deep endpoints
        for endpoint in &config.models.deep {
            health_status.insert(
                endpoint.name.clone(),
                EndpointHealth::new(endpoint.name.clone(), endpoint.base_url.clone()),
            );
        }

        tracing::info!(
            total_endpoints = health_status.len(),
            "HealthChecker initialized with all endpoints starting as healthy"
        );

        Self {
            health_status: Arc::new(RwLock::new(health_status)),
            config,
        }
    }

    /// Check if an endpoint is currently healthy
    pub async fn is_healthy(&self, endpoint_name: &str) -> bool {
        let status = self.health_status.read().await;
        status
            .get(endpoint_name)
            .map(|h| h.healthy)
            .unwrap_or(false) // Unknown endpoints are considered unhealthy
    }

    /// Mark an endpoint as having failed
    ///
    /// Increments consecutive failure count.
    /// After 3 consecutive failures, marks endpoint as unhealthy.
    ///
    /// Returns an error if the endpoint name is unknown.
    pub async fn mark_failure(&self, endpoint_name: &str) -> Result<(), HealthError> {
        let mut status = self.health_status.write().await;

        // Check if endpoint exists before mutably borrowing
        if !status.contains_key(endpoint_name) {
            let available: Vec<_> = status.keys().collect();
            tracing::error!(
                endpoint_name = %endpoint_name,
                available_endpoints = ?available,
                "Unknown endpoint in mark_failure - this is a BUG"
            );
            return Err(HealthError::UnknownEndpoint(endpoint_name.to_string()));
        }

        let health = status.get_mut(endpoint_name).unwrap();

        health.consecutive_failures += 1;
        health.last_check = Instant::now();

        // After 3 consecutive failures, mark as unhealthy
        if health.consecutive_failures >= 3 {
            if health.healthy {
                // Log only on transition to unhealthy
                tracing::warn!(
                    endpoint_name = %health.name,
                    consecutive_failures = health.consecutive_failures,
                    "Endpoint marked as unhealthy after 3 consecutive failures"
                );
            }
            health.healthy = false;
        } else {
            tracing::debug!(
                endpoint_name = %health.name,
                consecutive_failures = health.consecutive_failures,
                "Endpoint failure recorded (still healthy)"
            );
        }

        Ok(())
    }

    /// Mark an endpoint as having succeeded
    ///
    /// Resets consecutive failure count and marks endpoint as healthy.
    ///
    /// Returns an error if the endpoint name is unknown.
    pub async fn mark_success(&self, endpoint_name: &str) -> Result<(), HealthError> {
        let mut status = self.health_status.write().await;

        // Check if endpoint exists before mutably borrowing
        if !status.contains_key(endpoint_name) {
            let available: Vec<_> = status.keys().collect();
            tracing::error!(
                endpoint_name = %endpoint_name,
                available_endpoints = ?available,
                "Unknown endpoint in mark_success - this is a BUG"
            );
            return Err(HealthError::UnknownEndpoint(endpoint_name.to_string()));
        }

        let health = status.get_mut(endpoint_name).unwrap();

        let was_unhealthy = !health.healthy;

        health.consecutive_failures = 0;
        health.healthy = true;
        health.last_check = Instant::now();

        if was_unhealthy {
            // Log recovery
            tracing::info!(
                endpoint_name = %health.name,
                "Endpoint recovered to healthy state"
            );
        } else {
            tracing::debug!(
                endpoint_name = %health.name,
                "Endpoint health check succeeded"
            );
        }

        Ok(())
    }

    /// Get all health statuses for display/debugging
    pub async fn get_all_statuses(&self) -> Vec<EndpointHealth> {
        let status = self.health_status.read().await;
        status.values().cloned().collect()
    }

    /// Check a single endpoint's health via HTTP HEAD request
    async fn check_endpoint(&self, endpoint: &ModelEndpoint) -> bool {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    endpoint_name = %endpoint.name,
                    endpoint_url = %endpoint.base_url,
                    error = %e,
                    error_debug = ?e,
                    "CRITICAL: Failed to create HTTP client for health check. \
                    This indicates a systemic issue (TLS config, resource exhaustion), \
                    not just an unhealthy endpoint. Error: {:?}",
                    e
                );
                return false;
            }
        };

        // CRITICAL: base_url already includes /v1 (e.g., "http://host:port/v1")
        // We append "/models" to get "http://host:port/v1/models"
        // DO NOT append "/v1/models" - that would create "http://host:port/v1/v1/models" (404!)
        // This bug caused all endpoints to fail health checks after 90 seconds.
        let url = format!("{}/models", endpoint.base_url);

        match client.head(&url).send().await {
            Ok(response) => {
                let is_success = response.status().is_success();
                tracing::debug!(
                    endpoint_name = %endpoint.name,
                    url = %url,
                    status = %response.status(),
                    healthy = is_success,
                    "Health check completed"
                );
                is_success
            }
            Err(e) => {
                tracing::debug!(
                    endpoint_name = %endpoint.name,
                    url = %url,
                    error = %e,
                    "Health check failed"
                );
                false
            }
        }
    }

    /// Run health checks on all endpoints once
    async fn run_health_checks(&self) {
        let endpoints: Vec<ModelEndpoint> = {
            let config = &self.config;
            let mut all = Vec::new();
            all.extend(config.models.fast.clone());
            all.extend(config.models.balanced.clone());
            all.extend(config.models.deep.clone());
            all
        };

        for endpoint in endpoints {
            let is_healthy = self.check_endpoint(&endpoint).await;

            let result = if is_healthy {
                self.mark_success(&endpoint.name).await
            } else {
                self.mark_failure(&endpoint.name).await
            };

            if let Err(e) = result {
                tracing::error!(
                    endpoint_name = %endpoint.name,
                    error = %e,
                    "Failed to update health status - this should never happen"
                );
            }
        }
    }

    /// Start background health checking task
    ///
    /// Spawns a tokio task that runs health checks every 30 seconds.
    /// Includes automatic restart logic with exponential backoff (max 5 attempts).
    /// If the task fails repeatedly, health monitoring stops and endpoints will not recover.
    pub fn start_background_checks(self: Arc<Self>) {
        const MAX_RESTART_ATTEMPTS: u32 = 5;

        tokio::spawn(async move {
            let mut restart_count = 0;

            loop {
                let checker = Arc::clone(&self);
                let handle = tokio::spawn(async move {
                    tracing::info!(
                        attempt = restart_count + 1,
                        "Starting background health checks (30s interval)"
                    );

                    loop {
                        tokio::time::sleep(Duration::from_secs(30)).await;

                        tracing::debug!("Running scheduled health checks");
                        checker.run_health_checks().await;
                    }
                });

                // Monitor the health check task to detect failures
                match handle.await {
                    Ok(_) => {
                        // Task terminated normally (shouldn't happen - it's an infinite loop)
                        tracing::error!(
                            restart_count = restart_count,
                            "Background health check task terminated unexpectedly"
                        );
                    }
                    Err(e) => {
                        // Task panicked
                        tracing::error!(
                            error = %e,
                            restart_count = restart_count,
                            "Background health check task panicked"
                        );
                    }
                }

                restart_count += 1;

                if restart_count >= MAX_RESTART_ATTEMPTS {
                    tracing::error!(
                        max_attempts = MAX_RESTART_ATTEMPTS,
                        "Background health check task failed {} times. \
                        Health monitoring has stopped permanently. Endpoints marked \
                        unhealthy will remain unhealthy until server restart. \
                        This indicates a critical bug in the health check logic.",
                        MAX_RESTART_ATTEMPTS
                    );
                    break;
                }

                // Exponential backoff: 1s, 2s, 4s, 8s, 16s
                let backoff_seconds = 2_u64.pow(restart_count - 1);
                tracing::warn!(
                    restart_count = restart_count,
                    backoff_seconds = backoff_seconds,
                    max_attempts = MAX_RESTART_ATTEMPTS,
                    "Restarting background health check task after {}s backoff",
                    backoff_seconds
                );
                tokio::time::sleep(Duration::from_secs(backoff_seconds)).await;
            }
        });
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
    async fn test_health_checker_new_initializes_all_healthy() {
        let config = Arc::new(create_test_config());
        let checker = HealthChecker::new(config);

        // All endpoints should start as healthy
        assert!(checker.is_healthy("fast-1").await);
        assert!(checker.is_healthy("fast-2").await);
        assert!(checker.is_healthy("balanced-1").await);
        assert!(checker.is_healthy("deep-1").await);
    }

    #[tokio::test]
    async fn test_health_checker_unknown_endpoint_is_unhealthy() {
        let config = Arc::new(create_test_config());
        let checker = HealthChecker::new(config);

        // Unknown endpoint should be considered unhealthy
        assert!(!checker.is_healthy("unknown-endpoint").await);
    }

    #[tokio::test]
    async fn test_health_checker_mark_failure_tracks_consecutive() {
        let config = Arc::new(create_test_config());
        let checker = HealthChecker::new(config);

        // Should still be healthy after 1-2 failures
        checker.mark_failure("fast-1").await.unwrap();
        assert!(checker.is_healthy("fast-1").await);

        checker.mark_failure("fast-1").await.unwrap();
        assert!(checker.is_healthy("fast-1").await);

        // After 3rd consecutive failure, should be unhealthy
        checker.mark_failure("fast-1").await.unwrap();
        assert!(!checker.is_healthy("fast-1").await);
    }

    #[tokio::test]
    async fn test_health_checker_mark_success_recovers() {
        let config = Arc::new(create_test_config());
        let checker = HealthChecker::new(config);

        // Mark unhealthy with 3 failures
        checker.mark_failure("fast-1").await.unwrap();
        checker.mark_failure("fast-1").await.unwrap();
        checker.mark_failure("fast-1").await.unwrap();
        assert!(!checker.is_healthy("fast-1").await);

        // One success should recover
        checker.mark_success("fast-1").await.unwrap();
        assert!(checker.is_healthy("fast-1").await);

        // Consecutive failure count should be reset
        let statuses = checker.get_all_statuses().await;
        let fast1_status = statuses.iter().find(|s| s.name == "fast-1").unwrap();
        assert_eq!(fast1_status.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn test_health_checker_get_all_statuses_returns_all_endpoints() {
        let config = Arc::new(create_test_config());
        let checker = HealthChecker::new(config);

        let statuses = checker.get_all_statuses().await;

        // Should have 4 endpoints total (2 fast + 1 balanced + 1 deep)
        assert_eq!(statuses.len(), 4);

        // Verify all endpoint names present
        let names: Vec<String> = statuses.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"fast-1".to_string()));
        assert!(names.contains(&"fast-2".to_string()));
        assert!(names.contains(&"balanced-1".to_string()));
        assert!(names.contains(&"deep-1".to_string()));
    }

    #[tokio::test]
    async fn test_health_checker_success_resets_partial_failures() {
        let config = Arc::new(create_test_config());
        let checker = HealthChecker::new(config);

        // 2 failures (not enough to mark unhealthy)
        checker.mark_failure("fast-1").await.unwrap();
        checker.mark_failure("fast-1").await.unwrap();

        // Success should reset counter
        checker.mark_success("fast-1").await.unwrap();

        // Should still be healthy and counter reset
        assert!(checker.is_healthy("fast-1").await);

        // Verify counter is actually reset by checking we need 3 more failures
        checker.mark_failure("fast-1").await.unwrap();
        checker.mark_failure("fast-1").await.unwrap();
        assert!(checker.is_healthy("fast-1").await); // Still healthy after 2

        checker.mark_failure("fast-1").await.unwrap();
        assert!(!checker.is_healthy("fast-1").await); // Now unhealthy after 3rd
    }
}
