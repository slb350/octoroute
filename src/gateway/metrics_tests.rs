use super::{
    config::SemanticRoutingMode,
    metrics::{FailurePhase, GatewayMetrics, SemanticDecisionOutcome, UpstreamLabel},
    routing::{RouteDestination, RouteReason},
};
use axum::http::StatusCode;
use std::time::Duration;

#[test]
fn gateway_metrics_use_only_bounded_route_and_failure_labels() {
    let metrics = GatewayMetrics::new().expect("metrics registry");

    metrics
        .record_route(RouteDestination::Cloud, RouteReason::LocalEarlyFailure)
        .expect("route metric");
    metrics.record_fallback();
    metrics
        .record_semantic_decision(SemanticRoutingMode::Shadow, SemanticDecisionOutcome::Cloud)
        .expect("semantic decision metric");
    metrics
        .record_upstream_failure(UpstreamLabel::Local, FailurePhase::PreCommit)
        .expect("failure metric");
    metrics
        .record_upstream_response(UpstreamLabel::OpenRouter, StatusCode::OK)
        .expect("upstream response");
    metrics
        .record_upstream_transport_failure(UpstreamLabel::Local)
        .expect("upstream transport failure");
    metrics.record_local_busy_spillover();
    metrics
        .record_time_to_first_byte(RouteDestination::Cloud, Duration::from_millis(25))
        .expect("first-byte timing");
    metrics.record_routing_duration(Duration::from_millis(2));
    {
        let _observation = metrics
            .start_response(RouteDestination::Cloud)
            .expect("response observation");
    }

    let encoded = metrics.encode().expect("Prometheus encoding");
    assert!(encoded.contains(
        "octoroute_route_decisions_total{destination=\"cloud\",reason=\"local_early_failure\"} 1"
    ));
    assert!(encoded.contains("octoroute_local_fallbacks_total 1"));
    assert!(
        encoded.contains("octoroute_semantic_decisions_total{mode=\"shadow\",outcome=\"cloud\"} 1")
    );
    assert!(
        encoded.contains(
            "octoroute_upstream_failures_total{phase=\"pre_commit\",upstream=\"local\"} 1"
        )
    );
    assert!(encoded.contains(
        "octoroute_upstream_requests_total{outcome=\"response\",status_class=\"2xx\",upstream=\"openrouter\"} 1"
    ));
    assert!(encoded.contains(
        "octoroute_upstream_requests_total{outcome=\"transport_failure\",status_class=\"none\",upstream=\"local\"} 1"
    ));
    assert!(encoded.contains("octoroute_local_busy_spillovers_total 1"));
    assert!(
        encoded.contains("octoroute_time_to_first_byte_seconds_count{destination=\"cloud\"} 1")
    );
    assert!(encoded.contains("octoroute_routing_duration_seconds_count 1"));
    assert!(encoded.contains("octoroute_request_duration_seconds_count{destination=\"cloud\"} 1"));
    assert!(encoded.contains("octoroute_in_flight_requests{destination=\"cloud\"} 0"));
}
