//! Route execution: walk the plan's steps and stop at the first commitment.

use super::responses::{
    decorate_local, decorate_provider, fallback_trigger, pool_state_error,
    provider_fallback_trigger, provider_response_outcome, provider_state_error,
};
use super::{FabricGatewayService, FabricUpstreamTransport};
use crate::gateway::fabric::http_support::error_response;
use crate::gateway::fabric::metrics::{
    ProviderResponseOutcome, fallback_trigger_label, pool_state_label,
};
use crate::gateway::fabric::{
    FallbackTrigger, PoolAdmissionOutcome, PoolAdmissionState, ProviderAdmissionOutcome, RoutePlan,
    RouteTarget,
};
use crate::gateway::request::GatewayRequest;
use axum::{
    body::Body,
    http::{Response, StatusCode},
};
use std::time::Instant;

impl<T> FabricGatewayService<T>
where
    T: FabricUpstreamTransport,
{
    pub(super) async fn dispatch_route(
        &self,
        request: &GatewayRequest,
        plan: &RoutePlan,
        request_id: &str,
    ) -> Response<Body> {
        for (index, step) in plan.steps.iter().enumerate() {
            let has_more = index + 1 < plan.steps.len();
            // Restarted per step. One observation is the admission work for one
            // destination - health, slot, and token-count probes, credential
            // resolution, body construction - up to the moment its lease is
            // held. A request that falls forward records one observation per
            // step it reaches, and none of them include the upstream call that
            // caused the fall-forward.
            let routing_started = Instant::now();
            match step {
                RouteTarget::LocalPool(pool_name) => {
                    let outcome = match self.pools.get(pool_name) {
                        Some(pool) => match pool.try_admit(request).await {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                return error_response(
                                    StatusCode::BAD_REQUEST,
                                    &error.to_string(),
                                    "invalid_request_error",
                                    "invalid_token_budget",
                                    request_id,
                                );
                            }
                        },
                        None => PoolAdmissionOutcome::Rejected(PoolAdmissionState::Disabled),
                    };

                    match outcome {
                        PoolAdmissionOutcome::Admitted(lease) => {
                            self.metrics.record_pool_admitted(pool_name);
                            self.metrics
                                .record_routing_latency(routing_started.elapsed());
                            let pool = lease.pool().to_string();
                            let member = lease.member().to_string();
                            let revision = lease.model_revision().to_string();
                            match self.transport.local(*lease).await {
                                Ok(response)
                                    if response.status().is_server_error()
                                        && has_more
                                        && plan
                                            .fallback_on
                                            .contains(&FallbackTrigger::PrecommitFailure) =>
                                {
                                    tracing::warn!(
                                        request_id,
                                        route = plan.model.as_str(),
                                        pool,
                                        member,
                                        status = response.status().as_u16(),
                                        "v3 local target failed before commitment; trying next route step"
                                    );
                                    drop(response);
                                    continue;
                                }
                                Ok(response) => {
                                    return decorate_local(
                                        response, plan, &pool, &member, &revision, request_id,
                                    );
                                }
                                Err(error)
                                    if has_more
                                        && plan
                                            .fallback_on
                                            .contains(&FallbackTrigger::PrecommitFailure) =>
                                {
                                    tracing::warn!(
                                        request_id,
                                        route = plan.model.as_str(),
                                        pool,
                                        member,
                                        %error,
                                        "v3 local transport failed before commitment; trying next route step"
                                    );
                                    continue;
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        request_id,
                                        route = plan.model.as_str(),
                                        pool,
                                        member,
                                        %error,
                                        "v3 local transport failed before commitment"
                                    );
                                    return error_response(
                                        StatusCode::BAD_GATEWAY,
                                        "local upstream failed before response commitment",
                                        "upstream_error",
                                        "local_upstream_error",
                                        request_id,
                                    );
                                }
                            }
                        }
                        PoolAdmissionOutcome::Rejected(state) => {
                            self.metrics.record_pool_rejected(pool_name, state);
                            let trigger = fallback_trigger(state);
                            if has_more
                                && let Some(trigger) = trigger
                                && plan.fallback_on.contains(&trigger)
                            {
                                self.metrics.record_pool_fallback(pool_name, trigger);
                                tracing::warn!(
                                    request_id,
                                    route = plan.model.as_str(),
                                    pool = pool_name.as_str(),
                                    state = pool_state_label(state),
                                    trigger = fallback_trigger_label(trigger),
                                    "v3 local pool rejected the request; spilling to the next route step"
                                );
                                continue;
                            }
                            return pool_state_error(state, pool_name, request_id);
                        }
                    }
                }
                RouteTarget::Provider(provider_name) => {
                    let outcome = match self
                        .providers
                        .try_admit(provider_name, request, plan.default_reasoning_effort)
                        .await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            return error_response(
                                StatusCode::BAD_REQUEST,
                                &error.to_string(),
                                "invalid_request_error",
                                "provider_request_invalid",
                                request_id,
                            );
                        }
                    };
                    match outcome {
                        ProviderAdmissionOutcome::Admitted(lease) => {
                            let provider = lease.provider().to_string();
                            let model = lease.model().to_string();
                            self.metrics.record_admitted(&provider);
                            self.metrics
                                .record_routing_latency(routing_started.elapsed());
                            match self.transport.provider(*lease).await {
                                Ok(response)
                                    if response.status() == StatusCode::TOO_MANY_REQUESTS
                                        && has_more
                                        && plan
                                            .fallback_on
                                            .contains(&FallbackTrigger::RateLimited) =>
                                {
                                    self.metrics.record_response(
                                        &provider,
                                        ProviderResponseOutcome::RateLimited,
                                    );
                                    self.metrics
                                        .record_fallback(&provider, FallbackTrigger::RateLimited);
                                    tracing::warn!(
                                        request_id,
                                        route = plan.model.as_str(),
                                        provider,
                                        "provider rate limited before commitment; trying next route step"
                                    );
                                    drop(response);
                                    continue;
                                }
                                Ok(response)
                                    if response.status().is_server_error()
                                        && has_more
                                        && plan
                                            .fallback_on
                                            .contains(&FallbackTrigger::PrecommitFailure) =>
                                {
                                    self.metrics.record_response(
                                        &provider,
                                        ProviderResponseOutcome::ServerError,
                                    );
                                    self.metrics.record_fallback(
                                        &provider,
                                        FallbackTrigger::PrecommitFailure,
                                    );
                                    let status = response.status();
                                    self.providers
                                        .record_dispatch_failure(&provider, Some(status))
                                        .await;
                                    tracing::warn!(
                                        request_id,
                                        route = plan.model.as_str(),
                                        provider,
                                        status = status.as_u16(),
                                        "provider failed before commitment; trying next route step"
                                    );
                                    drop(response);
                                    continue;
                                }
                                Ok(response) => {
                                    let status = response.status();
                                    self.metrics.record_response(
                                        &provider,
                                        provider_response_outcome(status),
                                    );
                                    if status.is_server_error()
                                        || matches!(
                                            status,
                                            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                                        )
                                    {
                                        self.providers
                                            .record_dispatch_failure(&provider, Some(status))
                                            .await;
                                    }
                                    return decorate_provider(
                                        response, plan, &provider, &model, request_id,
                                    );
                                }
                                Err(error)
                                    if has_more
                                        && plan
                                            .fallback_on
                                            .contains(&FallbackTrigger::PrecommitFailure) =>
                                {
                                    self.metrics.record_response(
                                        &provider,
                                        ProviderResponseOutcome::TransportError,
                                    );
                                    self.providers
                                        .record_dispatch_failure(&provider, None)
                                        .await;
                                    self.metrics.record_fallback(
                                        &provider,
                                        FallbackTrigger::PrecommitFailure,
                                    );
                                    tracing::warn!(
                                        request_id,
                                        route = plan.model.as_str(),
                                        provider,
                                        %error,
                                        "provider transport failed before commitment; trying next route step"
                                    );
                                    continue;
                                }
                                Err(error) => {
                                    self.metrics.record_response(
                                        &provider,
                                        ProviderResponseOutcome::TransportError,
                                    );
                                    self.providers
                                        .record_dispatch_failure(&provider, None)
                                        .await;
                                    tracing::warn!(
                                        request_id,
                                        route = plan.model.as_str(),
                                        provider,
                                        %error,
                                        "provider transport failed before commitment"
                                    );
                                    return error_response(
                                        StatusCode::BAD_GATEWAY,
                                        "provider failed before response commitment",
                                        "upstream_error",
                                        "provider_upstream_error",
                                        request_id,
                                    );
                                }
                            }
                        }
                        ProviderAdmissionOutcome::Rejected(state) => {
                            self.metrics.record_rejected(provider_name, state);
                            if let Some(trigger) = provider_fallback_trigger(state)
                                .filter(|trigger| has_more && plan.fallback_on.contains(trigger))
                            {
                                self.metrics.record_fallback(provider_name, trigger);
                                continue;
                            }
                            return provider_state_error(state, provider_name, request_id);
                        }
                    }
                }
            }
        }

        error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "no eligible route target is available",
            "upstream_error",
            "no_eligible_target",
            request_id,
        )
    }
}
