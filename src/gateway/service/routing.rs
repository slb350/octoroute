use super::*;
use crate::gateway::{
    config::SemanticRoutingMode,
    intelligence::{IntelligentRoute, IntelligentRouterError, SemanticAssessment},
    local::AdmissionOutcome,
    metrics::SemanticDecisionOutcome,
    routing::{LocalRoutePlan, RoutePlan, RoutePolicy},
};
use tokio::sync::OwnedSemaphorePermit;

enum SemanticRouteAction {
    Admit(Option<OwnedSemaphorePermit>),
    Cloud(RouteReason),
}

impl<T> GatewayService<T>
where
    T: UpstreamTransport,
{
    pub(super) async fn route_and_dispatch(
        &self,
        request: GatewayRequest,
        intent: ModelIntent,
        privacy: PrivacyDirective,
        request_id: &str,
    ) -> Response<Body> {
        let routing_started = Instant::now();
        let plan = match RoutePolicy::new(&self.config).plan(&request, &intent, privacy) {
            Ok(plan) => plan,
            Err(error) => return route_error(error, request_id),
        };
        match plan {
            RoutePlan::Cloud(reason) => {
                self.record_routing_duration(routing_started);
                self.dispatch_cloud(request, intent, reason, request_id)
                    .await
            }
            RoutePlan::Local(local_plan) => {
                let reservation = if local_plan.is_automatic() {
                    match self
                        .apply_semantic_mode(&request, local_plan, request_id)
                        .await
                    {
                        SemanticRouteAction::Admit(reservation) => reservation,
                        SemanticRouteAction::Cloud(reason) => {
                            self.record_routing_duration(routing_started);
                            return self
                                .dispatch_cloud(request, intent, reason, request_id)
                                .await;
                        }
                    }
                } else {
                    None
                };
                self.admit_and_dispatch(
                    request,
                    intent,
                    local_plan,
                    reservation,
                    routing_started,
                    request_id,
                )
                .await
            }
        }
    }

    async fn apply_semantic_mode(
        &self,
        request: &GatewayRequest,
        local_plan: LocalRoutePlan,
        request_id: &str,
    ) -> SemanticRouteAction {
        let mode = self.config.routing().semantic_mode();
        if mode == SemanticRoutingMode::Disabled {
            return SemanticRouteAction::Admit(None);
        }
        match self.intelligent_router.route(request).await {
            IntelligentRoute::Observed {
                assessment,
                reservation,
            } => {
                self.record_semantic_observation(mode, request_id, Ok(assessment));
                let destination = assessment.destination();
                if mode == SemanticRoutingMode::Enforced && destination == RouteDestination::Cloud {
                    SemanticRouteAction::Cloud(RouteReason::CloudQuality)
                } else {
                    SemanticRouteAction::Admit(Some(reservation))
                }
            }
            IntelligentRoute::Failed { error, reservation } => {
                self.record_semantic_observation(mode, request_id, Err(&error));
                if mode == SemanticRoutingMode::Shadow {
                    SemanticRouteAction::Admit(Some(reservation))
                } else {
                    SemanticRouteAction::Cloud(semantic_failure_reason(local_plan, &error))
                }
            }
            IntelligentRoute::Unavailable(state) => {
                let error = IntelligentRouterError::LocalUnavailable(state);
                self.record_semantic_observation(mode, request_id, Err(&error));
                SemanticRouteAction::Cloud(semantic_failure_reason(local_plan, &error))
            }
        }
    }

    fn record_semantic_observation(
        &self,
        mode: SemanticRoutingMode,
        request_id: &str,
        outcome: Result<SemanticAssessment, &IntelligentRouterError>,
    ) {
        let metric_outcome = match outcome {
            Ok(assessment) => SemanticDecisionOutcome::from(assessment.destination()),
            Err(_) => SemanticDecisionOutcome::Failure,
        };
        if let Err(error) = self.metrics.record_semantic_decision(mode, metric_outcome) {
            tracing::warn!(%error, "failed to record semantic routing metric");
        }
        if mode == SemanticRoutingMode::Shadow {
            match outcome {
                Ok(assessment) => tracing::debug!(
                    request_id,
                    semantic_destination = assessment.destination().as_str(),
                    capability_boundary = assessment.boundary(),
                    local_success_probability = assessment.local_success_probability(),
                    "observed shadow semantic routing forecast"
                ),
                Err(error) => tracing::warn!(
                    request_id,
                    %error,
                    "shadow semantic routing decision failed without selecting destination"
                ),
            }
        } else if let Err(error) = outcome {
            tracing::warn!(
                request_id,
                %error,
                "semantic routing failed safely to cloud"
            );
        }
    }

    async fn admit_and_dispatch(
        &self,
        request: GatewayRequest,
        intent: ModelIntent,
        local_plan: LocalRoutePlan,
        reservation: Option<OwnedSemaphorePermit>,
        routing_started: Instant,
        request_id: &str,
    ) -> Response<Body> {
        let outcome = match reservation {
            Some(permit) => self.admission.try_admit_reserved(&request, permit).await,
            None => self.admission.try_admit(&request).await,
        };
        match outcome {
            Ok(AdmissionOutcome::Admitted(lease)) => {
                let decision = local_plan.admitted();
                self.record_routing_duration(routing_started);
                if decision.fallback_before_commit() {
                    self.dispatch_local_with_fallback(request, intent, decision, lease, request_id)
                        .await
                } else {
                    drop(request);
                    drop(intent);
                    self.dispatch_local(decision, lease, request_id).await
                }
            }
            Ok(AdmissionOutcome::Rejected(state)) => {
                let decision = match local_plan.resolve(state) {
                    Ok(decision) => decision,
                    Err(error) => return route_error(error, request_id),
                };
                self.record_routing_duration(routing_started);
                self.dispatch_cloud(request, intent, decision.reason(), request_id)
                    .await
            }
            Err(error) => error_response(
                StatusCode::BAD_REQUEST,
                &error.to_string(),
                "invalid_request_error",
                "invalid_token_budget",
                request_id,
            ),
        }
    }
}

fn semantic_failure_reason(
    local_plan: LocalRoutePlan,
    error: &IntelligentRouterError,
) -> RouteReason {
    error
        .local_state()
        .and_then(|state| local_plan.resolve(state).ok())
        .map_or(RouteReason::RouterFailure, |decision| decision.reason())
}
