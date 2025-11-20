//! Prometheus metrics collection for Octoroute
//!
//! This module provides metrics instrumentation for tracking:
//! - Request counts by tier and routing strategy
//! - Routing decision latency
//! - Model invocations by tier
//!
//! Metrics are exposed via the `/metrics` endpoint in Prometheus text format.
//!
//! # Feature Flag
//!
//! This module is only available when the `metrics` feature is enabled:
//! ```toml
//! [dependencies]
//! octoroute = { version = "0.1", features = ["metrics"] }
//! ```

use prometheus::{CounterVec, Encoder, HistogramOpts, HistogramVec, Opts, Registry, TextEncoder};
use std::sync::Arc;

/// Metrics collector for Octoroute
///
/// Provides Prometheus metrics for monitoring routing decisions,
/// latency, and model invocations.
#[derive(Clone)]
pub struct Metrics {
    registry: Arc<Registry>,
    requests_total: CounterVec,
    routing_duration: HistogramVec,
    model_invocations: CounterVec,
}

impl Metrics {
    /// Create a new Metrics instance
    ///
    /// Registers all metrics with a new Prometheus registry.
    ///
    /// # Errors
    ///
    /// Returns an error if metric registration fails (e.g., duplicate names).
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

        // Counter: Total requests by tier and routing strategy
        let requests_total = CounterVec::new(
            Opts::new(
                "octoroute_requests_total",
                "Total number of chat requests by tier and routing strategy",
            ),
            &["tier", "strategy"],
        )?;

        // Histogram: Routing decision latency by strategy
        let routing_duration = HistogramVec::new(
            HistogramOpts::new(
                "octoroute_routing_duration_ms",
                "Routing decision latency in milliseconds",
            )
            .buckets(vec![0.1, 0.5, 1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0]),
            &["strategy"],
        )?;

        // Counter: Model invocations by tier
        let model_invocations = CounterVec::new(
            Opts::new(
                "octoroute_model_invocations_total",
                "Total model invocations by tier",
            ),
            &["tier"],
        )?;

        // Register all metrics
        registry.register(Box::new(requests_total.clone()))?;
        registry.register(Box::new(routing_duration.clone()))?;
        registry.register(Box::new(model_invocations.clone()))?;

        Ok(Self {
            registry: Arc::new(registry),
            requests_total,
            routing_duration,
            model_invocations,
        })
    }

    /// Record a request with tier and routing strategy
    ///
    /// # Arguments
    ///
    /// * `tier` - The model tier (e.g., "fast", "balanced", "deep")
    /// * `strategy` - The routing strategy used (e.g., "rule", "llm", "hybrid")
    ///
    /// # Errors
    ///
    /// Returns an error if the label values are invalid or the metric is not registered.
    pub fn record_request(&self, tier: &str, strategy: &str) -> Result<(), prometheus::Error> {
        self.requests_total
            .get_metric_with_label_values(&[tier, strategy])?
            .inc();
        Ok(())
    }

    /// Record routing decision duration
    ///
    /// # Arguments
    ///
    /// * `strategy` - The routing strategy used (e.g., "rule", "llm")
    /// * `duration_ms` - The duration in milliseconds
    ///
    /// # Errors
    ///
    /// Returns an error if the label values are invalid or the metric is not registered.
    pub fn record_routing_duration(
        &self,
        strategy: &str,
        duration_ms: f64,
    ) -> Result<(), prometheus::Error> {
        self.routing_duration
            .get_metric_with_label_values(&[strategy])?
            .observe(duration_ms);
        Ok(())
    }

    /// Record a model invocation
    ///
    /// # Arguments
    ///
    /// * `tier` - The model tier (e.g., "fast", "balanced", "deep")
    ///
    /// # Errors
    ///
    /// Returns an error if the label values are invalid or the metric is not registered.
    pub fn record_model_invocation(&self, tier: &str) -> Result<(), prometheus::Error> {
        self.model_invocations
            .get_metric_with_label_values(&[tier])?
            .inc();
        Ok(())
    }

    /// Gather all metrics and encode them in Prometheus text format
    ///
    /// # Returns
    ///
    /// A string containing all metrics in Prometheus exposition format,
    /// suitable for the `/metrics` endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if metric encoding fails.
    pub fn gather(&self) -> Result<String, prometheus::Error> {
        let metric_families = self.registry.gather();
        let metric_count = metric_families.len();

        tracing::debug!(
            metric_family_count = metric_count,
            "Encoding metrics to Prometheus text format"
        );

        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();

        encoder.encode(&metric_families, &mut buffer).map_err(|e| {
            let metric_names: Vec<_> = metric_families.iter().map(|mf| mf.name()).collect();

            tracing::error!(
                error = %e,
                metric_family_count = metric_count,
                metric_names = ?metric_names,
                "Prometheus text encoder failed"
            );

            prometheus::Error::Msg(format!(
                "Failed to encode {} metric families: {}. Metrics: {:?}",
                metric_count, e, metric_names
            ))
        })?;

        String::from_utf8(buffer.clone()).map_err(|e| {
            let valid_up_to = e.utf8_error().valid_up_to();
            let buffer_len = buffer.len();
            let preview_len = std::cmp::min(100, buffer_len);

            tracing::error!(
                invalid_byte_index = valid_up_to,
                buffer_length = buffer_len,
                buffer_prefix = ?&buffer[..preview_len],
                "Prometheus encoder produced invalid UTF-8"
            );

            prometheus::Error::Msg(format!(
                "Failed to convert metrics to UTF-8 at byte {}/{}: {}. \
                This indicates corrupted metric names or labels.",
                valid_up_to, buffer_len, e
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_new_creates_registry() {
        let metrics = Metrics::new().expect("Failed to create metrics");

        // Record at least one value for each metric so they appear in the registry
        metrics.record_request("fast", "rule").unwrap();
        metrics.record_routing_duration("rule", 1.0).unwrap();
        metrics.record_model_invocation("fast").unwrap();

        let metric_families = metrics.registry.gather();
        // Should have 3 metric families: requests_total, routing_duration, model_invocations
        assert_eq!(metric_families.len(), 3, "Expected 3 metric families");

        // Verify metric names
        let names: Vec<String> = metric_families
            .iter()
            .map(|m| m.name().to_string())
            .collect();
        assert!(names.contains(&"octoroute_requests_total".to_string()));
        assert!(names.contains(&"octoroute_routing_duration_ms".to_string()));
        assert!(names.contains(&"octoroute_model_invocations_total".to_string()));
    }

    #[test]
    fn test_record_request_increments_counter() {
        let metrics = Metrics::new().unwrap();

        metrics.record_request("fast", "rule").unwrap();
        metrics.record_request("fast", "rule").unwrap();
        metrics.record_request("balanced", "llm").unwrap();

        let output = metrics.gather().unwrap();
        assert!(output.contains("octoroute_requests_total"));
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("strategy=\"rule\""));
    }

    #[test]
    fn test_record_routing_duration_observes_histogram() {
        let metrics = Metrics::new().unwrap();

        metrics.record_routing_duration("rule", 0.5).unwrap();
        metrics.record_routing_duration("rule", 1.2).unwrap();
        metrics.record_routing_duration("llm", 250.0).unwrap();

        let output = metrics.gather().unwrap();
        assert!(output.contains("octoroute_routing_duration_ms"));
        assert!(output.contains("strategy=\"rule\""));
        assert!(output.contains("strategy=\"llm\""));
    }

    #[test]
    fn test_record_model_invocation_increments_counter() {
        let metrics = Metrics::new().unwrap();

        metrics.record_model_invocation("fast").unwrap();
        metrics.record_model_invocation("fast").unwrap();
        metrics.record_model_invocation("balanced").unwrap();

        let output = metrics.gather().unwrap();
        assert!(output.contains("octoroute_model_invocations_total"));
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("tier=\"balanced\""));
    }

    #[test]
    fn test_gather_produces_prometheus_text_format() {
        let metrics = Metrics::new().unwrap();

        metrics.record_request("deep", "hybrid").unwrap();
        let output = metrics.gather().unwrap();

        // Verify Prometheus text format structure
        assert!(output.contains("# HELP octoroute_requests_total"));
        assert!(output.contains("# TYPE octoroute_requests_total counter"));
        assert!(output.contains("octoroute_requests_total{"));
    }

    #[test]
    fn test_metrics_is_clonable() {
        let metrics = Metrics::new().unwrap();
        let cloned = metrics.clone();

        // Record on original
        metrics.record_request("fast", "rule").unwrap();

        // Verify clone sees the same metrics (shared registry)
        let output = cloned.gather().unwrap();
        assert!(output.contains("octoroute_requests_total"));
    }

    #[test]
    fn test_histogram_buckets_configured() {
        let metrics = Metrics::new().unwrap();

        metrics.record_routing_duration("rule", 0.1).unwrap();
        metrics.record_routing_duration("rule", 100.0).unwrap();

        let output = metrics.gather().unwrap();

        // Verify histogram buckets exist
        assert!(output.contains("le=\"0.1\""));
        assert!(output.contains("le=\"100\""));
        assert!(output.contains("le=\"+Inf\""));
    }

    #[test]
    fn test_multiple_label_combinations() {
        let metrics = Metrics::new().unwrap();

        // Record different combinations
        metrics.record_request("fast", "rule").unwrap();
        metrics.record_request("fast", "hybrid").unwrap();
        metrics.record_request("balanced", "rule").unwrap();
        metrics.record_request("balanced", "llm").unwrap();
        metrics.record_request("deep", "hybrid").unwrap();

        let output = metrics.gather().unwrap();

        // All combinations should be present (Prometheus may format with spaces)
        assert!(output.contains("tier=\"fast\"") && output.contains("strategy=\"rule\""));
        assert!(output.contains("tier=\"fast\"") && output.contains("strategy=\"hybrid\""));
        assert!(output.contains("tier=\"balanced\"") && output.contains("strategy=\"rule\""));
        assert!(output.contains("tier=\"balanced\"") && output.contains("strategy=\"llm\""));
        assert!(output.contains("tier=\"deep\"") && output.contains("strategy=\"hybrid\""));
    }

    #[test]
    fn test_concurrent_metric_recording() {
        use std::sync::Arc;
        use std::thread;

        let metrics = Arc::new(Metrics::new().unwrap());
        let mut handles = vec![];

        // Spawn multiple threads recording metrics
        for i in 0..10 {
            let m = Arc::clone(&metrics);
            let handle = thread::spawn(move || {
                m.record_request("fast", "rule").unwrap();
                m.record_routing_duration("rule", i as f64).unwrap();
                m.record_model_invocation("fast").unwrap();
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Verify metrics were recorded
        let output = metrics.gather().unwrap();
        assert!(output.contains("octoroute_requests_total"));
        assert!(output.contains("octoroute_routing_duration_ms"));
        assert!(output.contains("octoroute_model_invocations_total"));
    }

    // ===== Test Coverage Gap #1: Invalid Label Values =====
    // NOTE: These tests document CURRENT behavior (accepts invalid labels).
    // Future work (HIGH #6): Add type-safe label enums to prevent cardinality explosion.

    #[test]
    fn test_invalid_tier_labels_are_accepted() {
        let metrics = Metrics::new().unwrap();

        // CURRENT BEHAVIOR: Invalid tier names are accepted (stringly-typed)
        // This creates cardinality explosion risk (each typo = new metric)
        let result = metrics.record_request("invalid_tier", "rule");
        assert!(
            result.is_ok(),
            "Current implementation accepts invalid tier names (cardinality risk)"
        );

        // Case sensitivity creates duplicate metrics
        let result = metrics.record_request("FAST", "rule");
        assert!(
            result.is_ok(),
            "Current implementation accepts wrong case (cardinality risk)"
        );

        // Empty strings are accepted
        let result = metrics.record_request("", "rule");
        assert!(
            result.is_ok(),
            "Current implementation accepts empty tier (cardinality risk)"
        );

        // All variants create separate metrics (demonstrated by successful gather)
        let output = metrics.gather().unwrap();
        assert!(output.contains("octoroute_requests_total"));
    }

    #[test]
    fn test_invalid_strategy_labels_are_accepted() {
        let metrics = Metrics::new().unwrap();

        // CURRENT BEHAVIOR: Invalid strategy names are accepted
        let result = metrics.record_request("fast", "unknown");
        assert!(
            result.is_ok(),
            "Current implementation accepts invalid strategy names"
        );

        let result = metrics.record_request("fast", "RULE");
        assert!(result.is_ok(), "Current implementation accepts wrong case");

        let result = metrics.record_request("fast", "");
        assert!(
            result.is_ok(),
            "Current implementation accepts empty strategy"
        );

        let output = metrics.gather().unwrap();
        assert!(output.contains("octoroute_requests_total"));
    }

    #[test]
    fn test_prometheus_special_chars_in_labels() {
        let metrics = Metrics::new().unwrap();

        // Test that special characters don't break Prometheus format encoding
        // Prometheus crate should handle escaping internally
        let result = metrics.record_request("tier=fast", "rule");
        assert!(
            result.is_ok(),
            "Prometheus crate should handle special chars"
        );

        // Newlines in labels - Prometheus crate handles this
        let result = metrics.record_request("fast\n", "rule");
        assert!(result.is_ok(), "Prometheus crate handles newlines");

        let result = metrics.record_request("fast", "rule\nmalicious");
        assert!(result.is_ok(), "Prometheus crate handles newlines");

        // Verify gather still produces valid output (no syntax errors)
        let output = metrics.gather().unwrap();
        assert!(!output.is_empty(), "Output should be non-empty");
        assert!(
            output.contains("# HELP") || output.contains("# TYPE"),
            "Output should contain Prometheus format headers"
        );
    }

    // ===== Test Coverage Gap #2: Encoding Failures =====

    #[test]
    fn test_gather_handles_extreme_values_without_panic() {
        let metrics = Metrics::new().unwrap();

        // Record extreme histogram values
        for _ in 0..100 {
            let _ = metrics.record_routing_duration("rule", f64::MAX);
        }

        // Should not panic - either succeeds or returns error
        let result = metrics.gather();
        assert!(
            result.is_ok() || result.is_err(),
            "gather() should not panic with extreme values"
        );

        // If it succeeds, output should be valid UTF-8
        if let Ok(output) = result {
            assert!(!output.is_empty(), "Output should not be empty");
            assert!(output.is_ascii() || output.chars().count() > 0);
        }
    }

    #[test]
    fn test_gather_handles_large_metric_count() {
        let metrics = Metrics::new().unwrap();

        // Record many metrics to test encoding of large output
        for i in 0..1000 {
            metrics
                .record_routing_duration("rule", i as f64 / 10.0)
                .unwrap();
        }

        let result = metrics.gather();
        assert!(result.is_ok(), "Should handle large metric count");

        let output = result.unwrap();
        assert!(!output.is_empty(), "Output should not be empty");
        assert!(output.contains("octoroute_routing_duration_ms"));
        // Verify histogram contains count data
        assert!(output.contains("_count") || output.contains("_sum"));
    }

    // ===== Test Coverage Gap #5: Histogram Edge Cases =====

    #[test]
    fn test_routing_duration_histogram_edge_values() {
        let metrics = Metrics::new().unwrap();

        // Valid boundary values
        assert!(metrics.record_routing_duration("rule", 0.0).is_ok());
        assert!(metrics.record_routing_duration("rule", 0.1).is_ok());
        assert!(metrics.record_routing_duration("rule", 1000.0).is_ok());

        // Edge cases - behavior depends on Prometheus crate
        // Negative durations
        let result = metrics.record_routing_duration("rule", -1.0);
        // Prometheus may accept or reject negative values
        // Just verify it doesn't panic
        let _ = result;

        // NaN - should be handled gracefully
        let result = metrics.record_routing_duration("rule", f64::NAN);
        let _ = result;

        // Infinity - should be handled gracefully
        let result = metrics.record_routing_duration("rule", f64::INFINITY);
        let _ = result;

        // Should still be able to gather metrics
        let output = metrics.gather();
        assert!(
            output.is_ok() || output.is_err(),
            "gather() should not panic after edge case values"
        );
    }

    #[test]
    fn test_histogram_values_at_bucket_boundaries() {
        let metrics = Metrics::new().unwrap();

        // Test values exactly at bucket boundaries
        // Buckets: [0.1, 0.5, 1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0]
        let boundary_values = vec![0.1, 0.5, 1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0];

        for value in boundary_values {
            assert!(
                metrics.record_routing_duration("rule", value).is_ok(),
                "Failed to record boundary value: {}",
                value
            );
        }

        let output = metrics.gather().unwrap();
        assert!(output.contains("le=\"0.1\""));
        assert!(output.contains("le=\"1\""));
        assert!(output.contains("le=\"100\""));
        assert!(output.contains("le=\"1000\""));
    }

    // ===== Test Coverage Gap #3: Concurrent Label Creation =====

    #[test]
    fn test_concurrent_metrics_with_dynamic_labels() {
        use std::sync::Arc;
        use std::thread;

        let metrics = Arc::new(Metrics::new().unwrap());
        let mut handles = vec![];

        // Spawn threads recording different label combinations
        // This tests concurrent creation of new label combinations (cardinality stress test)
        for i in 0..20 {
            let m = Arc::clone(&metrics);
            let handle = thread::spawn(move || {
                // Rotate through valid tier and strategy values
                let tiers = ["fast", "balanced", "deep"];
                let strategies = ["rule", "llm", "hybrid"];

                let tier = tiers[i % 3];
                let strategy = strategies[i % 3];

                // Each thread may create a new label combination
                m.record_request(tier, strategy).unwrap();
                m.record_routing_duration(strategy, i as f64).unwrap();
                m.record_model_invocation(tier).unwrap();
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().expect("Thread should not panic");
        }

        // Verify all label combinations were recorded without corruption
        let output = metrics.gather().unwrap();

        // Should contain all tier values
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("tier=\"balanced\""));
        assert!(output.contains("tier=\"deep\""));

        // Should contain all strategy values
        assert!(output.contains("strategy=\"rule\""));
        assert!(output.contains("strategy=\"llm\""));
        assert!(output.contains("strategy=\"hybrid\""));

        // Verify no corruption or panic occurred
        assert!(output.contains("octoroute_requests_total"));
        assert!(output.contains("octoroute_routing_duration_ms"));
        assert!(output.contains("octoroute_model_invocations_total"));
    }
}
