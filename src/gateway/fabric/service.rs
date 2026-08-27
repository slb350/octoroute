//! Authenticated v3 orchestration for virtual routes and local inference pools.

use super::{
    FabricConfig, FabricRouteError, FabricTransport, FabricUpstreamTransport, FallbackTrigger,
    LlamaCppPool, LlamaCppPoolBuildError, PoolAdmissionOutcome, PoolAdmissionState, RoutePlan,
    RouteTarget,
};
use crate::gateway::{
    auth::BearerAuthenticator,
    config::Environment,
    request::GatewayRequest,
    routing::PrivacyDirective,
    service::{
        FixedWindowRateLimiter, MetadataAuthorizationError, OCTOROUTE_REQUEST_ID_HEADER,
        REQUEST_ID_HEADER, error_response, header_bytes, hold_response_guard, insert_header,
        metadata_authorization_error, rate_limit_response,
    },
    transport::{GatewayTransportError, PreparedUpstreamResponse},
};
use axum::{
    body::{Body, Bytes, to_bytes},
    http::{HeaderMap, Request, Response, StatusCode},
};
use futures::future::join_all;
use secrecy::SecretString;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
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

/// Executable v3 gateway service. This first runtime slice dispatches local-pool steps.
pub struct FabricGatewayService<T> {
    config: Arc<FabricConfig>,
    authenticator: BearerAuthenticator,
    pools: BTreeMap<String, LlamaCppPool>,
    transport: T,
    inbound_permits: Arc<Semaphore>,
    rate_limiter: FixedWindowRateLimiter,
}

/// Bounded v3 readiness snapshot.
#[derive(Debug, Clone)]
pub struct FabricReadiness {
    pools: BTreeMap<String, PoolAdmissionState>,
}

impl FabricReadiness {
    pub fn pools(&self) -> &BTreeMap<String, PoolAdmissionState> {
        &self.pools
    }

    pub fn is_ready(&self) -> bool {
        self.pools
            .values()
            .any(|state| *state == PoolAdmissionState::Ready)
    }
}

impl<T> FabricGatewayService<T>
where
    T: FabricUpstreamTransport,
{
    /// Build the v3 service and resolve only secrets needed by the inbound API and enabled pools.
    pub fn new(
        config: FabricConfig,
        environment: &impl Environment,
        transport: T,
    ) -> Result<Self, FabricGatewayServiceBuildError> {
        let inbound_key = resolve_secret(
            environment,
            "server.api_key_env",
            config.server.api_key_env.as_str(),
        )?;
        let mut pools = BTreeMap::new();
        for (name, pool_config) in &config.local_pools {
            if !pool_config.enabled {
                continue;
            }
            let pool = LlamaCppPool::new(pool_config, environment).map_err(|source| {
                FabricGatewayServiceBuildError::Pool {
                    pool: name.clone(),
                    source,
                }
            })?;
            pools.insert(name.clone(), pool);
        }

        Ok(Self {
            authenticator: BearerAuthenticator::new(inbound_key),
            inbound_permits: Arc::new(Semaphore::new(config.server.max_in_flight)),
            rate_limiter: FixedWindowRateLimiter::new(config.server.requests_per_minute),
            config: Arc::new(config),
            pools,
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

    pub fn authorize_metadata(
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

    /// Probe all configured local pools concurrently.
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
        }
    }

    /// Bootstrap Prometheus exposition while detailed v3 metrics are added.
    pub fn metrics_text(&self) -> String {
        let mut output = String::from(
            "# HELP octoroute_fabric_runtime_info V3 inference-fabric runtime information.\n\
                 # TYPE octoroute_fabric_runtime_info gauge\n\
                 octoroute_fabric_runtime_info{config_version=\"3\",provider_runtime=\"pending\"} 1\n\
                 # HELP octoroute_fabric_pool_enabled Whether a configured local pool is enabled.\n\
                 # TYPE octoroute_fabric_pool_enabled gauge\n",
        );
        for (name, pool) in &self.config.local_pools {
            let enabled = u8::from(pool.enabled);
            output.push_str(&format!(
                "octoroute_fabric_pool_enabled{{pool=\"{name}\"}} {enabled}\n"
            ));
        }
        output
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
                            let trigger = fallback_trigger(state);
                            if has_more
                                && trigger
                                    .is_some_and(|trigger| plan.fallback_on.contains(&trigger))
                            {
                                continue;
                            }
                            return pool_state_error(state, pool_name, request_id);
                        }
                    }
                }
                RouteTarget::Provider(provider_name) => {
                    tracing::info!(
                        request_id,
                        route = plan.model.as_str(),
                        provider = provider_name,
                        "v3 provider step reached before provider runtime is enabled"
                    );
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "the selected provider route is configured but its runtime adapter is not enabled yet",
                        "upstream_error",
                        "provider_runtime_unavailable",
                        request_id,
                    );
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
    pub fn from_config(
        config: FabricConfig,
        environment: &impl Environment,
    ) -> Result<Self, FabricGatewayServiceBuildError> {
        let transport = FabricTransport::new()?;
        Self::new(config, environment, transport)
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
    Transport(#[from] GatewayTransportError),
}

fn resolve_secret(
    environment: &impl Environment,
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
        PoolAdmissionState::Disabled | PoolAdmissionState::Unhealthy => {
            Some(FallbackTrigger::Unhealthy)
        }
        PoolAdmissionState::Incompatible => Some(FallbackTrigger::Incompatible),
        PoolAdmissionState::Busy => Some(FallbackTrigger::Busy),
        PoolAdmissionState::ContextOverflow => Some(FallbackTrigger::ContextOverflow),
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
