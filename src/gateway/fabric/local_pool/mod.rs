//! Runtime admission for a pool of equivalent llama.cpp servers.
//!
//! [`member`] holds one member's health, slot, and token-count probes.

mod member;

use member::{InputTokenError, Member, MemberState};

use super::transport::UpstreamDeadlines;
use super::{LocalCapability, LocalPoolConfig};
use crate::gateway::{
    env::Environment,
    http_client::{LOCAL_CHAT_COMPLETIONS_PATH, build, endpoint_url},
    request::{GatewayRequest, GatewayRequestError, RequestFeature},
};
use bytes::Bytes;
use reqwest::{Client, Url};
use secrecy::{ExposeSecret, SecretString};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use thiserror::Error;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

const HEALTH_PATH: &str = "health";
const SLOTS_PATH: &str = "slots?fail_on_no_slot=1";
const INPUT_TOKENS_PATH: &str = "v1/chat/completions/input_tokens";
pub(super) const HEALTH_CACHE_TTL: Duration = Duration::from_secs(1);
pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// A member is otherwise healthy but its token-count endpoint did not
    /// answer usably, so exact context admission cannot be decided.
    ///
    /// `/v1/chat/completions/input_tokens` is not part of the OpenAI surface. A
    /// llama.cpp upgrade or a compatible-but-different server can drop it while
    /// `/health` and `/slots` still report a working member, and this state
    /// keeps that case distinguishable from an unreachable one.
    TokenCountUnavailable,
}

/// Lease held through the complete upstream response body.
pub struct PoolLease {
    pool: String,
    member: String,
    model_revision: String,
    chat_url: Url,
    api_key: Option<SecretString>,
    request_body: Bytes,
    deadlines: UpstreamDeadlines,
    _permit: OwnedSemaphorePermit,
}

/// Redacting `Debug`: a lease holds the serialized prompt and the member
/// credential, so a derived impl would write both to any log that formats it.
impl std::fmt::Debug for PoolLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PoolLease")
            .field("pool", &self.pool)
            .field("member", &self.member)
            .field("model_revision", &self.model_revision)
            .field("request_body", &"<redacted>")
            .field("api_key", &"<redacted>")
            .finish()
    }
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

    pub(crate) fn into_transport_parts(
        self,
    ) -> (
        Url,
        Option<SecretString>,
        Bytes,
        UpstreamDeadlines,
        OwnedSemaphorePermit,
    ) {
        (
            self.chat_url,
            self.api_key,
            self.request_body,
            self.deadlines,
            self._permit,
        )
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
    #[error(
        "credential referenced by local member `{member}` must use visible ASCII without whitespace"
    )]
    InvalidCredential { member: String },
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

impl LlamaCppPool {
    /// Build a local pool, resolving only the credential names referenced by members.
    pub fn new(
        config: &LocalPoolConfig,
        environment: &(impl Environment + ?Sized),
    ) -> Result<Self, LlamaCppPoolBuildError> {
        let client = build().map_err(LlamaCppPoolBuildError::HttpClient)?;
        Self::with_client(config, environment, client)
    }

    pub(crate) fn with_client(
        config: &LocalPoolConfig,
        environment: &(impl Environment + ?Sized),
        client: Client,
    ) -> Result<Self, LlamaCppPoolBuildError> {
        let mut members = Vec::new();
        for member in config.members.iter().filter(|member| member.enabled) {
            let api_key = match member.api_key_env.as_deref() {
                Some(name) => {
                    let value = environment
                        .get(name)
                        .filter(|value| !value.expose_secret().is_empty())
                        .ok_or_else(|| LlamaCppPoolBuildError::MissingEnvironmentVariable {
                            member: member.name.clone(),
                            name: name.to_string(),
                        })?;
                    if !value
                        .expose_secret()
                        .bytes()
                        .all(|byte| (0x21..=0x7e).contains(&byte))
                    {
                        return Err(LlamaCppPoolBuildError::InvalidCredential {
                            member: member.name.clone(),
                        });
                    }
                    Some(value)
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
                token_count_timeout: Duration::from_millis(config.token_count_timeout_ms),
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
        let request_body = request.body_bytes_for_model_with_reasoning_default(
            &self.inner.config.model,
            self.inner.config.default_reasoning_effort.as_str(),
        )?;
        let candidates = self.candidates();
        if candidates.is_empty() {
            return Ok(PoolAdmissionOutcome::Rejected(PoolAdmissionState::Busy));
        }

        let mut saw_busy = false;
        let mut saw_token_count_unavailable = false;
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
            let input_tokens = match member.input_tokens(request_body.clone()).await {
                Ok(input_tokens) => input_tokens,
                Err(InputTokenError::Transport) => continue,
                Err(InputTokenError::Unsupported) => {
                    saw_token_count_unavailable = true;
                    continue;
                }
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
                deadlines: UpstreamDeadlines::new(
                    self.inner.config.timeout_ms,
                    self.inner.config.first_byte_timeout_ms,
                ),
                _permit: permit,
            })));
        }

        Ok(PoolAdmissionOutcome::Rejected(if saw_busy {
            PoolAdmissionState::Busy
        } else if saw_token_count_unavailable {
            PoolAdmissionState::TokenCountUnavailable
        } else {
            PoolAdmissionState::Unhealthy
        }))
    }

    /// Return whether any member could accept a request now, without reserving it.
    ///
    /// This exercises every endpoint admission depends on, token counting
    /// included. Reporting `ready` from `/health` and `/slots` alone would let a
    /// pool whose token endpoint has gone missing look healthy indefinitely
    /// while rejecting every request it is offered.
    pub async fn readiness_state(&self) -> PoolAdmissionState {
        if !self.inner.config.enabled {
            return PoolAdmissionState::Disabled;
        }
        let probe_body = self.token_count_probe_body();
        let mut saw_busy = false;
        let mut saw_token_count_unavailable = false;
        for (_, member) in self.candidates_with_busy() {
            if member.permits.available_permits() == 0 {
                saw_busy = true;
                continue;
            }
            match member.availability_state().await {
                MemberState::Ready => {
                    match member.token_count_readiness(probe_body.clone()).await {
                        PoolAdmissionState::Ready => return PoolAdmissionState::Ready,
                        PoolAdmissionState::TokenCountUnavailable => {
                            saw_token_count_unavailable = true;
                        }
                        _ => {}
                    }
                }
                MemberState::Busy => saw_busy = true,
                MemberState::Unhealthy => {}
            }
        }
        if saw_busy {
            PoolAdmissionState::Busy
        } else if saw_token_count_unavailable {
            PoolAdmissionState::TokenCountUnavailable
        } else {
            PoolAdmissionState::Unhealthy
        }
    }

    /// Smallest well-formed body that exercises the token-count endpoint.
    ///
    /// Readiness carries no client prompt, so the probe body is fixed and
    /// contains no request-derived content.
    fn token_count_probe_body(&self) -> Bytes {
        Bytes::from(
            serde_json::to_vec(&serde_json::json!({
                "model": self.inner.config.model,
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .expect("the fixed readiness probe body serializes"),
        )
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
