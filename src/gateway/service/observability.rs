use super::GatewayService;
use crate::gateway::{
    metrics::{FailurePhase, UpstreamLabel},
    routing::RouteDestination,
    transport::UpstreamTransport,
};
use axum::http::StatusCode;
use std::time::Instant;

impl<T> GatewayService<T>
where
    T: UpstreamTransport,
{
    pub(super) fn record_upstream_response(&self, upstream: UpstreamLabel, status: StatusCode) {
        if let Err(error) = self.metrics.record_upstream_response(upstream, status) {
            tracing::warn!(%error, "failed to record gateway upstream response metric");
        }
    }

    pub(super) fn record_upstream_transport_failure(&self, upstream: UpstreamLabel) {
        if let Err(error) = self.metrics.record_upstream_transport_failure(upstream) {
            tracing::warn!(%error, "failed to record gateway upstream attempt metric");
        }
    }

    pub(super) fn record_time_to_first_byte(
        &self,
        destination: RouteDestination,
        started: Instant,
    ) {
        if let Err(error) = self
            .metrics
            .record_time_to_first_byte(destination, started.elapsed())
        {
            tracing::warn!(%error, "failed to record gateway first-byte metric");
        }
    }

    pub(super) fn record_routing_duration(&self, started: Instant) {
        self.metrics.record_routing_duration(started.elapsed());
    }

    pub(super) fn record_upstream_failure(&self, upstream: UpstreamLabel) {
        if let Err(error) = self
            .metrics
            .record_upstream_failure(upstream, FailurePhase::PreCommit)
        {
            tracing::warn!(%error, "failed to record gateway upstream failure metric");
        }
    }
}
