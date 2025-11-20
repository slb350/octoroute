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
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        encoder.encode(&metric_families, &mut buffer)?;

        String::from_utf8(buffer).map_err(|e| {
            prometheus::Error::Msg(format!("Failed to convert metrics to UTF-8: {}", e))
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
}
