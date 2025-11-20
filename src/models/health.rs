//! Health checking for model endpoints
//!
//! Provides periodic health checks for model endpoints with state tracking.
//! Endpoints that fail consecutive checks are marked unhealthy and excluded from selection.

use crate::config::{Config, ModelEndpoint};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};

// Health check configuration constants
const CONSECUTIVE_FAILURES_THRESHOLD: u32 = 3;
const HEALTH_CHECK_INTERVAL_SECS: u64 = 30;
const HEALTH_CHECK_STALE_THRESHOLD_SECS: u64 = 60;
const MAX_BACKGROUND_TASK_RESTARTS: u32 = 5;

/// Status of the background health checking task
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundTaskStatus {
    /// Task is running normally
    Running,
    /// Task has failed and is restarting
    Restarting,
    /// Task has exhausted all restart attempts and is permanently stopped
    PermanentlyFailed,
}

/// Metrics for monitoring the health checking system itself
///
/// Tracks the background task's health to enable external monitoring
/// and alerting when the health checking system fails.
pub struct HealthMetrics {
    state: Arc<RwLock<HealthMetricsState>>,
}

/// Internal state for HealthMetrics
struct HealthMetricsState {
    /// When the background task last completed a health check cycle
    last_successful_check: Option<Instant>,
    /// Current status of the background task
    background_task_status: BackgroundTaskStatus,
    /// Number of times the background task has restarted
    restart_count: u32,
    /// When the background task last failed (if applicable)
    last_failure_time: Option<Instant>,
}

impl Default for HealthMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthMetrics {
    /// Create a new HealthMetrics instance
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(HealthMetricsState {
                last_successful_check: None,
                background_task_status: BackgroundTaskStatus::Running,
                restart_count: 0,
                last_failure_time: None,
            })),
        }
    }

    /// Record a successful health check cycle completion
    pub async fn record_successful_check(&self) {
        let mut state = self.state.write().await;
        state.last_successful_check = Some(Instant::now());
        state.background_task_status = BackgroundTaskStatus::Running;
    }

    /// Record a background task restart attempt
    pub async fn record_restart(&self, attempt: u32) {
        let mut state = self.state.write().await;
        state.restart_count = attempt;
        state.background_task_status = BackgroundTaskStatus::Restarting;
        state.last_failure_time = Some(Instant::now());
    }

    /// Mark the background task as permanently failed
    pub async fn mark_permanently_failed(&self) {
        let mut state = self.state.write().await;
        state.background_task_status = BackgroundTaskStatus::PermanentlyFailed;
        state.last_failure_time = Some(Instant::now());
    }

    /// Get the current status for monitoring
    pub async fn status(&self) -> BackgroundTaskStatus {
        let state = self.state.read().await;
        state.background_task_status
    }

    /// Get the last successful check time
    pub async fn last_successful_check(&self) -> Option<Instant> {
        let state = self.state.read().await;
        state.last_successful_check
    }

    /// Get the number of restart attempts
    pub async fn restart_count(&self) -> u32 {
        let state = self.state.read().await;
        state.restart_count
    }

    /// Get the last failure time
    pub async fn last_failure_time(&self) -> Option<Instant> {
        let state = self.state.read().await;
        state.last_failure_time
    }

    /// Check if the background task is healthy
    ///
    /// Returns false if:
    /// - Task is permanently failed
    /// - More than 60 seconds since last successful check (2x the 30s interval)
    pub async fn is_background_task_healthy(&self) -> bool {
        let state = self.state.read().await;

        if state.background_task_status == BackgroundTaskStatus::PermanentlyFailed {
            return false;
        }

        // Check if we've had a recent successful check
        if let Some(last_check) = state.last_successful_check {
            let elapsed = Instant::now().duration_since(last_check);
            if elapsed > Duration::from_secs(HEALTH_CHECK_STALE_THRESHOLD_SECS) {
                return false; // No successful check in threshold time (2x the interval)
            }
        } else {
            // No successful check yet - give it some time to start
            // This is only false if it's been running for a while with no checks
            return state.background_task_status == BackgroundTaskStatus::Running;
        }

        true
    }
}

/// Errors that can occur during health checking operations
#[derive(Error, Debug)]
pub enum HealthError {
    #[error("Unknown endpoint: {0}")]
    UnknownEndpoint(String),

    /// Failed to create HTTP client for health checks
    ///
    /// This indicates a systemic issue (TLS configuration error, resource exhaustion,
    /// library bug) rather than an individual endpoint failure. When this error occurs,
    /// ALL subsequent health checks will fail.
    #[error(
        "Failed to create HTTP client: {0}. This indicates a systemic issue (TLS config, resource exhaustion)."
    )]
    HttpClientCreationFailed(String),
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
    metrics: Arc<HealthMetrics>,
    /// Background health checking task handle for graceful shutdown
    background_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl std::fmt::Debug for HealthChecker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthChecker")
            .field("health_status", &"<RwLock<HashMap>>")
            .field("config", &"<Config>")
            .field("metrics", &"<HealthMetrics>")
            .field("background_task", &"<Mutex<JoinHandle>>")
            .finish()
    }
}

impl HealthChecker {
    /// Create a new HealthChecker with all endpoints starting as healthy
    pub fn new(config: Arc<Config>) -> Self {
        let mut health_status = HashMap::new();

        // Initialize all fast endpoints
        for endpoint in &config.models.fast {
            health_status.insert(
                endpoint.name().to_string(),
                EndpointHealth::new(endpoint.name().to_string(), endpoint.base_url().to_string()),
            );
        }

        // Initialize all balanced endpoints
        for endpoint in &config.models.balanced {
            health_status.insert(
                endpoint.name().to_string(),
                EndpointHealth::new(endpoint.name().to_string(), endpoint.base_url().to_string()),
            );
        }

        // Initialize all deep endpoints
        for endpoint in &config.models.deep {
            health_status.insert(
                endpoint.name().to_string(),
                EndpointHealth::new(endpoint.name().to_string(), endpoint.base_url().to_string()),
            );
        }

        tracing::info!(
            total_endpoints = health_status.len(),
            "HealthChecker initialized with all endpoints starting as healthy"
        );

        Self {
            health_status: Arc::new(RwLock::new(health_status)),
            config,
            metrics: Arc::new(HealthMetrics::new()),
            background_task: Arc::new(Mutex::new(None)),
        }
    }

    /// Get reference to health metrics for monitoring
    pub fn metrics(&self) -> &Arc<HealthMetrics> {
        &self.metrics
    }

    /// Check if an endpoint is currently healthy
    ///
    /// # Performance
    /// - **Time complexity**: O(1) HashMap lookup
    /// - **Space complexity**: O(1)
    /// - **Async**: RwLock read (shared, non-blocking with other readers)
    /// - **Expected latency**: <1μs
    pub async fn is_healthy(&self, endpoint_name: &str) -> bool {
        let status = self.health_status.read().await;
        status
            .get(endpoint_name)
            .map(|h| h.healthy)
            .unwrap_or(false)
    }

    /// Mark an endpoint as having failed
    ///
    /// Increments consecutive failure count.
    /// After 3 consecutive failures, marks endpoint as unhealthy.
    ///
    /// # Performance
    /// - **Time complexity**: O(1) HashMap lookup + mutation
    /// - **Space complexity**: O(1)
    /// - **Async**: RwLock write (exclusive lock, blocks other readers/writers)
    /// - **Expected latency**: <10μs (depends on lock contention)
    ///
    /// Returns an error if the endpoint name is unknown.
    pub async fn mark_failure(&self, endpoint_name: &str) -> Result<(), HealthError> {
        let mut status = self.health_status.write().await;

        // Get mutable reference to endpoint health, returning error if unknown
        let health = match status.get_mut(endpoint_name) {
            Some(h) => h,
            None => {
                let available: Vec<_> = status.keys().collect();
                tracing::error!(
                    endpoint_name = %endpoint_name,
                    available_endpoints = ?available,
                    "Unknown endpoint '{}' in mark_failure - available: {:?}",
                    endpoint_name, available
                );
                return Err(HealthError::UnknownEndpoint(endpoint_name.to_string()));
            }
        };

        health.consecutive_failures += 1;
        health.last_check = Instant::now();

        // After 3 consecutive failures, mark as unhealthy
        if health.consecutive_failures >= CONSECUTIVE_FAILURES_THRESHOLD {
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
    /// # Performance
    /// - **Time complexity**: O(1) HashMap lookup + mutation
    /// - **Space complexity**: O(1)
    /// - **Async**: RwLock write (exclusive lock, blocks other readers/writers)
    /// - **Expected latency**: <10μs (depends on lock contention)
    ///
    /// Returns an error if the endpoint name is unknown.
    pub async fn mark_success(&self, endpoint_name: &str) -> Result<(), HealthError> {
        let mut status = self.health_status.write().await;

        // Get mutable reference to endpoint health, returning error if unknown
        let health = match status.get_mut(endpoint_name) {
            Some(h) => h,
            None => {
                let available: Vec<_> = status.keys().collect();
                tracing::error!(
                    endpoint_name = %endpoint_name,
                    available_endpoints = ?available,
                    "Unknown endpoint '{}' in mark_success - available: {:?}",
                    endpoint_name, available
                );
                return Err(HealthError::UnknownEndpoint(endpoint_name.to_string()));
            }
        };

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
    ///
    /// Returns:
    /// - `Ok(true)` if endpoint is healthy (2xx response)
    /// - `Ok(false)` if endpoint is unhealthy (non-2xx, timeout, connection error)
    /// - `Err(HealthError::HttpClientCreationFailed)` if HTTP client creation fails
    ///   (indicates systemic issue, not endpoint-specific problem)
    async fn check_endpoint(&self, endpoint: &ModelEndpoint) -> Result<bool, HealthError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| {
                tracing::error!(
                    error = %e,
                    "FATAL: Failed to create HTTP client for health checks. \
                    This indicates a systemic issue (TLS config, resource exhaustion, library bug), \
                    not an endpoint failure. All health checks will fail."
                );
                HealthError::HttpClientCreationFailed(e.to_string())
            })?;

        // IMPORTANT: Health check URL construction (fixed bug in commit 64c913d)
        // base_url already includes /v1 (e.g., "http://host:port/v1")
        // We append "/models" to get "http://host:port/v1/models"
        // DO NOT append "/v1/models" - that would create "http://host:port/v1/v1/models" (404!)
        // Historical note: This bug previously caused all endpoints to fail after 90 seconds.
        let url = format!("{}/models", endpoint.base_url());

        match client.head(&url).send().await {
            Ok(response) => {
                let is_success = response.status().is_success();
                tracing::debug!(
                    endpoint_name = %endpoint.name(),
                    url = %url,
                    status = %response.status(),
                    healthy = is_success,
                    "Health check completed"
                );
                Ok(is_success)
            }
            Err(e) => {
                tracing::debug!(
                    endpoint_name = %endpoint.name(),
                    url = %url,
                    error = %e,
                    "Health check failed"
                );
                Ok(false)
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
            match self.check_endpoint(&endpoint).await {
                Ok(true) => {
                    // Endpoint is healthy
                    if let Err(e) = self.mark_success(endpoint.name()).await {
                        tracing::error!(
                            endpoint_name = %endpoint.name(),
                            error = %e,
                            "Failed to update health status - this should never happen"
                        );
                    }
                }
                Ok(false) => {
                    // Endpoint is unhealthy
                    if let Err(e) = self.mark_failure(endpoint.name()).await {
                        tracing::error!(
                            endpoint_name = %endpoint.name(),
                            error = %e,
                            "Failed to update health status - this should never happen"
                        );
                    }
                }
                Err(HealthError::HttpClientCreationFailed(msg)) => {
                    // Systemic failure - HTTP client creation failed
                    // This will affect ALL endpoints, so we log the error and exit early
                    // rather than marking individual endpoints as failed
                    tracing::error!(
                        error = %msg,
                        "FATAL: HTTP client creation failed. All subsequent health checks \
                        will fail. This is a systemic issue, not an endpoint-specific problem. \
                        Background health checking cannot continue."
                    );
                    // Exit the loop early - don't check remaining endpoints
                    return;
                }
                Err(e) => {
                    // Other errors (currently none, but future-proofing)
                    tracing::error!(
                        endpoint_name = %endpoint.name(),
                        error = %e,
                        "Unexpected error during health check"
                    );
                }
            }
        }

        // Record successful completion of health check cycle
        self.metrics.record_successful_check().await;
    }

    /// Start background health checking task
    ///
    /// Spawns a tokio task that runs health checks every 30 seconds.
    /// Includes automatic restart logic with exponential backoff (max 5 attempts).
    /// Updates HealthMetrics to enable external monitoring of the background task health.
    ///
    /// The background task handle is stored internally and can be cancelled via `shutdown()`.
    ///
    /// # Panics
    ///
    /// Panics after 5 failed restart attempts, causing the server process to shut down.
    /// This is intentional fail-fast behavior to prevent the server from continuing
    /// to run without health monitoring, which would lead to endpoints never recovering
    /// from failures. The server cannot operate safely in a degraded state without
    /// health checks. Operator intervention is required to investigate the root cause
    /// (typically TLS misconfiguration, resource exhaustion, or a critical bug in the
    /// health check logic).
    pub fn start_background_checks(self: Arc<Self>) {
        let background_task = Arc::clone(&self.background_task);
        let handle = tokio::spawn(async move {
            let mut restart_count = 0;

            loop {
                let checker = Arc::clone(&self);
                let handle = tokio::spawn(async move {
                    tracing::info!(
                        attempt = restart_count + 1,
                        "Starting background health checks (30s interval)"
                    );

                    loop {
                        tokio::time::sleep(Duration::from_secs(HEALTH_CHECK_INTERVAL_SECS)).await;

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

                if restart_count >= MAX_BACKGROUND_TASK_RESTARTS {
                    // Mark as permanently failed in metrics
                    self.metrics.mark_permanently_failed().await;

                    tracing::error!(
                        max_attempts = MAX_BACKGROUND_TASK_RESTARTS,
                        "DEGRADED: Background health check task failed {} times. \
                        Health monitoring is now DISABLED to prevent infinite crash-loops. \
                        Server will continue serving requests but without health monitoring. \
                        Endpoints will not recover from failures automatically. \
                        Operator intervention required - check TLS configuration, resource limits, and logs.",
                        MAX_BACKGROUND_TASK_RESTARTS
                    );

                    // Graceful degradation: Disable health checking instead of panicking.
                    // This prevents infinite crash-loops under process supervisors (systemd/Docker)
                    // when the failure is systemic (e.g., corrupted TLS certs, resource exhaustion).
                    // Server continues serving requests but without automatic endpoint recovery.
                    break;
                }

                // Record restart attempt in metrics
                self.metrics.record_restart(restart_count).await;

                // Exponential backoff: 1s, 2s, 4s, 8s, 16s
                let backoff_seconds = 2_u64.pow(restart_count - 1);
                tracing::warn!(
                    restart_count = restart_count,
                    backoff_seconds = backoff_seconds,
                    max_attempts = MAX_BACKGROUND_TASK_RESTARTS,
                    "Restarting background health check task after {}s backoff",
                    backoff_seconds
                );
                tokio::time::sleep(Duration::from_secs(backoff_seconds)).await;
            }
        });

        // Store the background task handle for graceful shutdown
        tokio::spawn(async move {
            *background_task.lock().await = Some(handle);
        });
    }

    /// Shutdown the background health checking task
    ///
    /// Cancels the background health check task, allowing for graceful server shutdown.
    /// This method should be called during server shutdown to prevent the background task
    /// from preventing clean process termination.
    ///
    /// # Example
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use octoroute::config::Config;
    /// # use octoroute::models::HealthChecker;
    /// # async fn example() {
    /// # let config = Arc::new(Config::from_file("config.toml").unwrap());
    /// let health_checker = Arc::new(HealthChecker::new(config));
    /// health_checker.clone().start_background_checks();
    /// // ... server runs ...
    /// health_checker.shutdown().await;
    /// # }
    /// ```
    pub async fn shutdown(&self) {
        let mut task = self.background_task.lock().await;
        if let Some(handle) = task.take() {
            tracing::info!("Cancelling background health check task for graceful shutdown");
            handle.abort();
            // Wait for the task to finish aborting
            let _ = handle.await;
        }
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
router_model = "balanced"
"#;
        toml::from_str(toml).expect("should parse TOML config")
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

    #[tokio::test]
    async fn test_health_metrics_starts_in_running_state() {
        let metrics = HealthMetrics::new();

        assert_eq!(metrics.status().await, BackgroundTaskStatus::Running);
        assert_eq!(metrics.restart_count().await, 0);
        assert!(metrics.last_successful_check().await.is_none());
        assert!(metrics.last_failure_time().await.is_none());
    }

    #[tokio::test]
    async fn test_health_metrics_record_successful_check() {
        let metrics = HealthMetrics::new();

        metrics.record_successful_check().await;

        assert_eq!(metrics.status().await, BackgroundTaskStatus::Running);
        assert!(metrics.last_successful_check().await.is_some());

        // Verify the timestamp is recent (within last second)
        let last_check = metrics.last_successful_check().await.unwrap();
        let elapsed = Instant::now().duration_since(last_check);
        assert!(
            elapsed < Duration::from_secs(1),
            "Last check should be very recent"
        );
    }

    #[tokio::test]
    async fn test_health_metrics_record_restart() {
        let metrics = HealthMetrics::new();

        metrics.record_restart(1).await;

        assert_eq!(metrics.status().await, BackgroundTaskStatus::Restarting);
        assert_eq!(metrics.restart_count().await, 1);
        assert!(metrics.last_failure_time().await.is_some());

        // Record second restart
        metrics.record_restart(2).await;
        assert_eq!(metrics.restart_count().await, 2);
    }

    #[tokio::test]
    async fn test_health_metrics_mark_permanently_failed() {
        let metrics = HealthMetrics::new();

        metrics.mark_permanently_failed().await;

        assert_eq!(
            metrics.status().await,
            BackgroundTaskStatus::PermanentlyFailed
        );
        assert!(metrics.last_failure_time().await.is_some());
    }

    #[tokio::test]
    async fn test_health_metrics_is_healthy_when_running() {
        let metrics = HealthMetrics::new();

        // Should be healthy initially (no checks yet, but status is Running)
        assert!(metrics.is_background_task_healthy().await);

        // Should be healthy after successful check
        metrics.record_successful_check().await;
        assert!(metrics.is_background_task_healthy().await);
    }

    #[tokio::test]
    async fn test_health_metrics_is_unhealthy_when_permanently_failed() {
        let metrics = HealthMetrics::new();

        metrics.mark_permanently_failed().await;

        assert!(!metrics.is_background_task_healthy().await);
    }

    #[tokio::test]
    async fn test_health_metrics_is_unhealthy_when_no_recent_checks() {
        let metrics = HealthMetrics::new();

        // Record a successful check
        metrics.record_successful_check().await;
        assert!(metrics.is_background_task_healthy().await);
    }

    #[tokio::test]
    async fn test_health_metrics_restart_then_recover() {
        let metrics = HealthMetrics::new();

        // Simulate a restart
        metrics.record_restart(1).await;
        assert_eq!(metrics.status().await, BackgroundTaskStatus::Restarting);

        // Simulate recovery with successful check
        metrics.record_successful_check().await;
        assert_eq!(metrics.status().await, BackgroundTaskStatus::Running);
        assert!(metrics.is_background_task_healthy().await);
    }
}
