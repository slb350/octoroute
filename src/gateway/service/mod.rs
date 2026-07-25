//! Authenticated request orchestration across local and cloud upstreams.

use crate::gateway::{
    auth::BearerAuthenticator,
    config::GatewayConfig,
    local::{AdmissionOutcome, LlamaCppAdmission, LlamaCppAdmissionBuildError, LocalLease},
    metrics::{FailurePhase, GatewayMetrics, UpstreamLabel},
    openrouter::OpenRouterRequest,
    request::GatewayRequest,
    routing::{
        LocalAdmissionState, ModelIntent, PrivacyDirective, RouteDecision, RouteDestination,
        RoutePlan, RoutePolicy, RouteReason,
    },
    transport::{
        GatewayTransport, GatewayTransportError, PreparedUpstreamResponse, UpstreamTransport,
    },
};
use axum::{
    body::{Body, Bytes, to_bytes},
    http::{HeaderMap, Request, Response, StatusCode},
};
use std::{sync::Arc, time::Instant};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

mod limits;
mod observability;
mod responses;

use limits::{FixedWindowRateLimiter, header_bytes, hold_response_guard, observe_response_body};
pub(crate) use responses::{authorization_error as metadata_authorization_error, error_response};
use responses::{authorization_error, insert_header, rate_limit_response, route_error};

const DESTINATION_HEADER: &str = "x-octoroute-destination";
const REASON_HEADER: &str = "x-octoroute-reason";
const UPSTREAM_HEADER: &str = "x-octoroute-upstream";
const REQUEST_ID_HEADER: &str = "x-request-id";

/// Complete authenticated v2 chat-completions service.
pub struct GatewayService<T> {
    config: Arc<GatewayConfig>,
    authenticator: BearerAuthenticator,
    admission: LlamaCppAdmission,
    transport: T,
    inbound_permits: Arc<Semaphore>,
    cloud_permits: Arc<Semaphore>,
    rate_limiter: FixedWindowRateLimiter,
    metrics: GatewayMetrics,
}

/// Snapshot returned by the readiness endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayReadiness {
    local: LocalAdmissionState,
    openrouter: bool,
}

/// Authentication and header-bound failures for protected metadata routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataAuthorizationError {
    /// Aggregate header bytes exceed the configured limit.
    HeadersTooLarge,
    /// Bearer authentication failed.
    Unauthorized,
}

impl GatewayReadiness {
    /// Local llama.cpp admission state.
    pub fn local(self) -> LocalAdmissionState {
        self.local
    }

    /// Whether the OpenRouter credential probe succeeded.
    pub fn openrouter(self) -> bool {
        self.openrouter
    }

    /// The gateway can serve when at least one upstream is available.
    pub fn is_ready(self) -> bool {
        self.local == LocalAdmissionState::Ready || self.openrouter
    }
}

impl<T> GatewayService<T>
where
    T: UpstreamTransport,
{
    /// Combine validated configuration with a concrete pre-commit transport.
    pub fn new(config: GatewayConfig, transport: T) -> Result<Self, GatewayServiceBuildError> {
        let admission =
            LlamaCppAdmission::new(config.local()).map_err(GatewayServiceBuildError::Admission)?;
        Self::with_admission(config, transport, admission)
    }

    fn with_admission(
        config: GatewayConfig,
        transport: T,
        admission: LlamaCppAdmission,
    ) -> Result<Self, GatewayServiceBuildError> {
        let authenticator = BearerAuthenticator::new(config.server().api_key().clone());
        let inbound_permits = Arc::new(Semaphore::new(config.server().max_in_flight()));
        let cloud_permits = Arc::new(Semaphore::new(config.openrouter().max_in_flight()));
        let rate_limiter = FixedWindowRateLimiter::new(config.server().requests_per_minute());
        let metrics = GatewayMetrics::new()?;
        Ok(Self {
            config: Arc::new(config),
            authenticator,
            admission,
            transport,
            inbound_permits,
            cloud_permits,
            rate_limiter,
            metrics,
        })
    }

    /// Authenticate, route, dispatch, and stream one chat-completions request.
    pub async fn handle_chat(&self, headers: &HeaderMap, bytes: Bytes) -> Response<Body> {
        let (request_id, permit) = match self.preflight(headers) {
            Ok(preflight) => preflight,
            Err(response) => return *response,
        };
        if bytes.len() > self.config.server().max_request_bytes() {
            return hold_response_guard(
                error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body exceeds the configured size limit",
                    "invalid_request_error",
                    "request_too_large",
                    &request_id,
                ),
                permit,
            );
        }
        let response = self
            .handle_authorized_chat(headers, bytes, &request_id)
            .await;
        hold_response_guard(response, permit)
    }

    /// Authenticate headers before reading a bounded HTTP body.
    pub async fn handle_http_chat(&self, request: Request<Body>) -> Response<Body> {
        let (parts, body) = request.into_parts();
        let (request_id, permit) = match self.preflight(&parts.headers) {
            Ok(preflight) => preflight,
            Err(response) => return *response,
        };
        let bytes = match to_bytes(body, self.config.server().max_request_bytes()).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return hold_response_guard(
                    error_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request body exceeds the configured size limit",
                        "invalid_request_error",
                        "request_too_large",
                        &request_id,
                    ),
                    permit,
                );
            }
        };
        let response = self
            .handle_authorized_chat(&parts.headers, bytes, &request_id)
            .await;
        hold_response_guard(response, permit)
    }

    /// Validate authentication for protected metadata endpoints.
    pub fn authorize_metadata(
        &self,
        headers: &HeaderMap,
    ) -> Result<(), MetadataAuthorizationError> {
        if header_bytes(headers) > self.config.server().max_header_bytes() {
            return Err(MetadataAuthorizationError::HeadersTooLarge);
        }
        self.authenticator
            .authorize(headers)
            .map_err(|_| MetadataAuthorizationError::Unauthorized)
    }

    /// Virtual and exact model identifiers advertised to OpenAI clients.
    pub fn model_ids(&self) -> [&str; 4] {
        ["auto", "local", "cloud", self.config.local().model()]
    }

    /// Probe and aggregate local and cloud readiness concurrently.
    pub async fn readiness(&self) -> GatewayReadiness {
        let (local, openrouter) = tokio::join!(
            self.admission.readiness_state(),
            self.transport.openrouter_ready()
        );
        GatewayReadiness { local, openrouter }
    }

    fn preflight(
        &self,
        headers: &HeaderMap,
    ) -> Result<(String, OwnedSemaphorePermit), Box<Response<Body>>> {
        let request_id = Uuid::new_v4().to_string();
        if let Err(error) = self.authorize_metadata(headers) {
            return Err(Box::new(authorization_error(error, &request_id)));
        }
        if !self.rate_limiter.allow() {
            return Err(Box::new(rate_limit_response(
                "authenticated request rate limit exceeded",
                "rate_limit_exceeded",
                &request_id,
            )));
        }
        let permit = match Arc::clone(&self.inbound_permits).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return Err(Box::new(rate_limit_response(
                    "authenticated request concurrency limit exceeded",
                    "request_concurrency_limit",
                    &request_id,
                )));
            }
        };
        Ok((request_id, permit))
    }

    async fn handle_authorized_chat(
        &self,
        headers: &HeaderMap,
        bytes: Bytes,
        request_id: &str,
    ) -> Response<Body> {
        let request = match GatewayRequest::parse(&bytes) {
            Ok(request) => request,
            Err(error) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &error.to_string(),
                    "invalid_request_error",
                    "invalid_request",
                    request_id,
                );
            }
        };
        let intent = match ModelIntent::resolve(
            request.model(),
            self.config.local().model(),
            self.config.openrouter().auto_model(),
        ) {
            Ok(intent) => intent,
            Err(error) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &error.to_string(),
                    "invalid_request_error",
                    "unknown_model",
                    request_id,
                );
            }
        };
        let privacy = match PrivacyDirective::from_headers(headers) {
            Ok(privacy) => privacy,
            Err(error) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &error.to_string(),
                    "invalid_request_error",
                    "invalid_privacy_directive",
                    request_id,
                );
            }
        };

        self.route_and_dispatch(request, intent, privacy, request_id)
            .await
    }

    async fn route_and_dispatch(
        &self,
        request: GatewayRequest,
        intent: ModelIntent,
        privacy: PrivacyDirective,
        request_id: &str,
    ) -> Response<Body> {
        let routing_started = Instant::now();
        let policy = RoutePolicy::new(&self.config);
        let plan = match policy.plan(&request, &intent, privacy) {
            Ok(plan) => plan,
            Err(error) => return route_error(error, request_id),
        };
        match plan {
            RoutePlan::Cloud(reason) => {
                self.record_routing_duration(routing_started);
                self.dispatch_cloud(request, intent, reason, request_id)
                    .await
            }
            RoutePlan::Local(local_plan) => match self.admission.try_admit(&request).await {
                Ok(AdmissionOutcome::Admitted(lease)) => {
                    let decision = local_plan.admitted();
                    self.record_routing_duration(routing_started);
                    if decision.fallback_before_commit() {
                        self.dispatch_local_with_fallback(
                            request, intent, decision, lease, request_id,
                        )
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
            },
        }
    }

    async fn dispatch_local(
        &self,
        decision: RouteDecision,
        lease: LocalLease,
        request_id: &str,
    ) -> Response<Body> {
        let dispatch_started = Instant::now();
        match self.transport.local(lease).await {
            Ok(response) => {
                self.finish_local_response(response, decision, dispatch_started, request_id)
            }
            Err(_) => {
                self.record_local_transport_failure();
                error_response(
                    StatusCode::BAD_GATEWAY,
                    "local upstream failed before response commitment",
                    "upstream_error",
                    "local_upstream_error",
                    request_id,
                )
            }
        }
    }

    async fn dispatch_local_with_fallback(
        &self,
        request: GatewayRequest,
        intent: ModelIntent,
        decision: RouteDecision,
        lease: LocalLease,
        request_id: &str,
    ) -> Response<Body> {
        let dispatch_started = Instant::now();
        match self.transport.local(lease).await {
            Ok(response) if response.status().is_server_error() => {
                self.record_upstream_response(UpstreamLabel::Local, response.status());
                drop(response);
                self.metrics.record_fallback();
                self.dispatch_cloud(request, intent, RouteReason::LocalEarlyFailure, request_id)
                    .await
            }
            Ok(response) => {
                self.finish_local_response(response, decision, dispatch_started, request_id)
            }
            Err(_) => {
                self.record_local_transport_failure();
                self.metrics.record_fallback();
                self.dispatch_cloud(request, intent, RouteReason::LocalEarlyFailure, request_id)
                    .await
            }
        }
    }

    fn finish_local_response(
        &self,
        response: PreparedUpstreamResponse,
        decision: RouteDecision,
        dispatch_started: Instant,
        request_id: &str,
    ) -> Response<Body> {
        self.record_upstream_response(UpstreamLabel::Local, response.status());
        self.record_time_to_first_byte(RouteDestination::Local, dispatch_started);
        self.decorate(
            response,
            RouteDestination::Local,
            decision.reason(),
            UpstreamLabel::Local,
            request_id,
        )
    }

    fn record_local_transport_failure(&self) {
        self.record_upstream_transport_failure(UpstreamLabel::Local);
        self.record_upstream_failure(UpstreamLabel::Local);
    }

    async fn dispatch_cloud(
        &self,
        request: GatewayRequest,
        intent: ModelIntent,
        reason: RouteReason,
        request_id: &str,
    ) -> Response<Body> {
        if reason == RouteReason::LocalBusy {
            self.metrics.record_local_busy_spillover();
        }
        let request = match OpenRouterRequest::build(request, &intent, self.config.openrouter()) {
            Ok(request) => request,
            Err(error) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &error.to_string(),
                    "invalid_request_error",
                    "invalid_openrouter_request",
                    request_id,
                );
            }
        };
        let permit = match Arc::clone(&self.cloud_permits).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return rate_limit_response(
                    "cloud request concurrency limit exceeded",
                    "cloud_concurrency_limit",
                    request_id,
                );
            }
        };
        let dispatch_started = Instant::now();
        match self.transport.openrouter(request).await {
            Ok(response) => {
                self.record_upstream_response(UpstreamLabel::OpenRouter, response.status());
                self.record_time_to_first_byte(RouteDestination::Cloud, dispatch_started);
                hold_response_guard(
                    self.decorate(
                        response,
                        RouteDestination::Cloud,
                        reason,
                        UpstreamLabel::OpenRouter,
                        request_id,
                    ),
                    permit,
                )
            }
            Err(_) => {
                self.record_upstream_transport_failure(UpstreamLabel::OpenRouter);
                self.record_upstream_failure(UpstreamLabel::OpenRouter);
                error_response(
                    StatusCode::BAD_GATEWAY,
                    "OpenRouter failed before response commitment",
                    "upstream_error",
                    "openrouter_upstream_error",
                    request_id,
                )
            }
        }
    }

    fn decorate(
        &self,
        response: PreparedUpstreamResponse,
        destination: RouteDestination,
        reason: RouteReason,
        upstream: UpstreamLabel,
        request_id: &str,
    ) -> Response<Body> {
        if let Err(error) = self.metrics.record_route(destination, reason) {
            tracing::warn!(%error, "failed to record gateway route metric");
        }
        let response_observation = match self.metrics.start_response(destination) {
            Ok(observation) => Some(observation),
            Err(error) => {
                tracing::warn!(%error, "failed to start gateway response observation");
                None
            }
        };
        let mid_stream_failures = match self
            .metrics
            .failure_counter(upstream, FailurePhase::MidStream)
        {
            Ok(counter) => Some(counter),
            Err(error) => {
                tracing::warn!(%error, "failed to bind mid-stream upstream failure metric");
                None
            }
        };
        let mut response = observe_response_body(
            response.into_response(),
            mid_stream_failures,
            response_observation,
        );
        let destination_name = destination.as_str();
        let upstream_name = match upstream {
            UpstreamLabel::Local => self.config.local().name(),
            UpstreamLabel::OpenRouter => "openrouter",
        };
        tracing::info!(
            request_id,
            destination = destination_name,
            reason = reason.as_str(),
            upstream = upstream_name,
            status = response.status().as_u16(),
            "gateway response committed"
        );
        insert_header(response.headers_mut(), DESTINATION_HEADER, destination_name);
        insert_header(response.headers_mut(), REASON_HEADER, reason.as_str());
        insert_header(response.headers_mut(), UPSTREAM_HEADER, upstream_name);
        if !response.headers().contains_key(REQUEST_ID_HEADER) {
            insert_header(response.headers_mut(), REQUEST_ID_HEADER, request_id);
        }
        response
    }

    /// Prometheus text exposition for the v2 registry.
    pub fn metrics_text(&self) -> Result<String, prometheus::Error> {
        self.metrics.encode()
    }
}

impl GatewayService<GatewayTransport> {
    /// Build the production service from one validated configuration.
    pub fn from_config(config: GatewayConfig) -> Result<Self, GatewayServiceBuildError> {
        let transport = GatewayTransport::new(&config)?;
        let admission = LlamaCppAdmission::with_client(config.local(), transport.http_client())
            .map_err(GatewayServiceBuildError::Admission)?;
        Self::with_admission(config, transport, admission)
    }
}

/// Production gateway construction failures.
#[derive(Debug, Error)]
pub enum GatewayServiceBuildError {
    /// HTTP transport construction failed.
    #[error(transparent)]
    Transport(#[from] GatewayTransportError),
    /// llama.cpp admission construction failed.
    #[error(transparent)]
    Admission(LlamaCppAdmissionBuildError),
    /// Gateway metric registration failed.
    #[error("could not register gateway metrics")]
    Metrics(#[from] prometheus::Error),
}
