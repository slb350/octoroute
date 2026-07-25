//! llama.cpp health, capacity, and exact-context admission.

use crate::gateway::{
    config::LocalUpstreamConfig,
    http_client::{authorized, build},
    request::{GatewayRequest, GatewayRequestError},
    routing::LocalAdmissionState,
};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// Fail-closed result of checking whether Strix can accept a request now.
#[derive(Debug)]
pub enum AdmissionOutcome {
    /// All admission gates passed and the local capacity lease is held.
    Admitted(LocalLease),
    /// A bounded local state prevented admission.
    Rejected(LocalAdmissionState),
}

impl AdmissionOutcome {
    /// State consumed by deterministic route policy.
    pub fn state(&self) -> LocalAdmissionState {
        match self {
            Self::Admitted(_) => LocalAdmissionState::Ready,
            Self::Rejected(state) => *state,
        }
    }
}

/// Local capacity lease held until request completion or cancellation.
#[derive(Debug)]
pub struct LocalLease {
    permit: OwnedSemaphorePermit,
    request_body: bytes::Bytes,
}

impl LocalLease {
    pub(crate) fn into_parts(self) -> (bytes::Bytes, OwnedSemaphorePermit) {
        (self.request_body, self.permit)
    }
}

/// Construction failures detected before serving requests.
#[derive(Debug, Error)]
pub enum LlamaCppAdmissionBuildError {
    /// A validated same-origin path unexpectedly failed URL resolution.
    #[error("could not resolve configured llama.cpp path `{path}`")]
    InvalidPath {
        /// Configured path.
        path: String,
    },
    /// The shared HTTP client could not be built.
    #[error("could not build llama.cpp HTTP client")]
    HttpClient(#[source] reqwest::Error),
}

/// Non-blocking admission controller for one llama.cpp upstream.
#[derive(Clone)]
pub struct LlamaCppAdmission {
    inner: Arc<Inner>,
}

struct Inner {
    config: LocalUpstreamConfig,
    client: Client,
    health_url: Url,
    slots_url: Url,
    input_tokens_url: Url,
    permits: Arc<Semaphore>,
    cached_health: Mutex<Option<CachedHealth>>,
}

#[derive(Debug, Clone, Copy)]
struct CachedHealth {
    checked_at: Instant,
    healthy: bool,
}

#[derive(Debug, Deserialize)]
struct HealthResponse {
    status: String,
}

#[derive(Debug, Deserialize)]
struct SlotResponse {
    is_processing: bool,
}

#[derive(Debug, Deserialize)]
struct InputTokenResponse {
    input_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Idle,
    Busy,
    Unhealthy,
}

impl LlamaCppAdmission {
    /// Build an admission controller with an isolated rustls HTTP client.
    pub fn new(config: &LocalUpstreamConfig) -> Result<Self, LlamaCppAdmissionBuildError> {
        let client = build().map_err(LlamaCppAdmissionBuildError::HttpClient)?;
        Self::with_client(config, client)
    }

    pub(crate) fn with_client(
        config: &LocalUpstreamConfig,
        client: Client,
    ) -> Result<Self, LlamaCppAdmissionBuildError> {
        let health_url = resolve_path(config.base_url(), config.health_path())?;
        let slots_url = resolve_path(config.base_url(), config.slots_path())?;
        let input_tokens_url = resolve_path(config.base_url(), config.input_tokens_path())?;

        Ok(Self {
            inner: Arc::new(Inner {
                config: config.clone(),
                client,
                health_url,
                slots_url,
                input_tokens_url,
                permits: Arc::new(Semaphore::new(config.max_in_flight())),
                cached_health: Mutex::new(None),
            }),
        })
    }

    /// Try every local admission gate without waiting for Octoroute capacity.
    pub async fn try_admit(
        &self,
        request: &GatewayRequest,
    ) -> Result<AdmissionOutcome, GatewayRequestError> {
        let permit = match Arc::clone(&self.inner.permits).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return Ok(AdmissionOutcome::Rejected(LocalAdmissionState::Busy)),
        };

        let availability = self.availability_state().await;
        if availability != LocalAdmissionState::Ready {
            return Ok(AdmissionOutcome::Rejected(availability));
        }

        let output_tokens =
            request.output_token_budget(self.inner.config.default_max_output_tokens())?;
        let request_body = request.body_bytes_for_model(self.inner.config.model())?;
        let Some(input_tokens) = self.input_tokens(request_body.clone()).await else {
            return Ok(AdmissionOutcome::Rejected(LocalAdmissionState::Unhealthy));
        };
        let used_context = u64::from(input_tokens)
            + u64::from(output_tokens)
            + u64::from(self.inner.config.context_safety_tokens());
        if used_context > u64::from(self.inner.config.context_window()) {
            return Ok(AdmissionOutcome::Rejected(
                LocalAdmissionState::ContextOverflow,
            ));
        }

        Ok(AdmissionOutcome::Admitted(LocalLease {
            permit,
            request_body,
        }))
    }

    /// Snapshot local readiness without reserving capacity.
    pub async fn readiness_state(&self) -> LocalAdmissionState {
        if self.inner.permits.available_permits() == 0 {
            return LocalAdmissionState::Busy;
        }
        self.availability_state().await
    }

    async fn availability_state(&self) -> LocalAdmissionState {
        if let Some(healthy) = self.fresh_cached_health().await {
            return if healthy {
                admission_state(self.slot_state().await)
            } else {
                LocalAdmissionState::Unhealthy
            };
        }

        let (healthy, slot) = tokio::join!(self.refresh_health(), self.slot_state());
        if healthy {
            admission_state(slot)
        } else {
            LocalAdmissionState::Unhealthy
        }
    }

    async fn fresh_cached_health(&self) -> Option<bool> {
        let cached = self.inner.cached_health.lock().await;
        let ttl = Duration::from_millis(self.inner.config.health_cache_ttl_ms());
        cached
            .filter(|value| value.checked_at.elapsed() < ttl)
            .map(|value| value.healthy)
    }

    async fn refresh_health(&self) -> bool {
        let mut cached = self.inner.cached_health.lock().await;
        let ttl = Duration::from_millis(self.inner.config.health_cache_ttl_ms());
        if let Some(value) = *cached
            && value.checked_at.elapsed() < ttl
        {
            return value.healthy;
        }
        let healthy = self.probe_health().await;
        *cached = Some(CachedHealth {
            checked_at: Instant::now(),
            healthy,
        });
        healthy
    }

    async fn probe_health(&self) -> bool {
        match authorized(
            self.inner.client.get(self.inner.health_url.clone()),
            self.inner.config.api_key(),
        )
        .timeout(self.probe_timeout())
        .send()
        .await
        {
            Ok(response) if response.status().is_success() => response
                .json::<HealthResponse>()
                .await
                .is_ok_and(|response| response.status == "ok"),
            _ => false,
        }
    }

    async fn slot_state(&self) -> SlotState {
        let response = match authorized(
            self.inner.client.get(self.inner.slots_url.clone()),
            self.inner.config.api_key(),
        )
        .timeout(self.probe_timeout())
        .send()
        .await
        {
            Ok(response) => response,
            Err(_) => return SlotState::Unhealthy,
        };
        if response.status() == StatusCode::SERVICE_UNAVAILABLE {
            return SlotState::Busy;
        }
        if !response.status().is_success() {
            return SlotState::Unhealthy;
        }

        match response.json::<Vec<SlotResponse>>().await {
            Ok(slots) if slots.iter().any(|slot| !slot.is_processing) => SlotState::Idle,
            Ok(slots) if !slots.is_empty() => SlotState::Busy,
            _ => SlotState::Unhealthy,
        }
    }

    async fn input_tokens(&self, body: bytes::Bytes) -> Option<u32> {
        let response = authorized(
            self.inner.client.post(self.inner.input_tokens_url.clone()),
            self.inner.config.api_key(),
        )
        .timeout(self.probe_timeout())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .ok()?;
        if !response.status().is_success() {
            return None;
        }
        response
            .json::<InputTokenResponse>()
            .await
            .ok()
            .map(|response| response.input_tokens)
    }

    fn probe_timeout(&self) -> Duration {
        Duration::from_millis(self.inner.config.probe_timeout_ms())
    }
}

fn admission_state(slot: SlotState) -> LocalAdmissionState {
    match slot {
        SlotState::Idle => LocalAdmissionState::Ready,
        SlotState::Busy => LocalAdmissionState::Busy,
        SlotState::Unhealthy => LocalAdmissionState::Unhealthy,
    }
}

fn resolve_path(base: &Url, path: &str) -> Result<Url, LlamaCppAdmissionBuildError> {
    base.join(path)
        .map_err(|_| LlamaCppAdmissionBuildError::InvalidPath {
            path: path.to_string(),
        })
}
