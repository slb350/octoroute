//! Lazy, credential-isolated runtime registry for configured inference providers.
//!
//! [`credential`] resolves and caches provider credentials; [`body`] builds the
//! OpenAI-protocol request body; [`readiness`] owns the cached probes.

mod body;
mod credential;
mod readiness;

#[cfg(test)]
mod mutation_tests;
#[cfg(test)]
mod tests;

use body::build_open_ai_body;
use credential::{CachedCredential, ProviderCredentialSource};
use readiness::CachedReadiness;

pub(super) use readiness::authorize_http;

use super::{
    ProviderConfig, ProviderCredentialConfig, ProviderProfile, ProviderProtocol,
    ProviderRuntimeConfig, ReasoningEffort, anthropic,
    codex::{self, ChildEnvironment, CodexRequest},
    metrics::FabricMetrics,
    transport::UpstreamDeadlines,
};
use crate::gateway::{
    env::Environment,
    http_client::endpoint_url,
    request::{GatewayRequest, GatewayRequestError},
};
use axum::http::StatusCode;
use bytes::Bytes;
use reqwest::{Client, Url};
use secrecy::SecretString;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};
use thiserror::Error;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

const CHAT_COMPLETIONS_PATH: &str = "chat/completions";
const ANTHROPIC_MESSAGES_PATH: &str = "messages";
const MODELS_PATH: &str = "models";

/// Whether an upstream or its authenticating proxy refused Octoroute's credential.
pub(super) const fn is_upstream_credential_rejection(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::PROXY_AUTHENTICATION_REQUIRED
    )
}

/// Bounded provider state used by readiness, fallback policy, and error mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAdmissionState {
    Ready,
    Disabled,
    Incompatible,
    Busy,
    Unavailable,
    /// The provider is reachable but its credential is missing, malformed, or
    /// rejected.
    ///
    /// Deliberately distinct from `Unavailable`: an expired key is an operator
    /// error, and silently rerouting the traffic and the spend it carries to the
    /// next provider hides it. This state is outside the default fallback
    /// trigger set.
    Unauthenticated,
}

/// Result of attempting to reserve one configured provider.
pub enum ProviderAdmissionOutcome {
    Admitted(Box<ProviderLease>),
    Rejected(ProviderAdmissionState),
}

/// One provider dispatch with its isolated credential/process state and permit.
pub struct ProviderLease {
    provider: String,
    model: String,
    dispatch: ProviderDispatch,
    deadlines: UpstreamDeadlines,
    _permit: OwnedSemaphorePermit,
}

impl ProviderLease {
    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub(super) fn into_transport_parts(
        self,
    ) -> (ProviderDispatch, UpstreamDeadlines, OwnedSemaphorePermit) {
        (self.dispatch, self.deadlines, self._permit)
    }
}

pub(super) enum ProviderDispatch {
    Http(HttpProviderDispatch),
    Codex(CodexRequest),
}

pub(super) struct HttpProviderDispatch {
    pub(super) url: Url,
    pub(super) model: String,
    pub(super) api_key: SecretString,
    pub(super) body: Bytes,
    pub(super) adapter: ProviderHttpAdapter,
    pub(super) openrouter_profile: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ProviderHttpAdapter {
    OpenAi,
    Anthropic { stream: bool },
}

impl ProviderHttpAdapter {
    /// The wire protocol this adapter speaks.
    ///
    /// The transport authorizes its dispatch through the same
    /// [`authorize_http`] the readiness probe uses, so the credential header
    /// and the Anthropic version are defined once.
    pub(super) const fn protocol(self) -> ProviderProtocol {
        match self {
            Self::OpenAi => ProviderProtocol::OpenAi,
            Self::Anthropic { .. } => ProviderProtocol::Anthropic,
        }
    }
}

/// Runtime providers keyed only by validated configuration names.
pub struct ProviderRegistry {
    providers: BTreeMap<String, ProviderRuntime>,
}

enum ProviderRuntime {
    Disabled { metrics: Arc<FabricMetrics> },
    Http(Box<HttpProvider>),
    Codex(Box<CodexProvider>),
}

struct HttpProvider {
    config: ProviderConfig,
    protocol: ProviderProtocol,
    request_url: Url,
    models_url: Url,
    credential: CachedCredential,
    client: Client,
    permits: Arc<Semaphore>,
    readiness: Mutex<CachedReadiness>,
    metrics: Arc<FabricMetrics>,
}

struct CodexProvider {
    config: ProviderConfig,
    executable: PathBuf,
    environment: ChildEnvironment,
    permits: Arc<Semaphore>,
    readiness: Mutex<CachedReadiness>,
    metrics: Arc<FabricMetrics>,
}

impl ProviderRegistry {
    /// Report that a real dispatch contradicted cached readiness.
    ///
    /// Cached readiness is otherwise only refreshed on its TTL, so a provider
    /// that dies right after a successful probe keeps reporting `ready` for up
    /// to an hour while every request through it fails.
    pub(super) async fn record_dispatch_failure(&self, provider: &str, status: Option<StatusCode>) {
        match self.providers.get(provider) {
            Some(ProviderRuntime::Http(provider)) => provider.invalidate_readiness(status).await,
            Some(ProviderRuntime::Codex(provider)) => provider.invalidate_readiness(status).await,
            Some(ProviderRuntime::Disabled { .. }) | None => {}
        }
    }

    /// Build adapters without resolving credentials or launching commands.
    pub(super) fn new(
        configs: &BTreeMap<String, ProviderConfig>,
        environment: Arc<dyn Environment + Send + Sync>,
        metrics: Arc<FabricMetrics>,
        client: Client,
    ) -> Result<Self, ProviderRegistryBuildError> {
        let codex_environment = ChildEnvironment::current();
        let mut providers = BTreeMap::new();
        for (name, config) in configs {
            let runtime = if !config.enabled {
                ProviderRuntime::Disabled {
                    metrics: Arc::clone(&metrics),
                }
            } else {
                match &config.runtime {
                    ProviderRuntimeConfig::Http {
                        endpoint,
                        protocol,
                        credential,
                    } => ProviderRuntime::Http(Box::new(HttpProvider::new(
                        config,
                        endpoint,
                        *protocol,
                        credential,
                        Arc::clone(&environment),
                        client.clone(),
                        Arc::clone(&metrics),
                    )?)),
                    ProviderRuntimeConfig::CodexCli { executable } => {
                        ProviderRuntime::Codex(Box::new(CodexProvider::new(
                            config,
                            executable,
                            codex_environment.clone(),
                            Arc::clone(&metrics),
                        )))
                    }
                }
            };
            providers.insert(name.clone(), runtime);
        }
        Ok(Self { providers })
    }

    /// Reserve and prepare one configured provider request.
    pub(super) async fn try_admit(
        &self,
        name: &str,
        request: &GatewayRequest,
        route_reasoning_effort: ReasoningEffort,
    ) -> Result<ProviderAdmissionOutcome, ProviderRequestError> {
        let Some(runtime) = self.providers.get(name) else {
            return Ok(ProviderAdmissionOutcome::Rejected(
                ProviderAdmissionState::Unavailable,
            ));
        };
        match runtime {
            ProviderRuntime::Disabled { .. } => Ok(ProviderAdmissionOutcome::Rejected(
                ProviderAdmissionState::Disabled,
            )),
            ProviderRuntime::Http(provider) => provider.try_admit(request).await,
            ProviderRuntime::Codex(provider) => {
                provider.try_admit(request, route_reasoning_effort).await
            }
        }
    }

    /// Run cached, bounded authentication/reachability probes concurrently.
    pub(super) async fn readiness(
        &self,
        reachable: &BTreeSet<String>,
    ) -> BTreeMap<String, ProviderAdmissionState> {
        let probes = self.readiness_targets(reachable).map(|(name, runtime)| {
            let name = name.clone();
            async move {
                let state = match runtime {
                    ProviderRuntime::Disabled { metrics } => {
                        metrics.record_probe(&name, ProviderAdmissionState::Disabled);
                        ProviderAdmissionState::Disabled
                    }
                    ProviderRuntime::Http(provider) => provider.readiness().await,
                    ProviderRuntime::Codex(provider) => provider.readiness().await,
                };
                (name, state)
            }
        });
        futures::future::join_all(probes)
            .await
            .into_iter()
            .collect()
    }

    fn readiness_targets<'a>(
        &'a self,
        reachable: &'a BTreeSet<String>,
    ) -> impl Iterator<Item = (&'a String, &'a ProviderRuntime)> {
        reachable
            .iter()
            .filter_map(|name| self.providers.get_key_value(name))
    }
}

impl HttpProvider {
    fn new(
        config: &ProviderConfig,
        endpoint: &Url,
        protocol: ProviderProtocol,
        credential: &ProviderCredentialConfig,
        environment: Arc<dyn Environment + Send + Sync>,
        client: Client,
        metrics: Arc<FabricMetrics>,
    ) -> Result<Self, ProviderRegistryBuildError> {
        let path = match protocol {
            ProviderProtocol::OpenAi => CHAT_COMPLETIONS_PATH,
            ProviderProtocol::Anthropic => ANTHROPIC_MESSAGES_PATH,
        };
        let request_url = endpoint_url(endpoint, path).ok_or_else(|| invalid_provider(config))?;
        let models_url =
            endpoint_url(endpoint, MODELS_PATH).ok_or_else(|| invalid_provider(config))?;
        let credential = match credential {
            ProviderCredentialConfig::Environment(name) => ProviderCredentialSource::Environment {
                name: name.clone(),
                environment,
            },
            ProviderCredentialConfig::Command(command) => ProviderCredentialSource::Command {
                command: command.clone(),
                environment: ChildEnvironment::current(),
            },
        };
        Ok(Self {
            config: config.clone(),
            protocol,
            request_url,
            models_url,
            credential: CachedCredential::new(credential),
            client,
            permits: Arc::new(Semaphore::new(config.max_in_flight)),
            readiness: Mutex::new(CachedReadiness::default()),
            metrics,
        })
    }

    async fn try_admit(
        &self,
        request: &GatewayRequest,
    ) -> Result<ProviderAdmissionOutcome, ProviderRequestError> {
        let permit = match Arc::clone(&self.permits).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return Ok(ProviderAdmissionOutcome::Rejected(
                    ProviderAdmissionState::Busy,
                ));
            }
        };
        let adapter = match self.protocol {
            ProviderProtocol::OpenAi => ProviderHttpAdapter::OpenAi,
            ProviderProtocol::Anthropic => ProviderHttpAdapter::Anthropic {
                stream: request.is_stream(),
            },
        };
        let body = match adapter {
            ProviderHttpAdapter::OpenAi => build_open_ai_body(&self.config, request)?,
            ProviderHttpAdapter::Anthropic { .. } => {
                match anthropic::build_request(&self.config, request) {
                    Ok(request) => request.body,
                    Err(error) => return classify_anthropic_build_error(error),
                }
            }
        };
        let api_key = match self.credential.resolve().await {
            Ok(api_key) => api_key,
            Err(error) => {
                tracing::warn!(
                    provider = self.config.name.as_str(),
                    reason = error.code(),
                    "provider credential could not be resolved"
                );
                return Ok(ProviderAdmissionOutcome::Rejected(
                    ProviderAdmissionState::Unauthenticated,
                ));
            }
        };
        Ok(ProviderAdmissionOutcome::Admitted(Box::new(
            ProviderLease {
                provider: self.config.name.clone(),
                model: self.config.model.clone(),
                dispatch: ProviderDispatch::Http(HttpProviderDispatch {
                    url: self.request_url.clone(),
                    model: self.config.model.clone(),
                    api_key,
                    body,
                    adapter,
                    openrouter_profile: self.config.profile == ProviderProfile::OpenRouterAuto,
                }),
                deadlines: UpstreamDeadlines::new(
                    self.config.timeout_ms,
                    self.config.first_byte_timeout_ms,
                ),
                _permit: permit,
            },
        )))
    }
}

impl CodexProvider {
    fn new(
        config: &ProviderConfig,
        executable: &str,
        environment: ChildEnvironment,
        metrics: Arc<FabricMetrics>,
    ) -> Self {
        Self {
            config: config.clone(),
            executable: PathBuf::from(executable),
            environment,
            permits: Arc::new(Semaphore::new(config.max_in_flight)),
            readiness: Mutex::new(CachedReadiness::default()),
            metrics,
        }
    }

    async fn try_admit(
        &self,
        request: &GatewayRequest,
        route_reasoning_effort: ReasoningEffort,
    ) -> Result<ProviderAdmissionOutcome, ProviderRequestError> {
        let permit = match Arc::clone(&self.permits).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return Ok(ProviderAdmissionOutcome::Rejected(
                    ProviderAdmissionState::Busy,
                ));
            }
        };
        let request = match codex::build_request(
            &self.config,
            &self.executable,
            request,
            route_reasoning_effort,
            self.environment.clone(),
        ) {
            Ok(request) => request,
            Err(error) => return classify_codex_build_error(error),
        };
        Ok(ProviderAdmissionOutcome::Admitted(Box::new(
            ProviderLease {
                provider: self.config.name.clone(),
                model: self.config.model.clone(),
                dispatch: ProviderDispatch::Codex(request),
                deadlines: UpstreamDeadlines::new(
                    self.config.timeout_ms,
                    self.config.first_byte_timeout_ms,
                ),
                _permit: permit,
            },
        )))
    }
}

fn classify_anthropic_build_error(
    error: anthropic::AnthropicAdapterError,
) -> Result<ProviderAdmissionOutcome, ProviderRequestError> {
    if error.is_incompatible() {
        Ok(ProviderAdmissionOutcome::Rejected(
            ProviderAdmissionState::Incompatible,
        ))
    } else {
        Err(ProviderRequestError::Anthropic)
    }
}

fn classify_codex_build_error(
    error: codex::CodexAdapterError,
) -> Result<ProviderAdmissionOutcome, ProviderRequestError> {
    if error.is_incompatible() {
        Ok(ProviderAdmissionOutcome::Rejected(
            ProviderAdmissionState::Incompatible,
        ))
    } else {
        Err(ProviderRequestError::Codex)
    }
}

fn invalid_provider(config: &ProviderConfig) -> ProviderRegistryBuildError {
    ProviderRegistryBuildError::InvalidProvider {
        provider: config.name.clone(),
    }
}

/// Provider registry construction failures that never include credentials.
#[derive(Debug, Error)]
pub enum ProviderRegistryBuildError {
    #[error("configured provider `{provider}` cannot be constructed")]
    InvalidProvider { provider: String },
    #[error("could not build provider HTTP client")]
    HttpClient(#[source] reqwest::Error),
}

/// Safe provider request construction failures.
#[derive(Debug, Error)]
pub enum ProviderRequestError {
    #[error("invalid OpenRouter Auto plugins shape")]
    InvalidOpenRouterPlugins,
    #[error("could not serialize the provider request")]
    Serialization,
    #[error("Anthropic-compatible request translation failed")]
    Anthropic,
    #[error("Codex CLI request translation failed")]
    Codex,
    #[error(transparent)]
    Request(#[from] GatewayRequestError),
}

impl ProviderRequestError {
    /// Whether the caller's request body caused this construction failure.
    ///
    /// Serialization and adapter failures belong to the gateway. Classifying
    /// them here keeps routing policy and HTTP rendering from independently
    /// inspecting provider internals.
    pub(super) fn is_client_error(&self) -> bool {
        matches!(
            self,
            Self::InvalidOpenRouterPlugins
                | Self::Request(
                    GatewayRequestError::Json { .. } | GatewayRequestError::Invalid { .. }
                )
        )
    }
}
