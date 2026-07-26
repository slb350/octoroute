use super::*;
use crate::gateway::{
    intelligence::{IntelligentRoute, IntelligentRouterError},
    local::AdmissionOutcome,
    routing::{LocalRoutePlan, RoutePlan, RoutePolicy},
};
use tokio::sync::OwnedSemaphorePermit;

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
                    match self.intelligent_router.route(&request).await {
                        Ok(IntelligentRoute::Cloud) => {
                            self.record_routing_duration(routing_started);
                            return self
                                .dispatch_cloud(
                                    request,
                                    intent,
                                    RouteReason::CloudQuality,
                                    request_id,
                                )
                                .await;
                        }
                        Ok(IntelligentRoute::Local(permit)) => Some(permit),
                        Err(error) => {
                            let reason = semantic_failure_reason(local_plan, &error);
                            tracing::warn!(
                                request_id,
                                %error,
                                reason = reason.as_str(),
                                "semantic routing failed safely to cloud"
                            );
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
