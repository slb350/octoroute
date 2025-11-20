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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Rule-based routing
    Rule,
    /// LLM-powered routing
    Llm,
    /// Hybrid routing (rules + LLM fallback)
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
    /// Maximum possible label combinations: 3 tiers × 3 strategies = 9 time series.
    pub fn record_request(&self, tier: Tier, strategy: Strategy) -> Result<(), prometheus::Error> {
        self.requests_total
            .get_metric_with_label_values(&[tier.as_str(), strategy.as_str()])?
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
    /// Maximum possible label values: 3 strategies = 3 time series.
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

        self.routing_duration
            .get_metric_with_label_values(&[strategy.as_str()])?
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

        metrics
            .record_request(Tier::Deep, Strategy::Hybrid)
            .unwrap();
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
            .record_request(Tier::Fast, Strategy::Hybrid)
            .unwrap();
        metrics
            .record_request(Tier::Balanced, Strategy::Rule)
            .unwrap();
        metrics
            .record_request(Tier::Balanced, Strategy::Llm)
            .unwrap();
        metrics
            .record_request(Tier::Deep, Strategy::Hybrid)
            .unwrap();

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
        // Now with bounded cardinality (max 9 combinations for 3x3 enums)
        for i in 0..20 {
            let m = Arc::clone(&metrics);
            let handle = thread::spawn(move || {
                // Rotate through valid tier and strategy values using enums
                let tiers = [Tier::Fast, Tier::Balanced, Tier::Deep];
                let strategies = [Strategy::Rule, Strategy::Llm, Strategy::Hybrid];

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
        metrics
            .record_request(Tier::Deep, Strategy::Hybrid)
            .unwrap();

        metrics
            .record_routing_duration(Strategy::Rule, 1.0)
            .unwrap();
        metrics
            .record_routing_duration(Strategy::Llm, 250.0)
            .unwrap();

        metrics.record_model_invocation(Tier::Fast).unwrap();
        metrics.record_model_invocation(Tier::Balanced).unwrap();

        let output = metrics.gather().unwrap();

        // Verify correct label values appear
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("tier=\"balanced\""));
        assert!(output.contains("tier=\"deep\""));
        assert!(output.contains("strategy=\"rule\""));
        assert!(output.contains("strategy=\"llm\""));
        assert!(output.contains("strategy=\"hybrid\""));
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
        // Maximum possible cardinality: 3 tiers × 3 strategies = 9 combinations
        // vs unbounded with strings

        let output = metrics.gather().unwrap();

        // Only one "fast" variant exists
        let fast_count = output.matches("tier=\"fast\"").count();
        assert!(fast_count > 0, "Should have at least one fast tier metric");

        // NO "FAST", "Fast", "fasst", etc.
        assert!(!output.contains("tier=\"FAST\""));
        assert!(!output.contains("tier=\"Fast\""));
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
}
