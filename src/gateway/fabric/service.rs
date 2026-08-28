//! Authenticated v3 orchestration for virtual routes and local inference pools.

use super::http_support::{
    FixedWindowRateLimiter, MetadataAuthorizationError, OCTOROUTE_REQUEST_ID_HEADER,
    REQUEST_ID_HEADER, error_response, header_bytes, hold_response_guard, insert_header,
    metadata_authorization_error, rate_limit_response,
};
use super::metrics::{
    FabricMetrics, ProviderResponseOutcome, fallback_trigger_label, pool_state_label,
};
use super::{
    FabricConfig, FabricRouteError, FabricTransport, FabricTransportError, FabricUpstreamTransport,
    FallbackTrigger, LlamaCppPool, LlamaCppPoolBuildError, PoolAdmissionOutcome,
    PoolAdmissionState, PreparedUpstreamResponse, PrivacyDirective, ProviderAdmissionOutcome,
    ProviderAdmissionState, ProviderRegistry, ProviderRegistryBuildError, RoutePlan, RouteTarget,
};
use crate::gateway::{
    auth::BearerAuthenticator, env::Environment, http_client::build as build_http_client,
    request::GatewayRequest,
};
use axum::{
    body::{Body, Bytes, to_bytes},
    http::{HeaderMap, Request, Response, StatusCode},
};
use futures::future::join_all;
use reqwest::Client;
use secrecy::SecretString;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

const DESTINATION_HEADER: &str = "x-octoroute-destination";
const REASON_HEADER: &str = "x-octoroute-reason";
const UPSTREAM_HEADER: &str = "x-octoroute-upstream";
const ROUTE_HEADER: &str = "x-octoroute-route";
const POOL_HEADER: &str = "x-octoroute-pool";
const MEMBER_HEADER: &str = "x-octoroute-member";
const MODEL_REVISION_HEADER: &str = "x-octoroute-model-revision";
const PROVIDER_HEADER: &str = "x-octoroute-provider";
const TARGET_HEADER: &str = "x-octoroute-target";

/// Executable v3 gateway service for ordered local-pool and provider routes.
pub struct FabricGatewayService<T> {
    config: Arc<FabricConfig>,
    authenticator: BearerAuthenticator,
    pools: BTreeMap<String, LlamaCppPool>,
    providers: ProviderRegistry,
    metrics: Arc<FabricMetrics>,
    transport: T,
    inbound_permits: Arc<Semaphore>,
    rate_limiter: FixedWindowRateLimiter,
}

/// Bounded v3 readiness snapshot.
#[derive(Debug, Clone)]
pub struct FabricReadiness {
    pools: BTreeMap<String, PoolAdmissionState>,
    providers: BTreeMap<String, ProviderAdmissionState>,
}

impl FabricReadiness {
    pub fn pools(&self) -> &BTreeMap<String, PoolAdmissionState> {
        &self.pools
    }

    pub fn providers(&self) -> &BTreeMap<String, ProviderAdmissionState> {
        &self.providers
    }

    pub fn is_ready(&self) -> bool {
        self.pools
            .values()
            .any(|state| *state == PoolAdmissionState::Ready)
            || self
                .providers
                .values()
                .any(|state| *state == ProviderAdmissionState::Ready)
    }
}

impl<T> FabricGatewayService<T>
where
    T: FabricUpstreamTransport,
{
    /// Build the v3 service without resolving provider credentials.
    pub fn new<E>(
        config: FabricConfig,
        environment: E,
        transport: T,
    ) -> Result<Self, FabricGatewayServiceBuildError>
    where
        E: Environment + Send + Sync + 'static,
    {
        let client = build_http_client().map_err(FabricTransportError::HttpClient)?;
        Self::with_client(config, environment, transport, client)
    }

    /// Build the v3 service over one caller-supplied pooled client.
    pub(crate) fn with_client<E>(
        config: FabricConfig,
        environment: E,
        transport: T,
        client: Client,
    ) -> Result<Self, FabricGatewayServiceBuildError>
    where
        E: Environment + Send + Sync + 'static,
    {
        let environment: Arc<dyn Environment + Send + Sync> = Arc::new(environment);
        let inbound_key = resolve_secret(
            environment.as_ref(),
            "server.api_key_env",
            config.server.api_key_env.as_str(),
        )?;
        let mut pools = BTreeMap::new();
        for (name, pool_config) in &config.local_pools {
            if !pool_config.enabled {
                continue;
            }
            let pool = LlamaCppPool::with_client(pool_config, environment.as_ref(), client.clone())
                .map_err(|source| FabricGatewayServiceBuildError::Pool {
                    pool: name.clone(),
                    source,
                })?;
            pools.insert(name.clone(), pool);
        }
        let metrics = Arc::new(FabricMetrics::new(&config));
        let providers =
            ProviderRegistry::new(&config.providers, environment, Arc::clone(&metrics), client)?;

        Ok(Self {
            authenticator: BearerAuthenticator::new(inbound_key),
            inbound_permits: Arc::new(Semaphore::new(config.server.max_in_flight)),
            rate_limiter: FixedWindowRateLimiter::new(config.server.requests_per_minute),
            config: Arc::new(config),
            pools,
            providers,
            metrics,
            transport,
        })
    }

    /// Authenticate, route, dispatch, and stream one bounded chat-completions request.
    pub async fn handle_chat(&self, headers: &HeaderMap, bytes: Bytes) -> Response<Body> {
        let (request_id, permit) = match self.preflight(headers) {
            Ok(preflight) => preflight,
            Err(response) => return *response,
        };
        if bytes.len() > self.config.server.max_request_bytes {
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
        let bytes = match to_bytes(body, self.config.server.max_request_bytes).await {
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

    pub(super) fn authorize_metadata(
        &self,
        headers: &HeaderMap,
    ) -> Result<(), MetadataAuthorizationError> {
        if header_bytes(headers) > self.config.server.max_header_bytes {
            return Err(MetadataAuthorizationError::HeadersTooLarge);
        }
        self.authenticator
            .authorize(headers)
            .map_err(|_| MetadataAuthorizationError::Unauthorized)
    }

    /// Virtual model identifiers exposed to OpenAI-compatible clients.
    pub fn model_ids(&self) -> Vec<String> {
        let mut models = BTreeSet::from(["auto".to_string()]);
        models.extend(self.config.routes.keys().cloned());
        models.into_iter().collect()
    }

    /// Probe all configured local pools and providers concurrently.
    pub async fn readiness(&self) -> FabricReadiness {
        let probes = self.config.local_pools.iter().map(|(name, pool_config)| {
            let pool = self.pools.get(name).cloned();
            let name = name.clone();
            let enabled = pool_config.enabled;
            async move {
                let state = if !enabled {
                    PoolAdmissionState::Disabled
                } else {
                    match pool {
                        Some(pool) => pool.readiness_state().await,
                        None => PoolAdmissionState::Unhealthy,
                    }
                };
                (name, state)
            }
        });
        FabricReadiness {
            pools: join_all(probes).await.into_iter().collect(),
            providers: self.providers.readiness().await,
        }
    }

    /// Render bounded Prometheus exposition for the v3 runtime.
    pub fn metrics_text(&self) -> String {
        self.metrics.render(&self.config)
    }

    fn preflight(
        &self,
        headers: &HeaderMap,
    ) -> Result<(String, OwnedSemaphorePermit), Box<Response<Body>>> {
        let request_id = Uuid::new_v4().to_string();
        if let Err(error) = self.authorize_metadata(headers) {
            return Err(Box::new(metadata_authorization_error(error, &request_id)));
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
        let plan = match self
            .config
            .route_plan(request.model(), privacy == PrivacyDirective::LocalOnly)
        {
            Ok(plan) => plan,
            Err(error) => return route_error(error, request_id),
        };
        self.dispatch_route(&request, &plan, request_id).await
    }

    async fn dispatch_route(
        &self,
        request: &GatewayRequest,
        plan: &RoutePlan,
        request_id: &str,
    ) -> Response<Body> {
        // Routing latency is everything spent selecting a destination -
        // admission, health, slot, and token-count probes - up to the moment a
        // lease is held, and excludes the upstream call itself.
        let routing_started = Instant::now();
        for (index, step) in plan.steps.iter().enumerate() {
            let has_more = index + 1 < plan.steps.len();
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

impl FabricGatewayService<FabricTransport> {
    pub fn from_config<E>(
        config: FabricConfig,
        environment: E,
    ) -> Result<Self, FabricGatewayServiceBuildError>
    where
        E: Environment + Send + Sync + 'static,
    {
        // Strix health, slot, and token probes, Strix inference, and every
        // cloud provider share one pooled rustls client and its TLS session
        // cache. Credentials are still applied per request.
        let client = build_http_client().map_err(FabricTransportError::HttpClient)?;
        let transport = FabricTransport::with_client(client.clone());
        Self::with_client(config, environment, transport, client)
    }
}

/// V3 runtime construction failures detected before binding the listener.
#[derive(Debug, Error)]
pub enum FabricGatewayServiceBuildError {
    #[error("environment variable `{name}` required by `{field}` is missing or empty")]
    MissingEnvironmentVariable { field: String, name: String },
    #[error("credential referenced by `{field}` must use visible ASCII without whitespace")]
    InvalidCredential { field: String },
    #[error("could not build local pool `{pool}`: {source}")]
    Pool {
        pool: String,
        #[source]
        source: LlamaCppPoolBuildError,
    },
    #[error(transparent)]
    ProviderRegistry(#[from] ProviderRegistryBuildError),
    #[error(transparent)]
    Transport(#[from] FabricTransportError),
}

fn resolve_secret(
    environment: &(impl Environment + ?Sized),
    field: &str,
    name: &str,
) -> Result<SecretString, FabricGatewayServiceBuildError> {
    let value = environment
        .get(name)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(
            || FabricGatewayServiceBuildError::MissingEnvironmentVariable {
                field: field.to_string(),
                name: name.to_string(),
            },
        )?;
    if !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(FabricGatewayServiceBuildError::InvalidCredential {
            field: field.to_string(),
        });
    }
    Ok(SecretString::from(value))
}

fn fallback_trigger(state: PoolAdmissionState) -> Option<FallbackTrigger> {
    match state {
        PoolAdmissionState::Ready => None,
        PoolAdmissionState::Disabled
        | PoolAdmissionState::Unhealthy
        | PoolAdmissionState::TokenCountUnavailable => Some(FallbackTrigger::Unhealthy),
        PoolAdmissionState::Incompatible => Some(FallbackTrigger::Incompatible),
        PoolAdmissionState::Busy => Some(FallbackTrigger::Busy),
        PoolAdmissionState::ContextOverflow => Some(FallbackTrigger::ContextOverflow),
    }
}

fn provider_fallback_trigger(state: ProviderAdmissionState) -> Option<FallbackTrigger> {
    match state {
        ProviderAdmissionState::Ready => None,
        ProviderAdmissionState::Disabled | ProviderAdmissionState::Unavailable => {
            Some(FallbackTrigger::Unhealthy)
        }
        ProviderAdmissionState::Incompatible => Some(FallbackTrigger::Incompatible),
        ProviderAdmissionState::Busy => Some(FallbackTrigger::Busy),
        // Outside the default trigger set: an expired or missing key must not
        // silently reroute traffic and spend to the next provider.
        ProviderAdmissionState::Unauthenticated => Some(FallbackTrigger::Unauthenticated),
    }
}

fn provider_response_outcome(status: StatusCode) -> ProviderResponseOutcome {
    if status == StatusCode::TOO_MANY_REQUESTS {
        ProviderResponseOutcome::RateLimited
    } else if status.is_server_error() {
        ProviderResponseOutcome::ServerError
    } else if status.is_client_error() {
        ProviderResponseOutcome::ClientError
    } else {
        ProviderResponseOutcome::Success
    }
}

fn route_error(error: FabricRouteError, request_id: &str) -> Response<Body> {
    let status = match &error {
        FabricRouteError::UnknownModel(_) | FabricRouteError::ContradictoryPrivacy => {
            StatusCode::BAD_REQUEST
        }
        FabricRouteError::NoEligibleTarget => StatusCode::SERVICE_UNAVAILABLE,
    };
    error_response(
        status,
        &error.to_string(),
        "invalid_request_error",
        "routing_error",
        request_id,
    )
}

fn pool_state_error(state: PoolAdmissionState, pool: &str, request_id: &str) -> Response<Body> {
    let (status, message, code) = match state {
        PoolAdmissionState::Ready => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "local admission returned an inconsistent ready state",
            "internal_routing_error",
        ),
        PoolAdmissionState::Disabled => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected local pool is disabled",
            "local_pool_disabled",
        ),
        PoolAdmissionState::Incompatible => (
            StatusCode::BAD_REQUEST,
            "the selected local pool does not support this request",
            "local_incompatible",
        ),
        PoolAdmissionState::TokenCountUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected local pool could not count input tokens for this request",
            "local_token_count_unavailable",
        ),
        PoolAdmissionState::Busy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "every eligible local member is busy",
            "local_busy",
        ),
        PoolAdmissionState::Unhealthy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no healthy local member is available",
            "local_unhealthy",
        ),
        PoolAdmissionState::ContextOverflow => (
            StatusCode::BAD_REQUEST,
            "the request exceeds the selected local pool context budget",
            "local_context_overflow",
        ),
    };
    tracing::info!(
        request_id,
        pool,
        state = ?state,
        "v3 local route could not be admitted"
    );
    error_response(status, message, "invalid_request_error", code, request_id)
}

fn provider_state_error(
    state: ProviderAdmissionState,
    provider: &str,
    request_id: &str,
) -> Response<Body> {
    let (status, message, code) = match state {
        ProviderAdmissionState::Ready => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider admission returned an inconsistent ready state",
            "internal_routing_error",
        ),
        ProviderAdmissionState::Disabled => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider is disabled",
            "provider_disabled",
        ),
        ProviderAdmissionState::Incompatible => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider does not have a compatible runtime adapter",
            "provider_incompatible",
        ),
        ProviderAdmissionState::Busy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider is at its concurrency limit",
            "provider_busy",
        ),
        ProviderAdmissionState::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider is unavailable",
            "provider_unavailable",
        ),
        ProviderAdmissionState::Unauthenticated => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the selected provider rejected or could not supply its credential",
            "provider_unauthenticated",
        ),
    };
    tracing::info!(
        request_id,
        provider,
        state = ?state,
        "v3 provider route could not be admitted"
    );
    error_response(status, message, "upstream_error", code, request_id)
}

fn decorate_local(
    response: PreparedUpstreamResponse,
    plan: &RoutePlan,
    pool: &str,
    member: &str,
    revision: &str,
    request_id: &str,
) -> Response<Body> {
    let upstream = format!("{pool}/{member}");
    let mut response = response.into_response();
    insert_header(response.headers_mut(), DESTINATION_HEADER, "local");
    insert_header(response.headers_mut(), REASON_HEADER, "local_pool");
    insert_header(response.headers_mut(), UPSTREAM_HEADER, &upstream);
    insert_header(response.headers_mut(), ROUTE_HEADER, &plan.model);
    insert_header(
        response.headers_mut(),
        TARGET_HEADER,
        &format!("pool:{pool}"),
    );
    insert_header(response.headers_mut(), POOL_HEADER, pool);
    insert_header(response.headers_mut(), MEMBER_HEADER, member);
    insert_header(response.headers_mut(), MODEL_REVISION_HEADER, revision);
    insert_header(
        response.headers_mut(),
        OCTOROUTE_REQUEST_ID_HEADER,
        request_id,
    );
    if !response.headers().contains_key(REQUEST_ID_HEADER) {
        insert_header(response.headers_mut(), REQUEST_ID_HEADER, request_id);
    }
    tracing::info!(
        request_id,
        route = plan.model.as_str(),
        destination = "local",
        pool,
        member,
        status = response.status().as_u16(),
        "v3 gateway response committed"
    );
    response
}

fn decorate_provider(
    response: PreparedUpstreamResponse,
    plan: &RoutePlan,
    provider: &str,
    model: &str,
    request_id: &str,
) -> Response<Body> {
    let mut response = response.into_response();
    insert_header(response.headers_mut(), DESTINATION_HEADER, "cloud");
    insert_header(response.headers_mut(), REASON_HEADER, "provider");
    insert_header(response.headers_mut(), UPSTREAM_HEADER, provider);
    insert_header(response.headers_mut(), ROUTE_HEADER, &plan.model);
    insert_header(response.headers_mut(), PROVIDER_HEADER, provider);
    insert_header(
        response.headers_mut(),
        TARGET_HEADER,
        &format!("provider:{provider}"),
    );
    insert_header(
        response.headers_mut(),
        OCTOROUTE_REQUEST_ID_HEADER,
        request_id,
    );
    if !response.headers().contains_key(REQUEST_ID_HEADER) {
        insert_header(response.headers_mut(), REQUEST_ID_HEADER, request_id);
    }
    tracing::info!(
        request_id,
        route = plan.model.as_str(),
        destination = "cloud",
        provider,
        model,
        status = response.status().as_u16(),
        "v3 gateway response committed"
    );
    response
}
