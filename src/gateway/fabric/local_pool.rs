//! Runtime admission for a pool of equivalent llama.cpp servers.

use super::LocalPoolConfig;
use crate::gateway::{
    config::{Environment, LocalCapability},
    http_client::{LOCAL_CHAT_COMPLETIONS_PATH, authorized, build, endpoint_url},
    request::{GatewayRequest, GatewayRequestError, RequestFeature},
};
use bytes::Bytes;
use reqwest::{Client, StatusCode, Url};
use secrecy::SecretString;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

const HEALTH_PATH: &str = "health";
const SLOTS_PATH: &str = "slots?fail_on_no_slot=1";
const INPUT_TOKENS_PATH: &str = "v1/chat/completions/input_tokens";
const HEALTH_CACHE_TTL: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Fail-closed result of trying to admit one request to a local pool.
#[derive(Debug)]
pub enum PoolAdmissionOutcome {
    Admitted(Box<PoolLease>),
    Rejected(PoolAdmissionState),
}

impl PoolAdmissionOutcome {
    /// Bounded state suitable for route-policy and metrics mapping.
    pub fn state(&self) -> PoolAdmissionState {
        match self {
            Self::Admitted(_) => PoolAdmissionState::Ready,
            Self::Rejected(state) => *state,
        }
    }

    /// Consume an admitted outcome and return its lease.
    pub fn into_lease(self) -> Option<PoolLease> {
        match self {
            Self::Admitted(lease) => Some(*lease),
            Self::Rejected(_) => None,
        }
    }
}

/// Bounded local-pool admission state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolAdmissionState {
    Ready,
    Disabled,
    Incompatible,
    Busy,
    Unhealthy,
    ContextOverflow,
}

/// Lease held through the complete upstream response body.
#[derive(Debug)]
pub struct PoolLease {
    pool: String,
    member: String,
    model_revision: String,
    chat_url: Url,
    api_key: Option<SecretString>,
    request_body: Bytes,
    _permit: OwnedSemaphorePermit,
}

impl PoolLease {
    pub fn pool(&self) -> &str {
        &self.pool
    }

    pub fn member(&self) -> &str {
        &self.member
    }

    pub fn model_revision(&self) -> &str {
        &self.model_revision
    }

    pub fn chat_url(&self) -> &Url {
        &self.chat_url
    }

    pub fn api_key(&self) -> Option<&SecretString> {
        self.api_key.as_ref()
    }

    pub fn request_body(&self) -> &Bytes {
        &self.request_body
    }
}

/// Build failures detected before the v3 service starts accepting requests.
#[derive(Debug, Error)]
pub enum LlamaCppPoolBuildError {
    #[error("local pool `{pool}` has no enabled members")]
    NoEnabledMembers { pool: String },
    #[error(
        "environment variable `{name}` required by local member `{member}` is missing or empty"
    )]
    MissingEnvironmentVariable { member: String, name: String },
    #[error("could not resolve `{path}` for local member `{member}`")]
    InvalidPath { member: String, path: String },
    #[error("could not build isolated llama.cpp HTTP client")]
    HttpClient(#[source] reqwest::Error),
}

/// Independently admitted pool of equivalent llama.cpp replicas.
#[derive(Clone)]
pub struct LlamaCppPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    config: LocalPoolConfig,
    members: Vec<Arc<Member>>,
    cursor: AtomicUsize,
}

struct Member {
    name: String,
    priority: u16,
    api_key: Option<SecretString>,
    client: Client,
    health_url: Url,
    slots_url: Url,
    input_tokens_url: Url,
    chat_url: Url,
    permits: Arc<Semaphore>,
    max_in_flight: usize,
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
enum MemberState {
    Ready,
    Busy,
    Unhealthy,
}

impl LlamaCppPool {
    /// Build a local pool, resolving only the credential names referenced by members.
    pub fn new(
        config: &LocalPoolConfig,
        environment: &impl Environment,
    ) -> Result<Self, LlamaCppPoolBuildError> {
        let client = build().map_err(LlamaCppPoolBuildError::HttpClient)?;
        Self::with_client(config, environment, client)
    }

    pub(crate) fn with_client(
        config: &LocalPoolConfig,
        environment: &impl Environment,
        client: Client,
    ) -> Result<Self, LlamaCppPoolBuildError> {
        let mut members = Vec::new();
        for member in config.members.iter().filter(|member| member.enabled) {
            let api_key = match member.api_key_env.as_deref() {
                Some(name) => {
                    let value = environment
                        .get(name)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| LlamaCppPoolBuildError::MissingEnvironmentVariable {
                            member: member.name.clone(),
                            name: name.to_string(),
                        })?;
                    Some(SecretString::from(value))
                }
                None => None,
            };
            members.push(Arc::new(Member {
                name: member.name.clone(),
                priority: member.priority,
                api_key,
                client: client.clone(),
                health_url: resolve(&member.base_url, HEALTH_PATH, &member.name)?,
                slots_url: resolve(&member.base_url, SLOTS_PATH, &member.name)?,
                input_tokens_url: resolve(&member.base_url, INPUT_TOKENS_PATH, &member.name)?,
                chat_url: resolve(&member.base_url, LOCAL_CHAT_COMPLETIONS_PATH, &member.name)?,
                permits: Arc::new(Semaphore::new(member.max_in_flight)),
                max_in_flight: member.max_in_flight,
                cached_health: Mutex::new(None),
            }));
        }
        if members.is_empty() {
            return Err(LlamaCppPoolBuildError::NoEnabledMembers {
                pool: config.name.clone(),
            });
        }
        Ok(Self {
            inner: Arc::new(PoolInner {
                config: config.clone(),
                members,
                cursor: AtomicUsize::new(0),
            }),
        })
    }

    /// Try every eligible member without waiting for local capacity.
    pub async fn try_admit(
        &self,
        request: &GatewayRequest,
    ) -> Result<PoolAdmissionOutcome, GatewayRequestError> {
        if !self.inner.config.enabled {
            return Ok(PoolAdmissionOutcome::Rejected(PoolAdmissionState::Disabled));
        }
        let capabilities = match request_capabilities(request) {
            Ok(capabilities) => capabilities,
            Err(state) => return Ok(PoolAdmissionOutcome::Rejected(state)),
        };
        if !capabilities
            .iter()
            .all(|capability| self.inner.config.capabilities.contains(capability))
        {
            return Ok(PoolAdmissionOutcome::Rejected(
                PoolAdmissionState::Incompatible,
            ));
        }

        let output_tokens =
            request.output_token_budget(self.inner.config.default_max_output_tokens)?;
        let request_body = request.body_bytes_for_model(&self.inner.config.model)?;
        let candidates = self.candidates();
        if candidates.is_empty() {
            return Ok(PoolAdmissionOutcome::Rejected(PoolAdmissionState::Busy));
        }

        let mut saw_busy = false;
        for (index, member) in candidates {
            let permit = match Arc::clone(&member.permits).try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    saw_busy = true;
                    continue;
                }
            };
            match member.availability_state().await {
                MemberState::Busy => {
                    saw_busy = true;
                    continue;
                }
                MemberState::Unhealthy => continue,
                MemberState::Ready => {}
            }
            let Some(input_tokens) = member.input_tokens(request_body.clone()).await else {
                continue;
            };
            let used_context = u64::from(input_tokens)
                + u64::from(output_tokens)
                + u64::from(self.inner.config.context_safety_tokens);
            if used_context > u64::from(self.inner.config.context_window) {
                return Ok(PoolAdmissionOutcome::Rejected(
                    PoolAdmissionState::ContextOverflow,
                ));
            }

            self.inner
                .cursor
                .store((index + 1) % self.inner.members.len(), Ordering::Relaxed);
            return Ok(PoolAdmissionOutcome::Admitted(Box::new(PoolLease {
                pool: self.inner.config.name.clone(),
                member: member.name.clone(),
                model_revision: self.inner.config.model_revision.clone(),
                chat_url: member.chat_url.clone(),
                api_key: member.api_key.clone(),
                request_body,
                _permit: permit,
            })));
        }

        Ok(PoolAdmissionOutcome::Rejected(if saw_busy {
            PoolAdmissionState::Busy
        } else {
            PoolAdmissionState::Unhealthy
        }))
    }

    /// Return whether any member could accept a request now, without reserving it.
    pub async fn readiness_state(&self) -> PoolAdmissionState {
        if !self.inner.config.enabled {
            return PoolAdmissionState::Disabled;
        }
        let mut saw_busy = false;
        for (_, member) in self.candidates_with_busy() {
            if member.permits.available_permits() == 0 {
                saw_busy = true;
                continue;
            }
            match member.availability_state().await {
                MemberState::Ready => return PoolAdmissionState::Ready,
                MemberState::Busy => saw_busy = true,
                MemberState::Unhealthy => {}
            }
        }
        if saw_busy {
            PoolAdmissionState::Busy
        } else {
            PoolAdmissionState::Unhealthy
        }
    }

    fn candidates(&self) -> Vec<(usize, Arc<Member>)> {
        self.candidates_with_busy()
            .into_iter()
            .filter(|(_, member)| member.permits.available_permits() > 0)
            .collect()
    }

    fn candidates_with_busy(&self) -> Vec<(usize, Arc<Member>)> {
        let cursor = self.inner.cursor.load(Ordering::Relaxed) % self.inner.members.len();
        let count = self.inner.members.len();
        let mut candidates = self
            .inner
            .members
            .iter()
            .enumerate()
            .map(|(index, member)| {
                let in_flight = member.max_in_flight - member.permits.available_permits();
                let rotation = (index + count - cursor) % count;
                (
                    in_flight,
                    member.priority,
                    rotation,
                    index,
                    Arc::clone(member),
                )
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(left.1.cmp(&right.1))
                .then(left.2.cmp(&right.2))
                .then_with(|| left.4.name.cmp(&right.4.name))
        });
        candidates
            .into_iter()
            .map(|(_, _, _, index, member)| (index, member))
            .collect()
    }
}

impl Member {
    async fn availability_state(&self) -> MemberState {
        if let Some(healthy) = self.fresh_cached_health().await {
            return if healthy {
                self.slot_state().await
            } else {
                MemberState::Unhealthy
            };
        }
        let (healthy, slot) = tokio::join!(self.refresh_health(), self.slot_state());
        if healthy {
            slot
        } else {
            MemberState::Unhealthy
        }
    }

    async fn fresh_cached_health(&self) -> Option<bool> {
        let cached = self.cached_health.lock().await;
        cached
            .filter(|value| value.checked_at.elapsed() < HEALTH_CACHE_TTL)
            .map(|value| value.healthy)
    }

    async fn refresh_health(&self) -> bool {
        let mut cached = self.cached_health.lock().await;
        if let Some(value) = *cached
            && value.checked_at.elapsed() < HEALTH_CACHE_TTL
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
            self.client.get(self.health_url.clone()),
            self.api_key.as_ref(),
        )
        .timeout(PROBE_TIMEOUT)
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

    async fn slot_state(&self) -> MemberState {
        let response = match authorized(
            self.client.get(self.slots_url.clone()),
            self.api_key.as_ref(),
        )
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        {
            Ok(response) => response,
            Err(_) => return MemberState::Unhealthy,
        };
        if response.status() == StatusCode::SERVICE_UNAVAILABLE {
            return MemberState::Busy;
        }
        if !response.status().is_success() {
            return MemberState::Unhealthy;
        }
        match response.json::<Vec<SlotResponse>>().await {
            Ok(slots) if slots.iter().any(|slot| !slot.is_processing) => MemberState::Ready,
            Ok(slots) if !slots.is_empty() => MemberState::Busy,
            _ => MemberState::Unhealthy,
        }
    }

    async fn input_tokens(&self, body: Bytes) -> Option<u32> {
        let response = authorized(
            self.client.post(self.input_tokens_url.clone()),
            self.api_key.as_ref(),
        )
        .timeout(PROBE_TIMEOUT)
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
}

fn request_capabilities(
    request: &GatewayRequest,
) -> Result<BTreeSet<LocalCapability>, PoolAdmissionState> {
    let mut capabilities = BTreeSet::new();
    for feature in request.features() {
        match feature {
            RequestFeature::Capability(capability) => {
                capabilities.insert(*capability);
            }
            RequestFeature::OpenRouterPlugins
            | RequestFeature::NonTextOutput
            | RequestFeature::UnsupportedContent => {
                return Err(PoolAdmissionState::Incompatible);
            }
        }
    }
    Ok(capabilities)
}

fn resolve(base: &Url, path: &str, member: &str) -> Result<Url, LlamaCppPoolBuildError> {
    endpoint_url(base, path).ok_or_else(|| LlamaCppPoolBuildError::InvalidPath {
        member: member.to_string(),
        path: path.to_string(),
    })
}
