//! Lazy, credential-isolated runtime registry for configured inference providers.

use super::{
    ProviderConfig, ProviderKind, ProviderProfile, ProviderProtocol, ReasoningEffort, anthropic,
    codex::{self, ChildEnvironment, CodexRequest},
    metrics::FabricMetrics,
};
use crate::gateway::{
    env::Environment,
    http_client::endpoint_url,
    request::{GatewayRequest, GatewayRequestError},
};
use axum::http::StatusCode;
use bytes::Bytes;
use reqwest::{Client, RequestBuilder, Url};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Number, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
};

const CHAT_COMPLETIONS_PATH: &str = "chat/completions";
const ANTHROPIC_MESSAGES_PATH: &str = "messages";
const MODELS_PATH: &str = "models";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const OPENROUTER_AUTO_PLUGIN: &str = "auto-router";
const OPENROUTER_COST_QUALITY_TRADEOFF: u64 = 9;
const MAX_CREDENTIAL_BYTES: usize = 4 * 1024;
const CREDENTIAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

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
    timeout: Duration,
    _permit: OwnedSemaphorePermit,
}

impl ProviderLease {
    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub(super) fn into_transport_parts(self) -> (ProviderDispatch, Duration, OwnedSemaphorePermit) {
        (self.dispatch, self.timeout, self._permit)
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

/// Runtime providers keyed only by validated configuration names.
pub struct ProviderRegistry {
    providers: BTreeMap<String, ProviderRuntime>,
}

enum ProviderRuntime {
    Disabled { metrics: Arc<FabricMetrics> },
    Http(Arc<HttpProvider>),
    Codex(Arc<CodexProvider>),
}

struct HttpProvider {
    config: ProviderConfig,
    request_url: Url,
    models_url: Url,
    credential: CachedCredential,
    client: Client,
    permits: Arc<Semaphore>,
    readiness: Mutex<CachedReadiness>,
    metrics: Arc<FabricMetrics>,
}

/// Longest a resolved provider credential is reused before being re-resolved.
const CREDENTIAL_CACHE_TTL: Duration = Duration::from_secs(300);

/// A resolved credential held for a bounded time.
///
/// Without this, `api_key_command` spawns a subprocess on every request and
/// again on every readiness refresh. The cache is invalidated whenever the
/// provider answers 401 or 403, so a rotated key is picked up without a restart.
struct CachedCredential {
    source: ProviderCredentialSource,
    cached: Mutex<Option<(Instant, SecretString)>>,
}

impl CachedCredential {
    fn new(source: ProviderCredentialSource) -> Self {
        Self {
            source,
            cached: Mutex::new(None),
        }
    }

    async fn resolve(&self) -> Result<SecretString, ProviderCredentialError> {
        {
            let cached = self.cached.lock().await;
            if let Some((resolved_at, value)) = cached.as_ref()
                && resolved_at.elapsed() < CREDENTIAL_CACHE_TTL
            {
                return Ok(value.clone());
            }
        }
        let value = self.source.resolve().await?;
        *self.cached.lock().await = Some((Instant::now(), value.clone()));
        Ok(value)
    }

    /// Discard the cached credential after the provider rejected it.
    async fn invalidate(&self) {
        self.cached.lock().await.take();
    }
}

struct CodexProvider {
    config: ProviderConfig,
    executable: PathBuf,
    environment: ChildEnvironment,
    permits: Arc<Semaphore>,
    readiness: Mutex<CachedReadiness>,
    metrics: Arc<FabricMetrics>,
}

#[derive(Clone, Copy)]
struct CachedReadiness {
    checked_at: Option<Instant>,
    state: ProviderAdmissionState,
}

impl Default for CachedReadiness {
    fn default() -> Self {
        Self {
            checked_at: None,
            state: ProviderAdmissionState::Unavailable,
        }
    }
}

enum ProviderCredentialSource {
    Environment {
        name: String,
        environment: Arc<dyn Environment + Send + Sync>,
    },
    Command {
        command: Vec<String>,
        environment: ChildEnvironment,
    },
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
                match config.kind {
                    ProviderKind::Http => ProviderRuntime::Http(Arc::new(HttpProvider::new(
                        config,
                        Arc::clone(&environment),
                        client.clone(),
                        Arc::clone(&metrics),
                    )?)),
                    ProviderKind::CodexCli => ProviderRuntime::Codex(Arc::new(CodexProvider::new(
                        config,
                        codex_environment.clone(),
                        Arc::clone(&metrics),
                    ))),
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
    pub(super) async fn readiness(&self) -> BTreeMap<String, ProviderAdmissionState> {
        let probes = self.providers.iter().map(|(name, runtime)| {
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
}

impl HttpProvider {
    fn new(
        config: &ProviderConfig,
        environment: Arc<dyn Environment + Send + Sync>,
        client: Client,
        metrics: Arc<FabricMetrics>,
    ) -> Result<Self, ProviderRegistryBuildError> {
        let endpoint = config
            .endpoint
            .as_ref()
            .ok_or_else(|| invalid_provider(config))?;
        let protocol = config.protocol.ok_or_else(|| invalid_provider(config))?;
        let path = match protocol {
            ProviderProtocol::OpenAi => CHAT_COMPLETIONS_PATH,
            ProviderProtocol::Anthropic => ANTHROPIC_MESSAGES_PATH,
        };
        let request_url = endpoint_url(endpoint, path).ok_or_else(|| invalid_provider(config))?;
        let models_url =
            endpoint_url(endpoint, MODELS_PATH).ok_or_else(|| invalid_provider(config))?;
        let credential = match (&config.api_key_env, &config.api_key_command) {
            (Some(name), None) => ProviderCredentialSource::Environment {
                name: name.clone(),
                environment,
            },
            (None, Some(command)) => ProviderCredentialSource::Command {
                command: command.clone(),
                environment: ChildEnvironment::current(),
            },
            _ => return Err(invalid_provider(config)),
        };
        Ok(Self {
            config: config.clone(),
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
        let adapter = match self
            .config
            .protocol
            .expect("validated HTTP providers have a protocol")
        {
            ProviderProtocol::OpenAi => ProviderHttpAdapter::OpenAi,
            ProviderProtocol::Anthropic => ProviderHttpAdapter::Anthropic {
                stream: request
                    .body_value_for_model(&self.config.model)?
                    .get("stream")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
        };
        let body = match adapter {
            ProviderHttpAdapter::OpenAi => build_open_ai_body(&self.config, request)?,
            ProviderHttpAdapter::Anthropic { .. } => {
                match anthropic::build_request(&self.config, request) {
                    Ok(request) => request.body,
                    Err(error) if error.is_incompatible() => {
                        return Ok(ProviderAdmissionOutcome::Rejected(
                            ProviderAdmissionState::Incompatible,
                        ));
                    }
                    Err(_) => return Err(ProviderRequestError::Anthropic),
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
                timeout: Duration::from_millis(self.config.timeout_ms),
                _permit: permit,
            },
        )))
    }

    /// Discard cached readiness after a real dispatch proved it wrong.
    ///
    /// Without this a provider that dies right after a successful probe keeps
    /// reporting `ready` for the whole TTL, which can be an hour.
    async fn invalidate_readiness(&self, status: Option<StatusCode>) {
        if matches!(
            status,
            Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        ) {
            self.credential.invalidate().await;
        }
        *self.readiness.lock().await = CachedReadiness::default();
    }

    async fn readiness(&self) -> ProviderAdmissionState {
        if self.permits.available_permits() == 0 {
            return ProviderAdmissionState::Busy;
        }
        let mut cached = self.readiness.lock().await;
        let ttl = Duration::from_millis(self.config.readiness_ttl_ms);
        if cached
            .checked_at
            .is_some_and(|checked| checked.elapsed() < ttl)
        {
            return cached.state;
        }
        let state = match self.credential.resolve().await {
            Ok(api_key) => {
                let protocol = self
                    .config
                    .protocol
                    .expect("validated HTTP providers have a protocol");
                let request =
                    authorize_http(self.client.get(self.models_url.clone()), &api_key, protocol);
                match tokio::time::timeout(
                    Duration::from_millis(self.config.readiness_timeout_ms),
                    request.send(),
                )
                .await
                {
                    Ok(Ok(response)) if response.status().is_success() => {
                        ProviderAdmissionState::Ready
                    }
                    // A credential the provider rejects is an operator error,
                    // not an outage, and must not be collapsed into it.
                    Ok(Ok(response)) if matches!(response.status().as_u16(), 401 | 403) => {
                        self.credential.invalidate().await;
                        ProviderAdmissionState::Unauthenticated
                    }
                    // `/models` is not uniformly implemented. These statuses
                    // prove the endpoint answered, which is what readiness asks.
                    Ok(Ok(response)) if matches!(response.status().as_u16(), 405 | 429) => {
                        ProviderAdmissionState::Ready
                    }
                    _ => ProviderAdmissionState::Unavailable,
                }
            }
            Err(_) => ProviderAdmissionState::Unauthenticated,
        };
        *cached = CachedReadiness {
            checked_at: Some(Instant::now()),
            state,
        };
        self.metrics.record_probe(&self.config.name, state);
        state
    }
}

impl CodexProvider {
    fn new(
        config: &ProviderConfig,
        environment: ChildEnvironment,
        metrics: Arc<FabricMetrics>,
    ) -> Self {
        Self {
            config: config.clone(),
            executable: PathBuf::from(
                config
                    .executable
                    .as_deref()
                    .expect("validated codex_cli providers have an executable"),
            ),
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
            request,
            route_reasoning_effort,
            self.environment.clone(),
        ) {
            Ok(request) => request,
            Err(error) if error.is_incompatible() => {
                return Ok(ProviderAdmissionOutcome::Rejected(
                    ProviderAdmissionState::Incompatible,
                ));
            }
            Err(_) => return Err(ProviderRequestError::Codex),
        };
        Ok(ProviderAdmissionOutcome::Admitted(Box::new(
            ProviderLease {
                provider: self.config.name.clone(),
                model: self.config.model.clone(),
                dispatch: ProviderDispatch::Codex(request),
                timeout: Duration::from_millis(self.config.timeout_ms),
                _permit: permit,
            },
        )))
    }

    /// Discard cached readiness after a real dispatch proved it wrong.
    async fn invalidate_readiness(&self, _status: Option<StatusCode>) {
        *self.readiness.lock().await = CachedReadiness::default();
    }

    async fn readiness(&self) -> ProviderAdmissionState {
        if self.permits.available_permits() == 0 {
            return ProviderAdmissionState::Busy;
        }
        let mut cached = self.readiness.lock().await;
        let ttl = Duration::from_millis(self.config.readiness_ttl_ms);
        if cached
            .checked_at
            .is_some_and(|checked| checked.elapsed() < ttl)
        {
            return cached.state;
        }
        let state = if codex::probe(
            &self.executable,
            &self.environment,
            Duration::from_millis(self.config.readiness_timeout_ms),
        )
        .await
        .is_ok()
        {
            ProviderAdmissionState::Ready
        } else {
            ProviderAdmissionState::Unavailable
        };
        *cached = CachedReadiness {
            checked_at: Some(Instant::now()),
            state,
        };
        self.metrics.record_probe(&self.config.name, state);
        state
    }
}

fn authorize_http(
    request: RequestBuilder,
    api_key: &SecretString,
    protocol: ProviderProtocol,
) -> RequestBuilder {
    match protocol {
        ProviderProtocol::OpenAi => request.bearer_auth(api_key.expose_secret()),
        ProviderProtocol::Anthropic => request
            .header("x-api-key", api_key.expose_secret())
            .header("anthropic-version", ANTHROPIC_VERSION),
    }
}

impl ProviderCredentialSource {
    async fn resolve(&self) -> Result<SecretString, ProviderCredentialError> {
        match self {
            Self::Environment { name, environment } => environment
                .get(name)
                .ok_or(ProviderCredentialError::Missing)
                .and_then(validate_credential),
            Self::Command {
                command,
                environment,
            } => resolve_command_credential(command, environment).await,
        }
    }
}

fn build_open_ai_body(
    config: &ProviderConfig,
    request: &GatewayRequest,
) -> Result<Bytes, ProviderRequestError> {
    let mut body = request.body_value_for_model(&config.model)?;
    let object = body
        .as_object_mut()
        .expect("gateway request bodies are validated objects");

    // Only a provider explicitly configured for reasoning receives the field.
    // A route-wide default would send `reasoning_effort` to every OpenAI-protocol
    // model whether or not it accepts one, which mutates more than the model and
    // server-owned Auto policy.
    if let Some(effort) = config.reasoning_effort
        && !present(object, "reasoning_effort")
        && !present(object, "reasoning")
    {
        object.insert(
            "reasoning_effort".to_string(),
            Value::String(effort.as_str().to_string()),
        );
    }
    if let Some(temperature) = config.temperature
        && !present(object, "temperature")
    {
        let number = Number::from_f64(temperature).ok_or(ProviderRequestError::Serialization)?;
        object.insert("temperature".to_string(), Value::Number(number));
    }
    if let Some(max_tokens) = config.max_tokens
        && !present(object, "max_tokens")
        && !present(object, "max_completion_tokens")
    {
        object.insert(
            "max_tokens".to_string(),
            Value::Number(Number::from(max_tokens)),
        );
    }
    if config.profile == ProviderProfile::OpenRouterAuto {
        apply_openrouter_auto_profile(object)?;
    }

    serde_json::to_vec(&body)
        .map(Bytes::from)
        .map_err(|_| ProviderRequestError::Serialization)
}

fn apply_openrouter_auto_profile(
    body: &mut Map<String, Value>,
) -> Result<(), ProviderRequestError> {
    let plugins = body
        .entry("plugins")
        .or_insert_with(|| Value::Array(Vec::new()));
    if plugins.is_null() {
        *plugins = Value::Array(Vec::new());
    }
    let Value::Array(plugins) = plugins else {
        return Err(ProviderRequestError::InvalidOpenRouterPlugins);
    };

    let matching_index = {
        let mut matching = plugins.iter().enumerate().filter_map(|(index, plugin)| {
            plugin
                .as_object()
                .and_then(|plugin| plugin.get("id"))
                .and_then(Value::as_str)
                .filter(|id| *id == OPENROUTER_AUTO_PLUGIN)
                .map(|_| index)
        });
        let first = matching.next();
        if matching.next().is_some() {
            return Err(ProviderRequestError::InvalidOpenRouterPlugins);
        }
        first
    };

    let plugin = if let Some(index) = matching_index {
        plugins[index]
            .as_object_mut()
            .ok_or(ProviderRequestError::InvalidOpenRouterPlugins)?
    } else {
        plugins.push(Value::Object(Map::new()));
        plugins
            .last_mut()
            .and_then(Value::as_object_mut)
            .expect("the appended OpenRouter profile is an object")
    };
    plugin.insert(
        "id".to_string(),
        Value::String(OPENROUTER_AUTO_PLUGIN.to_string()),
    );
    plugin.insert(
        "cost_quality_tradeoff".to_string(),
        Value::Number(Number::from(OPENROUTER_COST_QUALITY_TRADEOFF)),
    );
    plugin.remove("allowed_models");
    Ok(())
}

fn present(body: &Map<String, Value>, field: &str) -> bool {
    body.get(field).is_some_and(|value| !value.is_null())
}

async fn resolve_command_credential(
    arguments: &[String],
    environment: &ChildEnvironment,
) -> Result<SecretString, ProviderCredentialError> {
    let (program, arguments) = arguments
        .split_first()
        .ok_or(ProviderCredentialError::CommandFailed)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // `op`, `security`, `pass`, `gcloud`, and `aws` all read HOME, and several
    // need TMPDIR and locale. Restoring PATH alone makes every one of them fail
    // as an indistinguishable provider outage.
    environment.apply(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| ProviderCredentialError::CommandFailed)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(ProviderCredentialError::CommandFailed)?;
    let mut output = Vec::with_capacity(MAX_CREDENTIAL_BYTES + 1);
    let read = async {
        stdout
            .take((MAX_CREDENTIAL_BYTES + 1) as u64)
            .read_to_end(&mut output)
            .await
    };
    match tokio::time::timeout(CREDENTIAL_COMMAND_TIMEOUT, read).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => {
            terminate(&mut child).await;
            return Err(ProviderCredentialError::CommandFailed);
        }
        Err(_) => {
            terminate(&mut child).await;
            return Err(ProviderCredentialError::CommandTimeout);
        }
    }
    if output.len() > MAX_CREDENTIAL_BYTES {
        terminate(&mut child).await;
        return Err(ProviderCredentialError::CommandOutputTooLarge);
    }
    let status = match tokio::time::timeout(CREDENTIAL_COMMAND_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => return Err(ProviderCredentialError::CommandFailed),
        Err(_) => {
            terminate(&mut child).await;
            return Err(ProviderCredentialError::CommandTimeout);
        }
    };
    if !status.success() {
        return Err(ProviderCredentialError::CommandFailed);
    }
    let output = String::from_utf8(output).map_err(|_| ProviderCredentialError::Invalid)?;
    validate_credential(SecretString::from(
        output.trim_end_matches(['\r', '\n']).to_string(),
    ))
}

async fn terminate(child: &mut Child) {
    let _ = child.kill().await;
}

fn validate_credential(value: SecretString) -> Result<SecretString, ProviderCredentialError> {
    let candidate = value.expose_secret();
    if candidate.is_empty()
        || candidate.len() > MAX_CREDENTIAL_BYTES
        || !candidate.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(ProviderCredentialError::Invalid);
    }
    Ok(value)
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

#[derive(Debug, Error)]
enum ProviderCredentialError {
    #[error("credential is missing")]
    Missing,
    #[error("credential has an invalid shape")]
    Invalid,
    #[error("credential command failed")]
    CommandFailed,
    #[error("credential command timed out")]
    CommandTimeout,
    #[error("credential command output exceeded its bound")]
    CommandOutputTooLarge,
}

impl ProviderCredentialError {
    const fn code(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::CommandFailed => "command_failed",
            Self::CommandTimeout => "command_timeout",
            Self::CommandOutputTooLarge => "command_output_too_large",
        }
    }
}
