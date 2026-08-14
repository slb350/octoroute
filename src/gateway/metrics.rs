//! Bounded-cardinality Prometheus metrics for the v2 gateway.

use crate::gateway::{
    config::SemanticRoutingMode,
    intelligence::SemanticBoundary,
    routing::{RouteDestination, RouteReason},
};
use axum::http::StatusCode;
use prometheus::{
    Encoder, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge,
    IntGaugeVec, Opts, Registry, TextEncoder,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Stable upstream metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamLabel {
    /// Configured llama.cpp service.
    Local,
    /// OpenRouter cloud gateway.
    OpenRouter,
}

impl UpstreamLabel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::OpenRouter => "openrouter",
        }
    }
}

/// Stable failure phase metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePhase {
    /// Failure occurred before a body byte became client-visible.
    PreCommit,
    /// Failure occurred after response commitment.
    MidStream,
}

/// Stable semantic classifier outcome metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticDecisionOutcome {
    Local,
    Cloud,
    Failure,
}

impl SemanticDecisionOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
            Self::Failure => "failure",
        }
    }
}

impl From<RouteDestination> for SemanticDecisionOutcome {
    fn from(destination: RouteDestination) -> Self {
        match destination {
            RouteDestination::Local => Self::Local,
            RouteDestination::Cloud => Self::Cloud,
        }
    }
}

impl FailurePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::PreCommit => "pre_commit",
            Self::MidStream => "mid_stream",
        }
    }
}

/// Independent v2 metric registry.
#[derive(Clone)]
pub struct GatewayMetrics {
    registry: Arc<Registry>,
    route_decisions: IntCounterVec,
    semantic_decisions: IntCounterVec,
    semantic_local_success_probability: HistogramVec,
    local_fallbacks: IntCounter,
    local_busy_spillovers: IntCounter,
    upstream_requests: IntCounterVec,
    upstream_failures: IntCounterVec,
    request_duration: HistogramVec,
    time_to_first_byte: HistogramVec,
    routing_duration: Histogram,
    in_flight_requests: IntGaugeVec,
}

/// Response-lifetime observation which records completion or cancellation.
pub struct ResponseObservation {
    in_flight: IntGauge,
    duration: Histogram,
    started: Instant,
}

impl Drop for ResponseObservation {
    fn drop(&mut self) {
        self.duration.observe(self.started.elapsed().as_secs_f64());
        self.in_flight.dec();
    }
}

impl GatewayMetrics {
    /// Register the complete fixed v2 metric set.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let route_decisions = IntCounterVec::new(
            Opts::new(
                "octoroute_route_decisions_total",
                "Completed gateway routes by actual destination and bounded reason",
            ),
            &["destination", "reason"],
        )?;
        let local_fallbacks = IntCounter::with_opts(Opts::new(
            "octoroute_local_fallbacks_total",
            "Automatic local attempts replaced by cloud before response commitment",
        ))?;
        let semantic_decisions = IntCounterVec::new(
            Opts::new(
                "octoroute_semantic_decisions_total",
                "Semantic classifier observations by configured mode and bounded outcome",
            ),
            &["mode", "outcome"],
        )?;
        let semantic_local_success_probability = HistogramVec::new(
            HistogramOpts::new(
                "octoroute_semantic_local_success_probability",
                "Validated local-success forecasts by configured mode and capability boundary",
            )
            .buckets(vec![0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]),
            &["mode", "boundary"],
        )?;
        let local_busy_spillovers = IntCounter::with_opts(Opts::new(
            "octoroute_local_busy_spillovers_total",
            "Automatic requests sent to cloud because local capacity was occupied",
        ))?;
        let upstream_requests = IntCounterVec::new(
            Opts::new(
                "octoroute_upstream_requests_total",
                "Upstream attempts by bounded upstream, outcome, and status class",
            ),
            &["upstream", "outcome", "status_class"],
        )?;
        let upstream_failures = IntCounterVec::new(
            Opts::new(
                "octoroute_upstream_failures_total",
                "Upstream transport failures by bounded upstream and phase",
            ),
            &["upstream", "phase"],
        )?;
        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "octoroute_request_duration_seconds",
                "Response body lifetime by actual destination",
            ),
            &["destination"],
        )?;
        let time_to_first_byte = HistogramVec::new(
            HistogramOpts::new(
                "octoroute_time_to_first_byte_seconds",
                "Dispatch through first upstream body byte by actual destination",
            ),
            &["destination"],
        )?;
        let routing_duration = Histogram::with_opts(HistogramOpts::new(
            "octoroute_routing_duration_seconds",
            "Request parsing, policy, and local admission duration",
        ))?;
        let in_flight_requests = IntGaugeVec::new(
            Opts::new(
                "octoroute_in_flight_requests",
                "Committed responses whose bodies remain active",
            ),
            &["destination"],
        )?;
        registry.register(Box::new(route_decisions.clone()))?;
        registry.register(Box::new(semantic_decisions.clone()))?;
        registry.register(Box::new(semantic_local_success_probability.clone()))?;
        registry.register(Box::new(local_fallbacks.clone()))?;
        registry.register(Box::new(local_busy_spillovers.clone()))?;
        registry.register(Box::new(upstream_requests.clone()))?;
        registry.register(Box::new(upstream_failures.clone()))?;
        registry.register(Box::new(request_duration.clone()))?;
        registry.register(Box::new(time_to_first_byte.clone()))?;
        registry.register(Box::new(routing_duration.clone()))?;
        registry.register(Box::new(in_flight_requests.clone()))?;

        Ok(Self {
            registry: Arc::new(registry),
            route_decisions,
            semantic_decisions,
            semantic_local_success_probability,
            local_fallbacks,
            local_busy_spillovers,
            upstream_requests,
            upstream_failures,
            request_duration,
            time_to_first_byte,
            routing_duration,
            in_flight_requests,
        })
    }

    /// Record the actual upstream destination returned to the client.
    pub fn record_route(
        &self,
        destination: RouteDestination,
        reason: RouteReason,
    ) -> Result<(), prometheus::Error> {
        self.route_decisions
            .get_metric_with_label_values(&[destination.as_str(), reason.as_str()])?
            .inc();
        Ok(())
    }

    /// Record one bounded semantic classifier observation.
    pub(crate) fn record_semantic_decision(
        &self,
        mode: SemanticRoutingMode,
        outcome: SemanticDecisionOutcome,
    ) -> Result<(), prometheus::Error> {
        self.semantic_decisions
            .get_metric_with_label_values(&[mode.as_str(), outcome.as_str()])?
            .inc();
        Ok(())
    }

    /// Record one validated, bounded semantic success forecast.
    pub(crate) fn record_semantic_forecast(
        &self,
        mode: SemanticRoutingMode,
        boundary: SemanticBoundary,
        local_success_probability: f64,
    ) -> Result<(), prometheus::Error> {
        self.semantic_local_success_probability
            .get_metric_with_label_values(&[mode.as_str(), boundary.as_str()])?
            .observe(local_success_probability);
        Ok(())
    }

    /// Record a pre-commit local-to-cloud fallback.
    pub fn record_fallback(&self) {
        self.local_fallbacks.inc();
    }

    /// Record an automatic busy-local route to cloud.
    pub fn record_local_busy_spillover(&self) {
        self.local_busy_spillovers.inc();
    }

    /// Record an upstream HTTP response before client commitment.
    pub fn record_upstream_response(
        &self,
        upstream: UpstreamLabel,
        status: StatusCode,
    ) -> Result<(), prometheus::Error> {
        self.upstream_requests
            .get_metric_with_label_values(&[upstream.as_str(), "response", status_class(status)])?
            .inc();
        Ok(())
    }

    /// Record an upstream attempt that failed before HTTP response status.
    pub fn record_upstream_transport_failure(
        &self,
        upstream: UpstreamLabel,
    ) -> Result<(), prometheus::Error> {
        self.upstream_requests
            .get_metric_with_label_values(&[upstream.as_str(), "transport_failure", "none"])?
            .inc();
        Ok(())
    }

    /// Record an upstream transport failure.
    pub fn record_upstream_failure(
        &self,
        upstream: UpstreamLabel,
        phase: FailurePhase,
    ) -> Result<(), prometheus::Error> {
        self.failure_counter(upstream, phase)?.inc();
        Ok(())
    }

    /// Resolve one pre-bound failure counter for a response stream.
    pub fn failure_counter(
        &self,
        upstream: UpstreamLabel,
        phase: FailurePhase,
    ) -> Result<IntCounter, prometheus::Error> {
        self.upstream_failures
            .get_metric_with_label_values(&[upstream.as_str(), phase.as_str()])
    }

    /// Observe dispatch through the first upstream response body byte.
    pub fn record_time_to_first_byte(
        &self,
        destination: RouteDestination,
        duration: Duration,
    ) -> Result<(), prometheus::Error> {
        self.time_to_first_byte
            .get_metric_with_label_values(&[destination.as_str()])?
            .observe(duration.as_secs_f64());
        Ok(())
    }

    /// Observe request parsing, route policy, and local admission.
    pub fn record_routing_duration(&self, duration: Duration) {
        self.routing_duration.observe(duration.as_secs_f64());
    }

    /// Start actual-destination response lifetime and in-flight observation.
    pub fn start_response(
        &self,
        destination: RouteDestination,
    ) -> Result<ResponseObservation, prometheus::Error> {
        let destination = destination.as_str();
        let in_flight = self
            .in_flight_requests
            .get_metric_with_label_values(&[destination])?;
        let duration = self
            .request_duration
            .get_metric_with_label_values(&[destination])?;
        in_flight.inc();
        Ok(ResponseObservation {
            in_flight,
            duration,
            started: Instant::now(),
        })
    }

    /// Encode the registry in Prometheus text exposition format.
    pub fn encode(&self) -> Result<String, prometheus::Error> {
        let encoder = TextEncoder::new();
        let mut bytes = Vec::new();
        encoder.encode(&self.registry.gather(), &mut bytes)?;
        String::from_utf8(bytes)
            .map_err(|error| prometheus::Error::Msg(format!("metrics were not UTF-8: {error}")))
    }
}

fn status_class(status: StatusCode) -> &'static str {
    match status.as_u16() / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "other",
    }
}
