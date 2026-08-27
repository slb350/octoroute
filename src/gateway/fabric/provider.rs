//! Lazy, credential-isolated runtime registry for configured inference providers.

use super::{ProviderConfig, ProviderKind, ProviderProfile, ProviderProtocol, ReasoningEffort};
use crate::gateway::{
    env::Environment,
    http_client::endpoint_url,
    request::{GatewayRequest, GatewayRequestError},
};
use bytes::Bytes;
use reqwest::Url;
use secrecy::SecretString;
use serde_json::{Map, Number, Value};
use std::{collections::BTreeMap, process::Stdio, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{OwnedSemaphorePermit, Semaphore},
};

const CHAT_COMPLETIONS_PATH: &str = "chat/completions";
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
}

/// Result of attempting to reserve one configured provider.
pub enum ProviderAdmissionOutcome {
    Admitted(Box<ProviderLease>),
    Rejected(ProviderAdmissionState),
}

/// One provider dispatch with its isolated credential and concurrency permit.
pub struct ProviderLease {
    provider: String,
    model: String,
    chat_url: Url,
    api_key: SecretString,
    request_body: Bytes,
    timeout: Duration,
    openrouter_profile: bool,
    _permit: OwnedSemaphorePermit,
}

impl ProviderLease {
    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn request_body(&self) -> &Bytes {
        &self.request_body
    }

    pub(crate) fn into_transport_parts(
        self,
    ) -> (
        Url,
        SecretString,
        Bytes,
        Duration,
        bool,
        OwnedSemaphorePermit,
    ) {
        (
            self.chat_url,
            self.api_key,
            self.request_body,
            self.timeout,
            self.openrouter_profile,
            self._permit,
        )
    }
}

/// Runtime providers keyed only by validated configuration names.
pub struct ProviderRegistry {
    providers: BTreeMap<String, ProviderRuntime>,
}

enum ProviderRuntime {
    Disabled,
    Unsupported,
    OpenAi(Arc<OpenAiProvider>),
}

struct OpenAiProvider {
    config: ProviderConfig,
    chat_url: Url,
    credential: ProviderCredentialSource,
    permits: Arc<Semaphore>,
}

enum ProviderCredentialSource {
    Environment {
        name: String,
        environment: Arc<dyn Environment + Send + Sync>,
    },
    Command(Vec<String>),
}

impl ProviderRegistry {
    /// Build adapters without resolving credentials or launching credential commands.
    pub fn new(
        configs: &BTreeMap<String, ProviderConfig>,
        environment: Arc<dyn Environment + Send + Sync>,
    ) -> Result<Self, ProviderRegistryBuildError> {
        let mut providers = BTreeMap::new();
        for (name, config) in configs {
            let runtime = if !config.enabled {
                ProviderRuntime::Disabled
            } else if config.kind != ProviderKind::Http
                || config.protocol != Some(ProviderProtocol::OpenAi)
            {
                ProviderRuntime::Unsupported
            } else {
                let endpoint = config.endpoint.as_ref().ok_or_else(|| {
                    ProviderRegistryBuildError::InvalidProvider {
                        provider: name.clone(),
                    }
                })?;
                let chat_url = endpoint_url(endpoint, CHAT_COMPLETIONS_PATH).ok_or_else(|| {
                    ProviderRegistryBuildError::InvalidProvider {
                        provider: name.clone(),
                    }
                })?;
                let credential = match (&config.api_key_env, &config.api_key_command) {
                    (Some(name), None) => ProviderCredentialSource::Environment {
                        name: name.clone(),
                        environment: Arc::clone(&environment),
                    },
                    (None, Some(command)) => ProviderCredentialSource::Command(command.clone()),
                    _ => {
                        return Err(ProviderRegistryBuildError::InvalidProvider {
                            provider: name.clone(),
                        });
                    }
                };
                ProviderRuntime::OpenAi(Arc::new(OpenAiProvider {
                    config: config.clone(),
                    chat_url,
                    credential,
                    permits: Arc::new(Semaphore::new(config.max_in_flight)),
                }))
            };
            providers.insert(name.clone(), runtime);
        }
        Ok(Self { providers })
    }

    /// Reserve and prepare an OpenAI-compatible provider request.
    pub async fn try_admit(
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
            ProviderRuntime::Disabled => Ok(ProviderAdmissionOutcome::Rejected(
                ProviderAdmissionState::Disabled,
            )),
            ProviderRuntime::Unsupported => Ok(ProviderAdmissionOutcome::Rejected(
                ProviderAdmissionState::Incompatible,
            )),
            ProviderRuntime::OpenAi(provider) => {
                provider.try_admit(request, route_reasoning_effort).await
            }
        }
    }

    /// Return non-probing runtime readiness for every configured provider.
    pub fn readiness(&self) -> BTreeMap<String, ProviderAdmissionState> {
        self.providers
            .iter()
            .map(|(name, runtime)| {
                let state = match runtime {
                    ProviderRuntime::Disabled => ProviderAdmissionState::Disabled,
                    ProviderRuntime::Unsupported => ProviderAdmissionState::Incompatible,
                    ProviderRuntime::OpenAi(provider)
                        if provider.permits.available_permits() == 0 =>
                    {
                        ProviderAdmissionState::Busy
                    }
                    ProviderRuntime::OpenAi(_) => ProviderAdmissionState::Ready,
                };
                (name.clone(), state)
            })
            .collect()
    }
}

impl OpenAiProvider {
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
        let request_body = build_open_ai_body(&self.config, request, route_reasoning_effort)?;
        let api_key = match self.credential.resolve().await {
            Ok(api_key) => api_key,
            Err(error) => {
                tracing::warn!(
                    provider = self.config.name.as_str(),
                    reason = error.code(),
                    "provider credential could not be resolved"
                );
                return Ok(ProviderAdmissionOutcome::Rejected(
                    ProviderAdmissionState::Unavailable,
                ));
            }
        };
        Ok(ProviderAdmissionOutcome::Admitted(Box::new(
            ProviderLease {
                provider: self.config.name.clone(),
                model: self.config.model.clone(),
                chat_url: self.chat_url.clone(),
                api_key,
                request_body,
                timeout: Duration::from_millis(self.config.timeout_ms),
                openrouter_profile: self.config.profile == ProviderProfile::OpenRouterAuto,
                _permit: permit,
            },
        )))
    }
}

impl ProviderCredentialSource {
    async fn resolve(&self) -> Result<SecretString, ProviderCredentialError> {
        match self {
            Self::Environment { name, environment } => environment
                .get(name)
                .ok_or(ProviderCredentialError::Missing)
                .and_then(validate_credential),
            Self::Command(command) => resolve_command_credential(command).await,
        }
    }
}

fn build_open_ai_body(
    config: &ProviderConfig,
    request: &GatewayRequest,
    route_reasoning_effort: ReasoningEffort,
) -> Result<Bytes, ProviderRequestError> {
    let mut body = request.body_value_for_model(&config.model)?;
    let object = body
        .as_object_mut()
        .expect("gateway request bodies are validated objects");

    if !present(object, "reasoning_effort") && !present(object, "reasoning") {
        let effort = config
            .reasoning_effort
            .unwrap_or(route_reasoning_effort)
            .as_str();
        object.insert(
            "reasoning_effort".to_string(),
            Value::String(effort.to_string()),
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

impl ReasoningEffort {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
        }
    }
}

async fn resolve_command_credential(
    arguments: &[String],
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
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
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
    validate_credential(output.trim_end_matches(['\r', '\n']).to_string())
}

async fn terminate(child: &mut Child) {
    let _ = child.kill().await;
}

fn validate_credential(value: String) -> Result<SecretString, ProviderCredentialError> {
    if value.is_empty()
        || value.len() > MAX_CREDENTIAL_BYTES
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(ProviderCredentialError::Invalid);
    }
    Ok(SecretString::from(value))
}

/// Provider registry construction failures that never include credentials.
#[derive(Debug, Error)]
pub enum ProviderRegistryBuildError {
    #[error("configured provider `{provider}` cannot be constructed")]
    InvalidProvider { provider: String },
}

/// Safe provider request construction failures.
#[derive(Debug, Error)]
pub enum ProviderRequestError {
    #[error("invalid OpenRouter Auto plugins shape")]
    InvalidOpenRouterPlugins,
    #[error("could not serialize the provider request")]
    Serialization,
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
