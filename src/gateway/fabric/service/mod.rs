//! Authenticated v3 orchestration for virtual routes and local inference pools.
//!
//! [`routing`] holds the route executor and [`responses`] the error mapping and
//! response decoration, so this module stays the service surface itself.

mod responses;
mod routing;

use responses::{resolve_secret, route_error};

use super::http_support::{
    FixedWindowRateLimiter, MetadataAuthorizationError, error_response, header_bytes,
    hold_response_guard, metadata_authorization_error, rate_limit_response,
};
use super::metrics::FabricMetrics;
use super::{
    FabricConfig, FabricTransport, FabricTransportError, FabricUpstreamTransport, LlamaCppPool,
    LlamaCppPoolBuildError, PoolAdmissionState, PrivacyDirective, ProviderAdmissionState,
    ProviderRegistry, ProviderRegistryBuildError,
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
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

const DESTINATION_HEADER: &str = "x-octoroute-destination";
const REASON_HEADER: &str = "x-octoroute-reason";
const UPSTREAM_HEADER: &str = "x-octoroute-upstream";
const ROUTE_HEADER: &str = "x-octoroute-route";
const POOL_HEADER: &str = "x-octoroute-pool";
const MEMBER_HEADER: &str = "x-octoroute-member";
const MODEL_REVISION_HEADER: &str = "x-octoroute-model-revision";
const PROVIDER_HEADER: &str = "x-octoroute-provider";
pub(super) const TARGET_HEADER: &str = "x-octoroute-target";

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
    readiness_cache: Mutex<Option<(Instant, FabricReadiness)>>,
}

/// How long an unauthenticated readiness answer may be reused.
const READINESS_SNAPSHOT_TTL: Duration = Duration::from_secs(5);

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

    /// Whether the gateway can serve a request now.
    ///
    /// Any ready target makes the gateway ready. `degraded` is the finer signal:
    /// a gateway whose whole local fleet is down but whose cloud providers are
    /// up will answer every request, and bill for it, while `/health` says
    /// nothing is wrong.
    pub fn is_ready(&self) -> bool {
        self.pools
            .values()
            .any(|state| *state == PoolAdmissionState::Ready)
            || self
                .providers
                .values()
                .any(|state| *state == ProviderAdmissionState::Ready)
    }

    /// Whether some configured target is unavailable while others still serve.
    pub fn is_degraded(&self) -> bool {
        let unready_pools = self.pools.values().any(|state| {
            !matches!(
                state,
                PoolAdmissionState::Ready | PoolAdmissionState::Busy | PoolAdmissionState::Disabled
            )
        });
        let unready_providers = self.providers.values().any(|state| {
            !matches!(
                state,
                ProviderAdmissionState::Ready
                    | ProviderAdmissionState::Busy
                    | ProviderAdmissionState::Disabled
            )
        });
        self.is_ready() && (unready_pools || unready_providers)
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
            readiness_cache: Mutex::new(None),
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

    /// Return a readiness snapshot, reusing a recent one when it is fresh.
    ///
    /// `/health/ready` and `/health` are unauthenticated by contract, and a
    /// readiness pass spawns `codex doctor` and sends credentialed `/models`
    /// probes. Without this an anonymous caller on the default `0.0.0.0` bind
    /// can amplify one cheap request into a subprocess spawn and a set of
    /// outbound requests, as fast as it can issue them.
    pub async fn cached_readiness(&self) -> FabricReadiness {
        // The guard is held across the probe, and the TTL re-checked after
        // acquiring it, so concurrent callers coalesce onto one pass instead of
        // each running their own. Dropping the lock first would leave the
        // amplification this cache exists to prevent: N simultaneous requests
        // would still produce N `codex doctor` spawns and N probe sets.
        // `Member::refresh_health` uses the same shape.
        let mut cached = self.readiness_cache.lock().await;
        if let Some((probed_at, readiness)) = cached.as_ref()
            && probed_at.elapsed() < READINESS_SNAPSHOT_TTL
        {
            return readiness.clone();
        }
        let readiness = self.readiness().await;
        *cached = Some((Instant::now(), readiness.clone()));
        readiness
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
