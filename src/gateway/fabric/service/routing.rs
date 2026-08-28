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

/// What one route step decided.
///
/// `FallForward` is the only way to reach the next step, so every path that
/// does not commit a response to the client is visible as one variant here
/// rather than as a `continue` buried several levels down.
enum StepOutcome {
    Committed(Box<Response<Body>>),
    FallForward,
}

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
            let outcome = match step {
                RouteTarget::LocalPool(pool) => {
                    self.local_step(request, plan, pool, has_more, request_id)
                        .await
                }
                RouteTarget::Provider(provider) => {
                    self.provider_step(request, plan, provider, has_more, request_id)
                        .await
                }
            };
            match outcome {
                StepOutcome::Committed(response) => return *response,
                StepOutcome::FallForward => continue,
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

    /// Admit and dispatch one local pool step.
    async fn local_step(
        &self,
        request: &GatewayRequest,
        plan: &RoutePlan,
        pool_name: &str,
        has_more: bool,
        request_id: &str,
    ) -> StepOutcome {
        // Restarted per step. One observation is the admission work for one
        // destination - health, slot, and token-count probes, credential
        // resolution, body construction - up to the moment its lease is held. A
        // request that falls forward records one observation per step it
        // reaches, and none of them include the upstream call that caused the
        // fall-forward.
        let routing_started = Instant::now();
        let outcome = match self.pools.get(pool_name) {
            Some(pool) => match pool.try_admit(request).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    return commit(error_response(
                        StatusCode::BAD_REQUEST,
                        &error.to_string(),
                        "invalid_request_error",
                        "invalid_token_budget",
                        request_id,
                    ));
                }
            },
            None => PoolAdmissionOutcome::Rejected(PoolAdmissionState::Disabled),
        };

        let lease = match outcome {
            PoolAdmissionOutcome::Admitted(lease) => lease,
            PoolAdmissionOutcome::Rejected(state) => {
                self.metrics.record_pool_rejected(pool_name, state);
                if let Some(trigger) = fallback_trigger(state)
                    .filter(|trigger| plan.may_fall_forward(has_more, *trigger))
                {
                    self.metrics.record_pool_fallback(pool_name, trigger);
                    tracing::warn!(
                        request_id,
                        route = plan.model.as_str(),
                        pool = pool_name,
                        state = pool_state_label(state),
                        trigger = fallback_trigger_label(trigger),
                        "v3 local pool rejected the request; spilling to the next route step"
                    );
                    return StepOutcome::FallForward;
                }
                return commit(pool_state_error(state, pool_name, request_id));
            }
        };

        self.metrics.record_pool_admitted(pool_name);
        self.metrics
            .record_routing_latency(routing_started.elapsed());
        let pool = lease.pool().to_string();
        let member = lease.member().to_string();
        let revision = lease.model_revision().to_string();

        match self.transport.local(*lease).await {
            Ok(response)
                if response.status().is_server_error()
                    && plan.may_fall_forward(has_more, FallbackTrigger::PrecommitFailure) =>
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
                StepOutcome::FallForward
            }
            Ok(response) => commit(decorate_local(
                response, plan, &pool, &member, &revision, request_id,
            )),
            Err(error) if plan.may_fall_forward(has_more, FallbackTrigger::PrecommitFailure) => {
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    pool,
                    member,
                    %error,
                    "v3 local transport failed before commitment; trying next route step"
                );
                StepOutcome::FallForward
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
                commit(error_response(
                    StatusCode::BAD_GATEWAY,
                    "local upstream failed before response commitment",
                    "upstream_error",
                    "local_upstream_error",
                    request_id,
                ))
            }
        }
    }

    /// Admit and dispatch one provider step.
    async fn provider_step(
        &self,
        request: &GatewayRequest,
        plan: &RoutePlan,
        provider_name: &str,
        has_more: bool,
        request_id: &str,
    ) -> StepOutcome {
        let routing_started = Instant::now();
        let outcome = match self
            .providers
            .try_admit(provider_name, request, plan.default_reasoning_effort)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                return commit(error_response(
                    StatusCode::BAD_REQUEST,
                    &error.to_string(),
                    "invalid_request_error",
                    "provider_request_invalid",
                    request_id,
                ));
            }
        };

        let lease = match outcome {
            ProviderAdmissionOutcome::Admitted(lease) => lease,
            ProviderAdmissionOutcome::Rejected(state) => {
                self.metrics.record_rejected(provider_name, state);
                if let Some(trigger) = provider_fallback_trigger(state)
                    .filter(|trigger| plan.may_fall_forward(has_more, *trigger))
                {
                    self.metrics.record_fallback(provider_name, trigger);
                    tracing::warn!(
                        request_id,
                        route = plan.model.as_str(),
                        provider = provider_name,
                        trigger = fallback_trigger_label(trigger),
                        "v3 provider rejected the request; spilling to the next route step"
                    );
                    return StepOutcome::FallForward;
                }
                return commit(provider_state_error(state, provider_name, request_id));
            }
        };

        let provider = lease.provider().to_string();
        let model = lease.model().to_string();
        self.metrics.record_admitted(&provider);
        self.metrics
            .record_routing_latency(routing_started.elapsed());

        match self.transport.provider(*lease).await {
            Ok(response)
                if response.status() == StatusCode::TOO_MANY_REQUESTS
                    && plan.may_fall_forward(has_more, FallbackTrigger::RateLimited) =>
            {
                self.metrics
                    .record_response(&provider, ProviderResponseOutcome::RateLimited);
                self.metrics
                    .record_fallback(&provider, FallbackTrigger::RateLimited);
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    provider,
                    "provider rate limited before commitment; trying next route step"
                );
                drop(response);
                StepOutcome::FallForward
            }
            Ok(response)
                if response.status().is_server_error()
                    && plan.may_fall_forward(has_more, FallbackTrigger::PrecommitFailure) =>
            {
                let status = response.status();
                self.metrics
                    .record_response(&provider, ProviderResponseOutcome::ServerError);
                self.metrics
                    .record_fallback(&provider, FallbackTrigger::PrecommitFailure);
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
                StepOutcome::FallForward
            }
            Ok(response) => {
                let status = response.status();
                self.metrics
                    .record_response(&provider, provider_response_outcome(status));
                // A committed response can still prove cached readiness wrong.
                if status.is_server_error()
                    || matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
                {
                    self.providers
                        .record_dispatch_failure(&provider, Some(status))
                        .await;
                }
                commit(decorate_provider(
                    response, plan, &provider, &model, request_id,
                ))
            }
            Err(error) => {
                self.metrics
                    .record_response(&provider, ProviderResponseOutcome::TransportError);
                self.providers
                    .record_dispatch_failure(&provider, None)
                    .await;
                if plan.may_fall_forward(has_more, FallbackTrigger::PrecommitFailure) {
                    self.metrics
                        .record_fallback(&provider, FallbackTrigger::PrecommitFailure);
                    tracing::warn!(
                        request_id,
                        route = plan.model.as_str(),
                        provider,
                        %error,
                        "provider transport failed before commitment; trying next route step"
                    );
                    return StepOutcome::FallForward;
                }
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    provider,
                    %error,
                    "provider transport failed before commitment"
                );
                commit(error_response(
                    StatusCode::BAD_GATEWAY,
                    "provider failed before response commitment",
                    "upstream_error",
                    "provider_upstream_error",
                    request_id,
                ))
            }
        }
    }
}

fn commit(response: Response<Body>) -> StepOutcome {
    StepOutcome::Committed(Box::new(response))
}
