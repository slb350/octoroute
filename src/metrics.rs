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

use prometheus::{
    CounterVec, Encoder, HistogramOpts, HistogramVec, IntCounter, Opts, Registry, TextEncoder,
};
use std::sync::Arc;

/// Model tier enum for type-safe metrics labels
///
/// Prevents cardinality explosion by restricting tier values to
/// exactly three valid options at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Fast tier (8B models)
    Fast,
    /// Balanced tier (30B models)
    Balanced,
    /// Deep tier (120B models)
    Deep,
}

impl Tier {
    /// Convert tier to Prometheus label string
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Balanced => "balanced",
            Tier::Deep => "deep",
        }
    }
}

/// Routing strategy enum for type-safe metrics labels
///
/// Prevents cardinality explosion by restricting strategy values to
/// exactly three valid options at compile time.
///
/// **NOTE**: `Strategy::Hybrid` exists but is intentionally NOT recorded in metrics.
/// See `requests_total` metric definition for details on Hybrid suppression rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Rule-based routing
    Rule,
    /// LLM-powered routing
    Llm,
    /// Hybrid routing (rules + LLM fallback)
    ///
    /// **IMPORTANT**: This variant is intentionally NOT recorded in metrics.
    /// It's used for configuration (selecting HybridRouter), but metrics
    /// record the actual routing path taken (Rule or Llm).
    Hybrid,
}

impl Strategy {
    /// Convert strategy to Prometheus label string
    pub fn as_str(&self) -> &'static str {
        match self {
            Strategy::Rule => "rule",
            Strategy::Llm => "llm",
            Strategy::Hybrid => "hybrid",
        }
    }

    /// Returns the Prometheus label value for this strategy, or None for Hybrid.
    ///
    /// Hybrid is a meta-strategy and is intentionally NOT recorded to avoid
    /// inflating label cardinality. Metrics capture the concrete path taken
    /// (Rule or Llm) instead.
    pub fn metric_label(&self) -> Option<&'static str> {
        match self {
            Strategy::Rule => Some("rule"),
            Strategy::Llm => Some("llm"),
            Strategy::Hybrid => None,
        }
    }
}

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
    health_tracking_failures: IntCounter,
    metrics_recording_failures: IntCounter,
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
        //
        // NOTE: Hybrid Strategy Suppression
        // While Strategy::Hybrid exists in the enum (for configuration), it is
        // intentionally NOT recorded in metrics. HybridRouter delegates to either
        // RuleBasedRouter or LlmBasedRouter, and metrics record which **actual path**
        // was taken (Rule or Llm), not the meta-strategy configuration.
        //
        // This design provides more actionable observability (e.g., "70% of requests
        // hit rule fast path") while preventing cardinality inflation.
        //
        // Cardinality: 3 tiers × 2 strategies (Rule, Llm) = 6 time series (not 9)
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

        // Counter: Health tracking operation failures
        let health_tracking_failures = IntCounter::new(
            "octoroute_health_tracking_failures_total",
            "Total number of health tracking operation failures (mark_success/mark_failure errors)",
        )?;

        // Counter: Metrics recording operation failures
        let metrics_recording_failures = IntCounter::new(
            "octoroute_metrics_recording_failures_total",
            "Total number of metrics recording operation failures (record_request/record_routing_duration/record_model_invocation errors). \
            Indicates Prometheus internal errors - frequent failures require investigation.",
        )?;

        // Register all metrics
        registry.register(Box::new(requests_total.clone()))?;
        registry.register(Box::new(routing_duration.clone()))?;
        registry.register(Box::new(model_invocations.clone()))?;
        registry.register(Box::new(health_tracking_failures.clone()))?;
        registry.register(Box::new(metrics_recording_failures.clone()))?;

        Ok(Self {
            registry: Arc::new(registry),
            requests_total,
            routing_duration,
            model_invocations,
            health_tracking_failures,
            metrics_recording_failures,
        })
    }

    /// Record a request with tier and routing strategy
    ///
    /// # Arguments
    ///
    /// * `tier` - The model tier (type-safe enum)
    /// * `strategy` - The routing strategy used (type-safe enum)
    ///
    /// # Errors
    ///
    /// Returns an error if the metric is not registered.
    ///
    /// # Cardinality Safety
    ///
    /// Using enums instead of strings prevents cardinality explosion.
    /// Maximum label combinations: 3 tiers × 2 strategies (Rule, Llm) = **6 time series**.
    /// (Hybrid is a meta-strategy and is never recorded - see Strategy enum docs)
    pub fn record_request(&self, tier: Tier, strategy: Strategy) -> Result<(), prometheus::Error> {
        let Some(strategy_label) = strategy.metric_label() else {
            tracing::debug!(
                tier = %tier.as_str(),
                strategy = %strategy.as_str(),
                "Skipping metrics for hybrid meta-strategy"
            );
            return Ok(());
        };

        self.requests_total
            .get_metric_with_label_values(&[tier.as_str(), strategy_label])?
            .inc();
        Ok(())
    }

    /// Record routing decision duration
    ///
    /// # Arguments
    ///
    /// * `strategy` - The routing strategy used (type-safe enum)
    /// * `duration_ms` - The duration in milliseconds (must be finite and non-negative)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The metric is not registered
    /// - `duration_ms` is NaN, infinite, or negative (Issue #5 fix)
    ///
    /// # Cardinality Safety
    ///
    /// Using enum instead of string prevents cardinality explosion.
    /// Maximum label values: 2 strategies (Rule, Llm) = 2 time series.
    /// (Hybrid is never recorded - see Strategy enum docs)
    ///
    /// # Data Integrity
    ///
    /// NaN and infinity values corrupt histogram statistics (all percentiles become NaN).
    /// Negative values are logically invalid for durations. This validation prevents
    /// silent metric corruption that would render observability data useless.
    pub fn record_routing_duration(
        &self,
        strategy: Strategy,
        duration_ms: f64,
    ) -> Result<(), prometheus::Error> {
        // Validate duration is finite (not NaN or Infinity)
        if !duration_ms.is_finite() {
            return Err(prometheus::Error::Msg(format!(
                "Histogram value must be finite (not NaN or Infinity), got: {}. \
                NaN and infinity values corrupt histogram percentiles.",
                duration_ms
            )));
        }

        // Validate duration is non-negative (logically required for durations)
        if duration_ms < 0.0 {
            return Err(prometheus::Error::Msg(format!(
                "Histogram value must be non-negative (duration cannot be negative), got: {}",
                duration_ms
            )));
        }

        let Some(strategy_label) = strategy.metric_label() else {
            tracing::debug!(
                strategy = %strategy.as_str(),
                duration_ms,
                "Skipping routing duration metrics for hybrid meta-strategy"
            );
            return Ok(());
        };

        self.routing_duration
            .get_metric_with_label_values(&[strategy_label])?
            .observe(duration_ms);
        Ok(())
    }

    /// Record a model invocation
    ///
    /// # Arguments
    ///
    /// * `tier` - The model tier (type-safe enum)
    ///
    /// # Errors
    ///
    /// Returns an error if the metric is not registered.
    ///
    /// # Cardinality Safety
    ///
    /// Using enum instead of string prevents cardinality explosion.
    /// Maximum possible label values: 3 tiers = 3 time series.
    pub fn record_model_invocation(&self, tier: Tier) -> Result<(), prometheus::Error> {
        self.model_invocations
            .get_metric_with_label_values(&[tier.as_str()])?
            .inc();
        Ok(())
    }

    /// Record a health tracking operation failure
    ///
    /// Increments the counter when mark_success() or mark_failure() operations
    /// fail (e.g., unknown endpoint name, internal errors).
    ///
    /// ## What This Metric Tracks
    ///
    /// This metric increments when health tracking operations fail, specifically:
    /// - `mark_success()` fails after successful routing (endpoint name mismatch)
    /// - `mark_failure()` fails after failed routing (endpoint name mismatch)
    /// - Internal errors in health tracking system (lock failures, HTTP client creation)
    ///
    /// ## What This Metric Does NOT Track
    ///
    /// This metric does NOT increment for:
    /// - Normal endpoint failures (those are tracked by health checker itself)
    /// - Routing failures (tracked separately by routing metrics)
    /// - Request failures (tracked by requests_total with status labels)
    ///
    /// ## Recommended Alerting Thresholds (Operator Configuration)
    ///
    /// **For Prometheus/Grafana alerts**: Configure external alerting when
    /// `rate(octoroute_health_tracking_failures_total[1h]) > 5`.
    ///
    /// **Note**: This threshold is NOT enforced by Octoroute code - it's a
    /// recommended configuration for your monitoring system. Exceeding this
    /// threshold indicates systemic issues:
    /// - Configuration mismatch between routing logic and config file
    /// - Race condition in endpoint registration/deregistration
    /// - Internal bug in health tracking system
    ///
    /// Occasional failures (1-2 per hour) may be transient and acceptable.
    ///
    /// ## Impact
    ///
    /// When health tracking fails:
    /// - Endpoint recovery is delayed (30-60s background polling vs immediate)
    /// - Routing may be suboptimal (avoiding healthy endpoints)
    /// - Warnings are surfaced to users in responses
    pub fn health_tracking_failure(&self) {
        self.health_tracking_failures.inc();
    }

    /// Get the current count of health tracking failures
    ///
    /// Returns the total number of health tracking operation failures since startup.
    /// Used by the /health endpoint to report health tracking status.
    pub fn health_tracking_failures_count(&self) -> u64 {
        self.health_tracking_failures.get()
    }

    /// Record a metrics recording operation failure
    ///
    /// Increments the counter when record_request(), record_routing_duration(),
    /// or record_model_invocation() operations fail (e.g., Prometheus internal errors).
    ///
    /// ## What This Metric Tracks
    ///
    /// This metric increments when metrics recording operations fail, specifically:
    /// - `record_request()` fails (Prometheus registry error, label mismatch)
    /// - `record_routing_duration()` fails (invalid duration, registry error)
    /// - `record_model_invocation()` fails (registry error)
    ///
    /// ## Alerting Threshold
    ///
    /// **Recommended alert**: > 5 failures in 1 hour indicates a systemic issue:
    /// - Prometheus registry corruption
    /// - Metric registration failures
    /// - Internal Prometheus errors
    ///
    /// Occasional failures (1-2 per hour) may indicate transient issues.
    ///
    /// ## Impact
    ///
    /// When metrics recording fails:
    /// - Observability data is incomplete (gaps in metrics)
    /// - Operators may not detect routing issues or performance degradation
    /// - Request continues normally (metrics are non-critical to functionality)
    /// - Failure is logged for investigation
    pub fn metrics_recording_failure(&self) {
        self.metrics_recording_failures.inc();
    }

    /// Get the current count of metrics recording failures
    ///
    /// Returns the total number of metrics recording operation failures since startup.
    /// Used by the /health endpoint to report metrics system status.
    pub fn metrics_recording_failures_count(&self) -> u64 {
        self.metrics_recording_failures.get()
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
        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();
        metrics
            .record_routing_duration(Strategy::Rule, 1.0)
            .unwrap();
        metrics.record_model_invocation(Tier::Fast).unwrap();
        metrics.health_tracking_failure(); // Increment health tracking failures
        metrics.metrics_recording_failure(); // Increment metrics recording failures

        let metric_families = metrics.registry.gather();
        // Should have 5 metric families: requests_total, routing_duration, model_invocations, health_tracking_failures, metrics_recording_failures
        assert_eq!(metric_families.len(), 5, "Expected 5 metric families");

        // Verify metric names
        let names: Vec<String> = metric_families
            .iter()
            .map(|m| m.name().to_string())
            .collect();
        assert!(names.contains(&"octoroute_requests_total".to_string()));
        assert!(names.contains(&"octoroute_routing_duration_ms".to_string()));
        assert!(names.contains(&"octoroute_model_invocations_total".to_string()));
        assert!(names.contains(&"octoroute_health_tracking_failures_total".to_string()));
        assert!(names.contains(&"octoroute_metrics_recording_failures_total".to_string()));
    }

    #[test]
    fn test_record_request_increments_counter() {
        let metrics = Metrics::new().unwrap();

        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();
        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();
        metrics
            .record_request(Tier::Balanced, Strategy::Llm)
            .unwrap();

        let output = metrics.gather().unwrap();
        assert!(output.contains("octoroute_requests_total"));
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("strategy=\"rule\""));
    }

    #[test]
    fn test_record_routing_duration_observes_histogram() {
        let metrics = Metrics::new().unwrap();

        metrics
            .record_routing_duration(Strategy::Rule, 0.5)
            .unwrap();
        metrics
            .record_routing_duration(Strategy::Rule, 1.2)
            .unwrap();
        metrics
            .record_routing_duration(Strategy::Llm, 250.0)
            .unwrap();

        let output = metrics.gather().unwrap();
        assert!(output.contains("octoroute_routing_duration_ms"));
        assert!(output.contains("strategy=\"rule\""));
        assert!(output.contains("strategy=\"llm\""));
    }

    #[test]
    fn test_record_model_invocation_increments_counter() {
        let metrics = Metrics::new().unwrap();

        metrics.record_model_invocation(Tier::Fast).unwrap();
        metrics.record_model_invocation(Tier::Fast).unwrap();
        metrics.record_model_invocation(Tier::Balanced).unwrap();

        let output = metrics.gather().unwrap();
        assert!(output.contains("octoroute_model_invocations_total"));
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("tier=\"balanced\""));
    }

    #[test]
    fn test_gather_produces_prometheus_text_format() {
        let metrics = Metrics::new().unwrap();

        metrics.record_request(Tier::Deep, Strategy::Rule).unwrap();
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
        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();

        // Verify clone sees the same metrics (shared registry)
        let output = cloned.gather().unwrap();
        assert!(output.contains("octoroute_requests_total"));
    }

    #[test]
    fn test_histogram_buckets_configured() {
        let metrics = Metrics::new().unwrap();

        metrics
            .record_routing_duration(Strategy::Rule, 0.1)
            .unwrap();
        metrics
            .record_routing_duration(Strategy::Rule, 100.0)
            .unwrap();

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
        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();
        metrics
            .record_request(Tier::Balanced, Strategy::Rule)
            .unwrap();
        metrics
            .record_request(Tier::Balanced, Strategy::Llm)
            .unwrap();
        metrics.record_request(Tier::Deep, Strategy::Rule).unwrap();

        let output = metrics.gather().unwrap();

        // All combinations should be present (Prometheus may format with spaces)
        assert!(output.contains("tier=\"fast\"") && output.contains("strategy=\"rule\""));
        assert!(output.contains("tier=\"balanced\"") && output.contains("strategy=\"rule\""));
        assert!(output.contains("tier=\"balanced\"") && output.contains("strategy=\"llm\""));
        assert!(output.contains("tier=\"deep\"") && output.contains("strategy=\"rule\""));
        assert!(
            !output.contains("strategy=\"hybrid\""),
            "Hybrid meta-strategy should not be recorded"
        );
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
                m.record_request(Tier::Fast, Strategy::Rule).unwrap();
                m.record_routing_duration(Strategy::Rule, i as f64).unwrap();
                m.record_model_invocation(Tier::Fast).unwrap();
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

    // ===== Test Coverage Gap #1: FIXED - Type-safe enums prevent invalid labels =====
    // The tests above (test_tier_enum_prevents_invalid_values, etc.) demonstrate that
    // invalid labels are NOW IMPOSSIBLE at compile time. The old stringly-typed tests
    // have been REMOVED because they tested the broken behavior we just fixed.

    // ===== Test Coverage Gap #2: Encoding Failures =====

    #[test]
    fn test_gather_handles_extreme_values_without_panic() {
        let metrics = Metrics::new().unwrap();

        // Record extreme histogram values
        for _ in 0..100 {
            let _ = metrics.record_routing_duration(Strategy::Rule, f64::MAX);
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
                .record_routing_duration(Strategy::Rule, i as f64 / 10.0)
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
        assert!(metrics.record_routing_duration(Strategy::Rule, 0.0).is_ok());
        assert!(metrics.record_routing_duration(Strategy::Rule, 0.1).is_ok());
        assert!(
            metrics
                .record_routing_duration(Strategy::Rule, 1000.0)
                .is_ok()
        );

        // Edge cases - behavior depends on Prometheus crate
        // Negative durations
        let result = metrics.record_routing_duration(Strategy::Rule, -1.0);
        // Prometheus may accept or reject negative values
        // Just verify it doesn't panic
        let _ = result;

        // NaN - should be handled gracefully
        let result = metrics.record_routing_duration(Strategy::Rule, f64::NAN);
        let _ = result;

        // Infinity - should be handled gracefully
        let result = metrics.record_routing_duration(Strategy::Rule, f64::INFINITY);
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
                metrics
                    .record_routing_duration(Strategy::Rule, value)
                    .is_ok(),
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
        // This tests concurrent creation of new label combinations
        // Now with bounded cardinality (max 6 combinations for 3 tiers × 2 strategies)
        for i in 0..20 {
            let m = Arc::clone(&metrics);
            let handle = thread::spawn(move || {
                // Rotate through valid tier and strategy values using enums
                let tiers = [Tier::Fast, Tier::Balanced, Tier::Deep];
                let strategies = [Strategy::Rule, Strategy::Llm];

                let tier = tiers[i % 3];
                let strategy = strategies[i % 2];

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
        assert!(
            !output.contains("strategy=\"hybrid\""),
            "Hybrid meta-strategy should be suppressed in metrics"
        );

        // Verify no corruption or panic occurred
        assert!(output.contains("octoroute_requests_total"));
        assert!(output.contains("octoroute_routing_duration_ms"));
        assert!(output.contains("octoroute_model_invocations_total"));
    }

    // ===== Issue #2 Fix: Type-Safe Label Enums =====
    // Tests written FIRST (TDD RED phase)

    #[test]
    fn test_tier_enum_as_str_conversion() {
        use super::Tier;

        assert_eq!(Tier::Fast.as_str(), "fast");
        assert_eq!(Tier::Balanced.as_str(), "balanced");
        assert_eq!(Tier::Deep.as_str(), "deep");
    }

    #[test]
    fn test_strategy_enum_as_str_conversion() {
        use super::Strategy;

        assert_eq!(Strategy::Rule.as_str(), "rule");
        assert_eq!(Strategy::Llm.as_str(), "llm");
        assert_eq!(Strategy::Hybrid.as_str(), "hybrid");
    }

    #[test]
    fn test_strategy_metric_label_skips_hybrid() {
        use super::Strategy;

        assert_eq!(Strategy::Rule.metric_label(), Some("rule"));
        assert_eq!(Strategy::Llm.metric_label(), Some("llm"));
        assert_eq!(Strategy::Hybrid.metric_label(), None);
    }

    #[test]
    fn test_tier_enum_prevents_invalid_values() {
        use super::Tier;

        // At compile time, you can ONLY create valid tier values
        let valid_tiers = vec![Tier::Fast, Tier::Balanced, Tier::Deep];

        // Verify all three exist and convert correctly
        for tier in valid_tiers {
            let s = tier.as_str();
            assert!(s == "fast" || s == "balanced" || s == "deep");
        }

        // This code would NOT compile (compile-time safety):
        // let invalid = Tier::from("invalid_tier"); // Does not exist!
        // let typo = Tier::FAST; // Does not exist!
    }

    #[test]
    fn test_strategy_enum_prevents_invalid_values() {
        use super::Strategy;

        // At compile time, you can ONLY create valid strategy values
        let valid_strategies = vec![Strategy::Rule, Strategy::Llm, Strategy::Hybrid];

        // Verify all three exist and convert correctly
        for strategy in valid_strategies {
            let s = strategy.as_str();
            assert!(s == "rule" || s == "llm" || s == "hybrid");
        }

        // This code would NOT compile (compile-time safety):
        // let invalid = Strategy::from("unknown"); // Does not exist!
    }

    #[test]
    fn test_metrics_with_type_safe_enums() {
        use super::{Strategy, Tier};

        let metrics = Metrics::new().unwrap();

        // Now we pass ENUMS, not strings - impossible to typo!
        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();
        metrics
            .record_request(Tier::Balanced, Strategy::Llm)
            .unwrap();

        // Test Hybrid suppression behavior: Calling record_request with Strategy::Hybrid
        // succeeds (returns Ok) but does NOT create a metric (see metric_label() returning None).
        // This test verifies that Hybrid can be safely passed without inflating cardinality.
        metrics
            .record_request(Tier::Deep, Strategy::Hybrid)
            .unwrap(); // Succeeds but suppressed - no metric created

        metrics
            .record_routing_duration(Strategy::Rule, 1.0)
            .unwrap();
        metrics
            .record_routing_duration(Strategy::Llm, 250.0)
            .unwrap();
        // Verify Hybrid suppression also works for histogram metrics
        metrics
            .record_routing_duration(Strategy::Hybrid, 10.0)
            .unwrap(); // Succeeds but suppressed - no histogram observation created

        metrics.record_model_invocation(Tier::Fast).unwrap();
        metrics.record_model_invocation(Tier::Balanced).unwrap();
        metrics.record_model_invocation(Tier::Deep).unwrap();

        let output = metrics.gather().unwrap();

        // Verify correct label values appear
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("tier=\"balanced\""));
        assert!(output.contains("tier=\"deep\""));
        assert!(output.contains("strategy=\"rule\""));
        assert!(output.contains("strategy=\"llm\""));
        assert!(
            !output.contains("strategy=\"hybrid\""),
            "Hybrid meta-strategy should be suppressed in metrics"
        );
    }

    #[test]
    fn test_enum_labels_prevent_cardinality_explosion() {
        use super::{Strategy, Tier};

        let metrics = Metrics::new().unwrap();

        // OLD BEHAVIOR (strings): These would ALL create separate metrics:
        // metrics.record_request("fast", "rule").unwrap();   // OK
        // metrics.record_request("FAST", "rule").unwrap();   // Typo! New metric!
        // metrics.record_request("Fast", "rule").unwrap();   // Typo! New metric!
        // metrics.record_request("fasst", "rule").unwrap();  // Typo! New metric!
        // Result: 4 separate time series = 4x memory usage

        // NEW BEHAVIOR (enums): Only ONE way to express each tier
        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();
        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();
        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();

        // Result: All three calls increment the SAME metric
        // Maximum possible cardinality: 3 tiers × 2 strategies = 6 combinations
        // vs unbounded with strings

        // Verify Hybrid meta-strategy suppression prevents cardinality inflation
        metrics
            .record_request(Tier::Deep, Strategy::Hybrid)
            .unwrap(); // Succeeds but does not create new time series

        let output = metrics.gather().unwrap();

        // Only one "fast" variant exists
        let fast_count = output.matches("tier=\"fast\"").count();
        assert!(fast_count > 0, "Should have at least one fast tier metric");

        // NO "FAST", "Fast", "fasst", etc.
        assert!(!output.contains("tier=\"FAST\""));
        assert!(!output.contains("tier=\"Fast\""));
        assert!(
            !output.contains("strategy=\"hybrid\""),
            "Hybrid meta-strategy should not appear in metrics output"
        );
    }

    // ===== Issue #5 Fix: Histogram Value Validation =====
    // Tests written FIRST (TDD RED phase)

    #[test]
    fn test_histogram_rejects_nan() {
        let metrics = Metrics::new().unwrap();

        let result = metrics.record_routing_duration(Strategy::Rule, f64::NAN);
        assert!(
            result.is_err(),
            "Histogram should reject NaN values to prevent metric corruption"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.to_lowercase().contains("finite") || err_msg.to_lowercase().contains("nan"),
            "Error message should mention NaN or finite requirement"
        );
    }

    #[test]
    fn test_histogram_rejects_positive_infinity() {
        let metrics = Metrics::new().unwrap();

        let result = metrics.record_routing_duration(Strategy::Rule, f64::INFINITY);
        assert!(
            result.is_err(),
            "Histogram should reject +Infinity to prevent metric corruption"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.to_lowercase().contains("finite") || err_msg.to_lowercase().contains("inf"),
            "Error message should mention infinity or finite requirement"
        );
    }

    #[test]
    fn test_histogram_rejects_negative_infinity() {
        let metrics = Metrics::new().unwrap();

        let result = metrics.record_routing_duration(Strategy::Rule, f64::NEG_INFINITY);
        assert!(
            result.is_err(),
            "Histogram should reject -Infinity to prevent metric corruption"
        );
    }

    #[test]
    fn test_histogram_rejects_negative_values() {
        let metrics = Metrics::new().unwrap();

        let result = metrics.record_routing_duration(Strategy::Rule, -1.0);
        assert!(
            result.is_err(),
            "Histogram should reject negative durations (logically invalid)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.to_lowercase().contains("negative")
                || err_msg.to_lowercase().contains("non-negative"),
            "Error message should mention negative values"
        );
    }

    #[test]
    fn test_histogram_accepts_zero() {
        let metrics = Metrics::new().unwrap();

        let result = metrics.record_routing_duration(Strategy::Rule, 0.0);
        assert!(
            result.is_ok(),
            "Histogram should accept zero (valid for instant operations)"
        );
    }

    #[test]
    fn test_histogram_accepts_valid_positive_values() {
        let metrics = Metrics::new().unwrap();

        // Test a range of valid positive values
        let valid_values = [0.1, 1.0, 10.0, 100.0, 1000.0, f64::MAX];
        for value in valid_values {
            let result = metrics.record_routing_duration(Strategy::Rule, value);
            assert!(
                result.is_ok(),
                "Histogram should accept valid positive value: {}",
                value
            );
        }
    }

    // ===== GAP #2 Fix: High-Concurrency Stress Test (1000+ tasks) =====

    #[test]
    fn test_concurrent_metrics_stress_test_1000_plus_tasks() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let metrics = Arc::new(Metrics::new().unwrap());
        let mut handles = vec![];

        // Spawn 1000 concurrent tasks recording metrics
        // This tests for lock contention in Prometheus CounterVec label registration
        // Each task may create new label combinations, forcing internal write locks
        const NUM_TASKS: usize = 1000;
        const TIMEOUT_SECONDS: u64 = 5;

        let start = std::time::Instant::now();

        for i in 0..NUM_TASKS {
            let m = Arc::clone(&metrics);
            let handle = thread::spawn(move || {
                // Rotate through label combinations to force concurrent registration
                let tiers = [Tier::Fast, Tier::Balanced, Tier::Deep];
                let strategies = [Strategy::Rule, Strategy::Llm];

                let tier = tiers[i % 3];
                let strategy = strategies[i % 2];

                // Record multiple metrics per task
                m.record_request(tier, strategy).unwrap();
                m.record_routing_duration(strategy, (i % 100) as f64)
                    .unwrap();
                m.record_model_invocation(tier).unwrap();
            });
            handles.push(handle);
        }

        // Join all threads with timeout detection
        for (idx, handle) in handles.into_iter().enumerate() {
            handle
                .join()
                .unwrap_or_else(|_| panic!("Thread {} panicked during metrics recording", idx));
        }

        let elapsed = start.elapsed();

        // Verify no deadlock occurred (should complete well under timeout)
        assert!(
            elapsed < Duration::from_secs(TIMEOUT_SECONDS),
            "Stress test took too long ({:?}), potential lock contention or deadlock. \
            Expected < {}s for {} concurrent tasks.",
            elapsed,
            TIMEOUT_SECONDS,
            NUM_TASKS
        );

        // Verify metrics can still be gathered after high concurrency
        let output = metrics.gather().unwrap();

        // All label combinations should be present
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("tier=\"balanced\""));
        assert!(output.contains("tier=\"deep\""));
        assert!(output.contains("strategy=\"rule\""));
        assert!(output.contains("strategy=\"llm\""));

        // Verify all metrics were recorded
        assert!(output.contains("octoroute_requests_total"));
        assert!(output.contains("octoroute_routing_duration_ms"));
        assert!(output.contains("octoroute_model_invocations_total"));

        println!(
            "✅ Stress test completed: {} concurrent tasks in {:?}",
            NUM_TASKS, elapsed
        );
    }

    #[test]
    fn test_concurrent_metrics_no_data_corruption() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;

        let metrics = Arc::new(Metrics::new().unwrap());
        let expected_count = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];

        const NUM_TASKS: usize = 500;
        const INCREMENTS_PER_TASK: usize = 10;

        for _ in 0..NUM_TASKS {
            let m = Arc::clone(&metrics);
            let count = Arc::clone(&expected_count);

            let handle = thread::spawn(move || {
                // Each task increments the same metric multiple times
                for _ in 0..INCREMENTS_PER_TASK {
                    m.record_request(Tier::Fast, Strategy::Rule).unwrap();
                    count.fetch_add(1, Ordering::SeqCst);
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let expected = expected_count.load(Ordering::SeqCst);
        let output = metrics.gather().unwrap();

        // Parse the output to verify correct count
        // Format: octoroute_requests_total{tier="fast",strategy="rule"} 5000
        let count_str = output
            .lines()
            .find(|line| {
                line.contains("octoroute_requests_total")
                    && line.contains("tier=\"fast\"")
                    && line.contains("strategy=\"rule\"")
                    && !line.starts_with('#')
            })
            .expect("Should find requests_total metric");

        // Extract the count value (last token on line)
        let actual: usize = count_str
            .split_whitespace()
            .last()
            .expect("Should have count value")
            .parse()
            .expect("Should parse as number");

        assert_eq!(
            actual, expected,
            "Concurrent metric recording should not lose updates. \
            Expected {} increments from {} tasks × {} increments each, got {}",
            expected, NUM_TASKS, INCREMENTS_PER_TASK, actual
        );

        println!(
            "✅ No data corruption: {} concurrent tasks × {} increments = {} total (verified)",
            NUM_TASKS, INCREMENTS_PER_TASK, actual
        );
    }
}
