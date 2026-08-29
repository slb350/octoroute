//! Route execution: walk the plan's steps and stop at the first commitment.

use super::responses::{
    decorate_local, decorate_provider, fallback_trigger, local_credential_rejected,
    pool_state_error, provider_credential_rejected, provider_fallback_trigger,
    provider_request_error, provider_response_outcome, provider_state_error,
};
use super::{FabricGatewayService, FabricUpstreamTransport};
use crate::gateway::fabric::http_support::error_response;
use crate::gateway::fabric::metrics::{
    ProviderResponseOutcome, fallback_trigger_label, pool_state_label,
};
use crate::gateway::fabric::{
    FallbackTrigger, PoolAdmissionOutcome, PoolAdmissionState, ProviderAdmissionOutcome,
    ProviderAdmissionState, RoutePlan, RouteTarget, provider::is_upstream_credential_rejection,
};
use crate::gateway::request::GatewayRequest;
use axum::{
    body::Body,
    http::{Response, StatusCode},
};
use std::time::Instant;

/// How a route step refused at admission.
///
/// A route that runs out of steps has collected one of these per step it
/// reached, and only one can be committed. Neither "first" nor "last" is the
/// answer: the executor keeps the most significant rejection, with the first
/// seen retained within a tier.
#[derive(Debug, Clone, Copy)]
enum StepRejection {
    Pool(PoolAdmissionState),
    Provider(ProviderAdmissionState),
}

/// An operator's credential is missing, expired, or refused.
const OPERATOR_CREDENTIAL: u8 = 0;
/// The caller can fix the request; retrying it unchanged never succeeds.
const CLIENT_TERMINAL: u8 = 1;
/// Capacity or health: the same request may well succeed later.
const CAPACITY: u8 = 2;
/// An admission state that should not have reached the executor at all.
const INCONSISTENT: u8 = 3;

impl StepRejection {
    fn into_response(self, request_id: &str) -> Response<Body> {
        match self {
            Self::Pool(state) => pool_state_error(state, request_id),
            Self::Provider(state) => provider_state_error(state, request_id),
        }
    }

    fn governing(current: Option<Self>, next: Self) -> Self {
        match current {
            Some(current) if current.rank() <= next.rank() => current,
            _ => next,
        }
    }

    /// Significance, lowest first.
    fn rank(self) -> u8 {
        match self {
            Self::Pool(state) => match state {
                PoolAdmissionState::Unauthenticated => OPERATOR_CREDENTIAL,
                PoolAdmissionState::Incompatible | PoolAdmissionState::ContextOverflow => {
                    CLIENT_TERMINAL
                }
                PoolAdmissionState::Disabled
                | PoolAdmissionState::Unhealthy
                | PoolAdmissionState::TokenCountUnavailable
                | PoolAdmissionState::Busy => CAPACITY,
                PoolAdmissionState::Ready => INCONSISTENT,
            },
            Self::Provider(state) => match state {
                ProviderAdmissionState::Unauthenticated => OPERATOR_CREDENTIAL,
                ProviderAdmissionState::Incompatible => CLIENT_TERMINAL,
                ProviderAdmissionState::Disabled
                | ProviderAdmissionState::Unavailable
                | ProviderAdmissionState::Busy => CAPACITY,
                ProviderAdmissionState::Ready => INCONSISTENT,
            },
        }
    }
}

/// What one route step decided.
///
/// `FallForward` is the only way to reach the next step, so every path that
/// does not commit a response to the client is visible as one variant here
/// rather than as a `continue` buried several levels down. It carries the
/// admission rejection that caused it, when there was one, so a later terminal
/// error can name the rejection that governed the route.
enum StepOutcome {
    Committed(Box<Response<Body>>),
    FallForward(Option<StepRejection>),
    /// Admission refused and the route may not spill: the executor renders it.
    Rejected(StepRejection),
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
        // Steps that fall forward still carry a rejection, and the terminal
        // error reports the most significant one the route collected rather
        // than the first or the last. See `StepRejection::governing`.
        let mut governing: Option<StepRejection> = None;
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
                StepOutcome::FallForward(rejection) => {
                    if let Some(rejection) = rejection {
                        governing = Some(StepRejection::governing(governing, rejection));
                    }
                }
                StepOutcome::Rejected(rejection) => {
                    return StepRejection::governing(governing, rejection)
                        .into_response(request_id);
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
        // resolution, body construction - up to the moment its lease is held or
        // the step is refused. A rejected step does that same probe work, so it
        // is recorded too: a histogram that covered admissions only would go
        // quiet during the outage an operator is trying to read.
        let routing_started = Instant::now();
        let admission = match self.pools.get(pool_name) {
            Some(pool) => pool.try_admit(request).await,
            None => Ok(PoolAdmissionOutcome::Rejected(PoolAdmissionState::Disabled)),
        };
        self.metrics
            .record_routing_latency(routing_started.elapsed());
        let outcome = match admission {
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
        };

        let lease = match outcome {
            PoolAdmissionOutcome::Admitted(lease) => lease,
            PoolAdmissionOutcome::Rejected(state) => {
                self.metrics.record_pool_rejected(pool_name, state);
                let rejection = StepRejection::Pool(state);
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
                    return StepOutcome::FallForward(Some(rejection));
                }
                tracing::info!(
                    request_id,
                    route = plan.model.as_str(),
                    pool = pool_name,
                    state = pool_state_label(state),
                    "v3 local route could not be admitted"
                );
                return StepOutcome::Rejected(rejection);
            }
        };

        self.metrics.record_pool_admitted(pool_name);
        let pool = lease.pool().to_string();
        let member = lease.member().to_string();
        let revision = lease.model_revision().to_string();

        match self.transport.local(*lease).await {
            Ok(response)
                if is_upstream_credential_rejection(response.status())
                    && plan.may_fall_forward(has_more, FallbackTrigger::Unauthenticated) =>
            {
                self.metrics
                    .record_pool_fallback(pool_name, FallbackTrigger::Unauthenticated);
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    pool,
                    member,
                    status = response.status().as_u16(),
                    "v3 local target rejected the gateway credential; trying next route step"
                );
                drop(response);
                StepOutcome::FallForward(None)
            }
            Ok(response) if is_upstream_credential_rejection(response.status()) => {
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    pool,
                    member,
                    status = response.status().as_u16(),
                    "v3 local target rejected the gateway credential"
                );
                drop(response);
                commit(local_credential_rejected(request_id))
            }
            Ok(response)
                if response.status() == StatusCode::TOO_MANY_REQUESTS
                    && plan.may_fall_forward(has_more, FallbackTrigger::RateLimited) =>
            {
                // A member can serve 429 from its own queue bound, and the
                // trigger the provider path honours means the same thing here.
                self.metrics
                    .record_pool_fallback(pool_name, FallbackTrigger::RateLimited);
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    pool,
                    member,
                    "v3 local target rate limited before commitment; trying next route step"
                );
                drop(response);
                StepOutcome::FallForward(None)
            }
            Ok(response)
                if response.status().is_server_error()
                    && plan.may_fall_forward(has_more, FallbackTrigger::PrecommitFailure) =>
            {
                // A member that admits and then fails is still local capacity
                // spilling to the next step, and the pool fallback counter is
                // the only place that shows it.
                self.metrics
                    .record_pool_fallback(pool_name, FallbackTrigger::PrecommitFailure);
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    pool,
                    member,
                    status = response.status().as_u16(),
                    "v3 local target failed before commitment; trying next route step"
                );
                drop(response);
                StepOutcome::FallForward(None)
            }
            Ok(response) => commit(decorate_local(
                response, plan, &pool, &member, &revision, request_id,
            )),
            Err(error) if plan.may_fall_forward(has_more, FallbackTrigger::PrecommitFailure) => {
                self.metrics
                    .record_pool_fallback(pool_name, FallbackTrigger::PrecommitFailure);
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    pool,
                    member,
                    %error,
                    "v3 local transport failed before commitment; trying next route step"
                );
                StepOutcome::FallForward(None)
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
        let admission = self
            .providers
            .try_admit(provider_name, request, plan.default_reasoning_effort)
            .await;
        self.metrics
            .record_routing_latency(routing_started.elapsed());
        let outcome = match admission {
            Ok(outcome) => outcome,
            Err(error) => {
                // A translation failure is a gateway fault with no upstream
                // call behind it, so it is recorded as an admission rejection
                // rather than left out of the metrics entirely.
                if !error.is_client_error() {
                    self.metrics
                        .record_rejected(provider_name, ProviderAdmissionState::Incompatible);
                }
                return commit(provider_request_error(&error, request_id));
            }
        };

        let lease = match outcome {
            ProviderAdmissionOutcome::Admitted(lease) => lease,
            ProviderAdmissionOutcome::Rejected(state) => {
                self.metrics.record_rejected(provider_name, state);
                let rejection = StepRejection::Provider(state);
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
                    return StepOutcome::FallForward(Some(rejection));
                }
                tracing::info!(
                    request_id,
                    route = plan.model.as_str(),
                    provider = provider_name,
                    state = ?state,
                    "v3 provider route could not be admitted"
                );
                return StepOutcome::Rejected(rejection);
            }
        };

        let provider = lease.provider().to_string();
        let model = lease.model().to_string();
        self.metrics.record_admitted(&provider);

        match self.transport.provider(*lease).await {
            Ok(response)
                if response.status() == StatusCode::TOO_MANY_REQUESTS
                    && plan.may_fall_forward(has_more, FallbackTrigger::RateLimited) =>
            {
                self.metrics
                    .record_response(&provider, ProviderResponseOutcome::RateLimited);
                self.metrics
                    .record_fallback(&provider, FallbackTrigger::RateLimited);
                // Deliberately no `record_dispatch_failure`: a 429 is the
                // provider throttling this caller, not evidence that the cached
                // readiness answer is stale. Invalidating it would send another
                // `/models` probe to a provider that just asked for less load.
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    provider,
                    "provider rate limited before commitment; trying next route step"
                );
                drop(response);
                StepOutcome::FallForward(None)
            }
            Ok(response)
                if is_upstream_credential_rejection(response.status())
                    && plan.may_fall_forward(has_more, FallbackTrigger::Unauthenticated) =>
            {
                // A credential the provider refuses is the same operator
                // condition as one that could not be resolved at admission, so
                // a route that opted into `unauthenticated` gets it honoured at
                // both points.
                let status = response.status();
                self.metrics
                    .record_response(&provider, provider_response_outcome(status));
                self.metrics
                    .record_fallback(&provider, FallbackTrigger::Unauthenticated);
                self.providers
                    .record_dispatch_failure(&provider, Some(status))
                    .await;
                tracing::warn!(
                    request_id,
                    route = plan.model.as_str(),
                    provider,
                    status = status.as_u16(),
                    "provider rejected the gateway credential; trying next route step"
                );
                drop(response);
                StepOutcome::FallForward(None)
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
                StepOutcome::FallForward(None)
            }
            Ok(response) => {
                let status = response.status();
                self.metrics
                    .record_response(&provider, provider_response_outcome(status));
                let credential_rejected = is_upstream_credential_rejection(status);
                // A committed response can still prove cached readiness wrong.
                if status.is_server_error() || credential_rejected {
                    self.providers
                        .record_dispatch_failure(&provider, Some(status))
                        .await;
                }
                if credential_rejected {
                    tracing::warn!(
                        request_id,
                        route = plan.model.as_str(),
                        provider,
                        status = status.as_u16(),
                        "provider rejected the gateway credential"
                    );
                    drop(response);
                    return commit(provider_credential_rejected(request_id));
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
                    return StepOutcome::FallForward(None);
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
