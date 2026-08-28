//! Validated static configuration for the v3 inference fabric.

use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    str::FromStr,
};
use thiserror::Error;

/// The only configuration version accepted by this parser.
pub const FABRIC_CONFIG_VERSION: i64 = 3;

const DEFAULT_SERVER_MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_SERVER_MAX_HEADER_BYTES: usize = 32 * 1024;
const DEFAULT_SERVER_MAX_IN_FLIGHT: usize = 64;
const DEFAULT_SERVER_REQUESTS_PER_MINUTE: u32 = 120;
const DEFAULT_MEMBER_MAX_IN_FLIGHT: usize = 1;
const DEFAULT_PROVIDER_MAX_IN_FLIGHT: usize = 1;
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 1024 * 1024;
const MAX_CONCURRENCY: usize = 10_000;
const MAX_REQUESTS_PER_MINUTE: u32 = 1_000_000;
const DEFAULT_CONTEXT_SAFETY_TOKENS: u32 = 2_048;
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 16_384;
const DEFAULT_UPSTREAM_TIMEOUT_MS: u64 = 1_800_000;
const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 1_800_000;
const DEFAULT_PROVIDER_READINESS_TTL_MS: u64 = 30_000;
const DEFAULT_PROVIDER_READINESS_TIMEOUT_MS: u64 = 30_000;
const MAX_PROVIDER_READINESS_TTL_MS: u64 = 3_600_000;
const MAX_PROVIDER_READINESS_TIMEOUT_MS: u64 = 300_000;
const MAX_UPSTREAM_TIMEOUT_MS: u64 = 3_600_000;
const MAX_COMMAND_ARGUMENTS: usize = 32;
const MAX_COMMAND_ARGUMENT_BYTES: usize = 4 * 1024;
const MAX_COMMAND_BYTES: usize = 16 * 1024;
const MAX_MODEL_BYTES: usize = 512;
const DEFAULT_PRIORITY: u16 = 100;
const DEFAULT_CODEX_EXECUTABLE: &str = "codex";

/// Capabilities which may be admitted to a local inference pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalCapability {
    /// OpenAI chat-completion messages.
    Chat,
    /// Incremental SSE responses.
    Stream,
    /// OpenAI tool definitions and calls.
    Tools,
    /// JSON object or JSON schema output.
    StructuredOutput,
    /// Image content blocks.
    ImageInput,
    /// Audio content blocks.
    AudioInput,
    /// Video content blocks.
    VideoInput,
    /// Reasoning controls or reasoning content.
    Reasoning,
}

/// Fully validated v3 inference-fabric configuration.
#[derive(Debug, Clone)]
pub struct FabricConfig {
    /// Listener and inbound-authentication settings.
    pub server: FabricServerConfig,
    /// Local inference pools keyed by stable name.
    pub local_pools: BTreeMap<String, LocalPoolConfig>,
    /// Cloud and subscription-backed providers keyed by stable name.
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Virtual-model routes keyed by the OpenAI `model` value.
    pub routes: BTreeMap<String, VirtualRoute>,
    /// Route used when a caller requests `model: auto`.
    pub default_model: String,
    /// Logging settings retained for the v3 runtime.
    pub observability: FabricObservabilityConfig,
}

impl FabricConfig {
    /// Parse and validate a complete v3 configuration document.
    pub fn from_toml(input: &str) -> Result<Self, FabricConfigError> {
        let raw: RawFabricConfig =
            toml::from_str(input).map_err(|error| safe_parse_error(input, error))?;
        raw.validate()
    }
}

/// Validated inbound server settings shared by every route.
#[derive(Debug, Clone)]
pub struct FabricServerConfig {
    pub host: IpAddr,
    pub port: u16,
    pub api_key_env: String,
    pub max_request_bytes: usize,
    pub max_header_bytes: usize,
    pub max_in_flight: usize,
    pub requests_per_minute: u32,
}

/// Logging settings for the v3 runtime.
#[derive(Debug, Clone)]
pub struct FabricObservabilityConfig {
    pub log_level: String,
}

/// A pool of equivalent local inference servers.
#[derive(Debug, Clone)]
pub struct LocalPoolConfig {
    pub name: String,
    pub enabled: bool,
    pub model: String,
    pub model_revision: String,
    pub context_window: u32,
    pub context_safety_tokens: u32,
    pub default_max_output_tokens: u32,
    pub timeout_ms: u64,
    pub capabilities: BTreeSet<LocalCapability>,
    pub strategy: PoolStrategy,
    pub default_reasoning_effort: ReasoningEffort,
    pub members: Vec<LocalMemberConfig>,
}

/// One independently admitted local inference endpoint.
#[derive(Debug, Clone)]
pub struct LocalMemberConfig {
    pub name: String,
    pub enabled: bool,
    pub base_url: Url,
    pub api_key_env: Option<String>,
    pub max_in_flight: usize,
    pub priority: u16,
}

/// Selection strategy for a local pool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolStrategy {
    /// Prefer the member with the fewest active requests, rotating ties.
    #[default]
    LeastLoaded,
}

/// A cloud API or locally installed subscription-backed executable.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub name: String,
    pub enabled: bool,
    pub kind: ProviderKind,
    pub endpoint: Option<Url>,
    pub protocol: Option<ProviderProtocol>,
    pub model: String,
    pub api_key_env: Option<String>,
    pub api_key_command: Option<Vec<String>>,
    pub max_in_flight: usize,
    pub timeout_ms: u64,
    pub readiness_ttl_ms: u64,
    pub readiness_timeout_ms: u64,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub profile: ProviderProfile,
    pub executable: Option<String>,
}

/// Concrete execution path for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// An HTTP API whose wire protocol is configured separately.
    Http,
    /// The installed Codex CLI using ChatGPT-managed authentication.
    CodexCli,
}

/// HTTP wire protocol used by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    OpenAi,
    Anthropic,
}

/// Request-shaping profile applied by the eventual provider adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProfile {
    /// Preserve caller fields except for the destination model.
    #[default]
    Passthrough,
    /// Apply Octoroute-owned OpenRouter Auto policy fields.
    OpenRouterAuto,
}

/// Supported reasoning settings exposed by configured backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    Xhigh,
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

/// Privacy contract attached to a virtual-model route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutePrivacy {
    /// Every target must remain on the local network.
    LocalOnly,
    /// Local targets are attempted first and cloud targets are allowed later.
    CloudAllowed,
    /// Every target must be a cloud or subscription-backed provider.
    CloudOnly,
}

/// A validated virtual model and its ordered target chain.
#[derive(Debug, Clone)]
pub struct VirtualRoute {
    pub model: String,
    pub privacy: RoutePrivacy,
    pub steps: Vec<RouteTarget>,
    pub default_reasoning_effort: ReasoningEffort,
    pub fallback_on: BTreeSet<FallbackTrigger>,
}

/// One step in a virtual-model route chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteTarget {
    LocalPool(String),
    Provider(String),
}

impl RouteTarget {
    /// Stable configured target name without the `pool:` or `provider:` prefix.
    pub fn name(&self) -> &str {
        match self {
            Self::LocalPool(name) | Self::Provider(name) => name,
        }
    }
}

impl FromStr for RouteTarget {
    type Err = FabricConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (kind, name) = value.split_once(':').ok_or_else(|| {
            invalid(
                "routing.routes.steps",
                format!("`{value}` must use `pool:name` or `provider:name`"),
            )
        })?;
        validate_name("routing.routes.steps", name)?;
        match kind {
            "pool" => Ok(Self::LocalPool(name.to_string())),
            "provider" => Ok(Self::Provider(name.to_string())),
            _ => Err(invalid(
                "routing.routes.steps",
                format!("`{value}` must use `pool:name` or `provider:name`"),
            )),
        }
    }
}

/// Conditions under which a route may continue to its next target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackTrigger {
    Busy,
    Unhealthy,
    ContextOverflow,
    Incompatible,
    RateLimited,
    PrecommitFailure,
}

/// Safe static v3 configuration failures.
#[derive(Debug, Error)]
pub enum FabricConfigError {
    #[error("invalid v3 TOML at line {line}, column {column}; values omitted")]
    Parse { line: usize, column: usize },
    #[error("unsupported Octoroute config version {0}; expected 3")]
    UnsupportedVersion(i64),
    #[error("invalid `{field}`: {message}")]
    Invalid { field: String, message: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFabricConfig {
    config_version: i64,
    server: RawServerConfig,
    fabric: RawInferenceFabric,
    routing: RawRoutingConfig,
    #[serde(default)]
    observability: RawObservabilityConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServerConfig {
    host: String,
    port: u16,
    api_key_env: String,
    #[serde(default = "default_server_max_request_bytes")]
    max_request_bytes: usize,
    #[serde(default = "default_server_max_header_bytes")]
    max_header_bytes: usize,
    #[serde(default = "default_server_max_in_flight")]
    max_in_flight: usize,
    #[serde(default = "default_server_requests_per_minute")]
    requests_per_minute: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInferenceFabric {
    #[serde(default)]
    local_pools: Vec<RawLocalPoolConfig>,
    #[serde(default)]
    providers: Vec<RawProviderConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocalPoolConfig {
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
    model: String,
    model_revision: String,
    context_window: u32,
    #[serde(default = "default_context_safety_tokens")]
    context_safety_tokens: u32,
    #[serde(default = "default_max_output_tokens")]
    default_max_output_tokens: u32,
    #[serde(default = "default_upstream_timeout_ms")]
    timeout_ms: u64,
    capabilities: BTreeSet<LocalCapability>,
    #[serde(default)]
    strategy: PoolStrategy,
    #[serde(default = "default_reasoning_effort")]
    default_reasoning_effort: ReasoningEffort,
    #[serde(default)]
    members: Vec<RawLocalMemberConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocalMemberConfig {
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
    base_url: String,
    api_key_env: Option<String>,
    #[serde(default = "default_member_max_in_flight")]
    max_in_flight: usize,
    #[serde(default = "default_priority")]
    priority: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProviderConfig {
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
    kind: ProviderKind,
    endpoint: Option<String>,
    protocol: Option<ProviderProtocol>,
    model: String,
    api_key_env: Option<String>,
    api_key_command: Option<Vec<String>>,
    #[serde(default = "default_provider_max_in_flight")]
    max_in_flight: usize,
    #[serde(default = "default_provider_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_provider_readiness_ttl_ms")]
    readiness_ttl_ms: u64,
    #[serde(default = "default_provider_readiness_timeout_ms")]
    readiness_timeout_ms: u64,
    reasoning_effort: Option<ReasoningEffort>,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    #[serde(default)]
    profile: ProviderProfile,
    executable: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoutingConfig {
    default_model: String,
    #[serde(default)]
    routes: Vec<RawVirtualRoute>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVirtualRoute {
    model: String,
    privacy: RoutePrivacy,
    steps: Vec<String>,
    #[serde(default = "default_reasoning_effort")]
    default_reasoning_effort: ReasoningEffort,
    #[serde(default = "default_fallback_triggers")]
    fallback_on: BTreeSet<FallbackTrigger>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawObservabilityConfig {
    #[serde(default = "default_log_level")]
    log_level: String,
}

impl Default for RawObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
        }
    }
}

impl RawFabricConfig {
    fn validate(self) -> Result<FabricConfig, FabricConfigError> {
        if self.config_version != FABRIC_CONFIG_VERSION {
            return Err(FabricConfigError::UnsupportedVersion(self.config_version));
        }

        let server = self.server.validate()?;
        let local_pools = validate_local_pools(self.fabric.local_pools)?;
        let providers = validate_providers(self.fabric.providers)?;
        let routes = validate_routes(self.routing.routes, &local_pools, &providers)?;
        validate_name("routing.default_model", &self.routing.default_model)?;
        if !routes.contains_key(&self.routing.default_model) {
            return Err(invalid(
                "routing.default_model",
                "must reference a configured virtual route",
            ));
        }
        validate_log_level(&self.observability.log_level)?;

        Ok(FabricConfig {
            server,
            local_pools,
            providers,
            routes,
            default_model: self.routing.default_model,
            observability: FabricObservabilityConfig {
                log_level: self.observability.log_level,
            },
        })
    }
}

impl RawServerConfig {
    fn validate(self) -> Result<FabricServerConfig, FabricConfigError> {
        let host = self
            .host
            .parse::<IpAddr>()
            .map_err(|_| invalid("server.host", "must be an IP address"))?;
        if self.port == 0 {
            return Err(invalid("server.port", "must be greater than zero"));
        }
        validate_env_name("server.api_key_env", &self.api_key_env)?;
        validate_usize_range(
            "server.max_request_bytes",
            self.max_request_bytes,
            MAX_REQUEST_BYTES,
        )?;
        validate_usize_range(
            "server.max_header_bytes",
            self.max_header_bytes,
            MAX_HEADER_BYTES,
        )?;
        validate_usize_range("server.max_in_flight", self.max_in_flight, MAX_CONCURRENCY)?;
        validate_u32_range(
            "server.requests_per_minute",
            self.requests_per_minute,
            MAX_REQUESTS_PER_MINUTE,
        )?;
        Ok(FabricServerConfig {
            host,
            port: self.port,
            api_key_env: self.api_key_env,
            max_request_bytes: self.max_request_bytes,
            max_header_bytes: self.max_header_bytes,
            max_in_flight: self.max_in_flight,
            requests_per_minute: self.requests_per_minute,
        })
    }
}

fn validate_local_pools(
    raw_pools: Vec<RawLocalPoolConfig>,
) -> Result<BTreeMap<String, LocalPoolConfig>, FabricConfigError> {
    let mut pools = BTreeMap::new();
    for raw in raw_pools {
        validate_name("fabric.local_pools.name", &raw.name)?;
        validate_model("fabric.local_pools.model", &raw.model)?;
        validate_revision("fabric.local_pools.model_revision", &raw.model_revision)?;
        if raw.context_window == 0 {
            return Err(invalid(
                "fabric.local_pools.context_window",
                "must be greater than zero",
            ));
        }
        if raw.context_safety_tokens >= raw.context_window {
            return Err(invalid(
                "fabric.local_pools.context_safety_tokens",
                "must be smaller than context_window",
            ));
        }
        let usable_context = raw.context_window - raw.context_safety_tokens;
        if raw.default_max_output_tokens == 0 || raw.default_max_output_tokens >= usable_context {
            return Err(invalid(
                "fabric.local_pools.default_max_output_tokens",
                "must be positive and leave room for input",
            ));
        }
        validate_u64_range(
            "fabric.local_pools.timeout_ms",
            raw.timeout_ms,
            MAX_UPSTREAM_TIMEOUT_MS,
        )?;
        if !raw.capabilities.contains(&LocalCapability::Chat) {
            return Err(invalid(
                "fabric.local_pools.capabilities",
                "must include `chat`",
            ));
        }

        let mut member_names = BTreeSet::new();
        let mut members = Vec::with_capacity(raw.members.len());
        for member in raw.members {
            validate_name("fabric.local_pools.members.name", &member.name)?;
            if !member_names.insert(member.name.clone()) {
                return Err(invalid(
                    "fabric.local_pools.members.name",
                    format!("duplicate member `{}`", member.name),
                ));
            }
            validate_usize_range(
                "fabric.local_pools.members.max_in_flight",
                member.max_in_flight,
                MAX_CONCURRENCY,
            )?;
            if let Some(name) = member.api_key_env.as_deref() {
                validate_env_name("fabric.local_pools.members.api_key_env", name)?;
            }
            members.push(LocalMemberConfig {
                name: member.name,
                enabled: member.enabled,
                base_url: validate_url(
                    "fabric.local_pools.members.base_url",
                    &member.base_url,
                    false,
                )?,
                api_key_env: member.api_key_env,
                max_in_flight: member.max_in_flight,
                priority: member.priority,
            });
        }
        if members.is_empty() {
            return Err(invalid(
                "fabric.local_pools.members",
                "must include at least one member",
            ));
        }

        let name = raw.name.clone();
        let pool = LocalPoolConfig {
            name: raw.name,
            enabled: raw.enabled,
            model: raw.model,
            model_revision: raw.model_revision,
            context_window: raw.context_window,
            context_safety_tokens: raw.context_safety_tokens,
            default_max_output_tokens: raw.default_max_output_tokens,
            timeout_ms: raw.timeout_ms,
            capabilities: raw.capabilities,
            strategy: raw.strategy,
            default_reasoning_effort: raw.default_reasoning_effort,
            members,
        };
        if pools.insert(name.clone(), pool).is_some() {
            return Err(invalid(
                "fabric.local_pools.name",
                format!("duplicate pool `{name}`"),
            ));
        }
    }
    Ok(pools)
}

fn validate_providers(
    raw_providers: Vec<RawProviderConfig>,
) -> Result<BTreeMap<String, ProviderConfig>, FabricConfigError> {
    let mut providers = BTreeMap::new();
    for raw in raw_providers {
        validate_name("fabric.providers.name", &raw.name)?;
        validate_model("fabric.providers.model", &raw.model)?;
        validate_usize_range(
            "fabric.providers.max_in_flight",
            raw.max_in_flight,
            MAX_CONCURRENCY,
        )?;
        validate_u64_range(
            "fabric.providers.timeout_ms",
            raw.timeout_ms,
            MAX_UPSTREAM_TIMEOUT_MS,
        )?;
        validate_u64_range(
            "fabric.providers.readiness_ttl_ms",
            raw.readiness_ttl_ms,
            MAX_PROVIDER_READINESS_TTL_MS,
        )?;
        validate_u64_range(
            "fabric.providers.readiness_timeout_ms",
            raw.readiness_timeout_ms,
            MAX_PROVIDER_READINESS_TIMEOUT_MS,
        )?;
        if raw
            .temperature
            .is_some_and(|temperature| !temperature.is_finite())
        {
            return Err(invalid(
                "fabric.providers.temperature",
                "must be finite when configured",
            ));
        }
        if raw.max_tokens == Some(0) {
            return Err(invalid(
                "fabric.providers.max_tokens",
                "must be greater than zero when configured",
            ));
        }

        let (endpoint, protocol) = match raw.kind {
            ProviderKind::Http => {
                if raw.executable.is_some() {
                    return Err(invalid(
                        "fabric.providers.executable",
                        "is accepted only for codex_cli providers",
                    ));
                }
                let endpoint = raw.endpoint.as_deref().ok_or_else(|| {
                    invalid(
                        "fabric.providers.endpoint",
                        "is required for an HTTP provider",
                    )
                })?;
                let protocol = raw.protocol.ok_or_else(|| {
                    invalid(
                        "fabric.providers.protocol",
                        "is required for an HTTP provider",
                    )
                })?;
                let has_env = raw.api_key_env.is_some();
                let has_command = raw.api_key_command.is_some();
                if has_env == has_command {
                    return Err(invalid(
                        "fabric.providers",
                        "HTTP providers require exactly one of api_key_env or api_key_command",
                    ));
                }
                if let Some(name) = raw.api_key_env.as_deref() {
                    validate_env_name("fabric.providers.api_key_env", name)?;
                }
                if let Some(command) = raw.api_key_command.as_deref() {
                    validate_command("fabric.providers.api_key_command", command)?;
                }
                if raw.profile == ProviderProfile::OpenRouterAuto
                    && protocol != ProviderProtocol::OpenAi
                {
                    return Err(invalid(
                        "fabric.providers.profile",
                        "openrouter_auto requires the OpenAI protocol",
                    ));
                }
                if protocol == ProviderProtocol::Anthropic && raw.max_tokens.is_none() {
                    return Err(invalid(
                        "fabric.providers.max_tokens",
                        "is required for Anthropic-compatible providers",
                    ));
                }
                (
                    Some(validate_url("fabric.providers.endpoint", endpoint, true)?),
                    Some(protocol),
                )
            }
            ProviderKind::CodexCli => {
                if raw.endpoint.is_some()
                    || raw.protocol.is_some()
                    || raw.api_key_env.is_some()
                    || raw.api_key_command.is_some()
                    || raw.temperature.is_some()
                    || raw.max_tokens.is_some()
                    || raw.profile != ProviderProfile::Passthrough
                {
                    return Err(invalid(
                        "fabric.providers",
                        "codex_cli does not accept endpoint, protocol, credential, sampling, token, or profile fields",
                    ));
                }
                validate_executable(
                    "fabric.providers.executable",
                    raw.executable
                        .as_deref()
                        .unwrap_or(DEFAULT_CODEX_EXECUTABLE),
                )?;
                (None, None)
            }
        };

        let name = raw.name.clone();
        let provider = ProviderConfig {
            name: raw.name,
            enabled: raw.enabled,
            kind: raw.kind,
            endpoint,
            protocol,
            model: raw.model,
            api_key_env: raw.api_key_env,
            api_key_command: raw.api_key_command,
            max_in_flight: raw.max_in_flight,
            timeout_ms: raw.timeout_ms,
            readiness_ttl_ms: raw.readiness_ttl_ms,
            readiness_timeout_ms: raw.readiness_timeout_ms,
            reasoning_effort: raw.reasoning_effort,
            temperature: raw.temperature,
            max_tokens: raw.max_tokens,
            profile: raw.profile,
            executable: (raw.kind == ProviderKind::CodexCli).then(|| {
                raw.executable
                    .unwrap_or_else(|| DEFAULT_CODEX_EXECUTABLE.to_string())
            }),
        };
        if providers.insert(name.clone(), provider).is_some() {
            return Err(invalid(
                "fabric.providers.name",
                format!("duplicate provider `{name}`"),
            ));
        }
    }
    Ok(providers)
}

fn validate_routes(
    raw_routes: Vec<RawVirtualRoute>,
    pools: &BTreeMap<String, LocalPoolConfig>,
    providers: &BTreeMap<String, ProviderConfig>,
) -> Result<BTreeMap<String, VirtualRoute>, FabricConfigError> {
    let mut routes = BTreeMap::new();
    for raw in raw_routes {
        validate_name("routing.routes.model", &raw.model)?;
        if raw.model == "auto" {
            return Err(invalid(
                "routing.routes.model",
                "`auto` is reserved for the configured default-model alias",
            ));
        }
        if raw.steps.is_empty() {
            return Err(invalid(
                "routing.routes.steps",
                "must include at least one target",
            ));
        }
        let mut target_names = BTreeSet::new();
        for step in &raw.steps {
            if !target_names.insert(step) {
                return Err(invalid(
                    "routing.routes.steps",
                    format!("duplicate target `{step}`"),
                ));
            }
        }
        let steps = raw
            .steps
            .iter()
            .map(|step| step.parse::<RouteTarget>())
            .collect::<Result<Vec<_>, _>>()?;

        let mut saw_provider = false;
        for step in &steps {
            match step {
                RouteTarget::LocalPool(name) => {
                    if !pools.contains_key(name) {
                        return Err(invalid(
                            "routing.routes.steps",
                            format!("unknown local pool `{name}`"),
                        ));
                    }
                    if saw_provider {
                        return Err(invalid(
                            "routing.routes.steps",
                            "local pools must precede providers in a route",
                        ));
                    }
                    if raw.privacy == RoutePrivacy::CloudOnly {
                        return Err(invalid(
                            "routing.routes.privacy",
                            "cloud_only routes cannot reference local pools",
                        ));
                    }
                }
                RouteTarget::Provider(name) => {
                    saw_provider = true;
                    if !providers.contains_key(name) {
                        return Err(invalid(
                            "routing.routes.steps",
                            format!("unknown provider `{name}`"),
                        ));
                    }
                    if raw.privacy == RoutePrivacy::LocalOnly {
                        return Err(invalid(
                            "routing.routes.privacy",
                            "local_only routes cannot reference providers",
                        ));
                    }
                }
            }
        }

        let model = raw.model.clone();
        let route = VirtualRoute {
            model: raw.model,
            privacy: raw.privacy,
            steps,
            default_reasoning_effort: raw.default_reasoning_effort,
            fallback_on: raw.fallback_on,
        };
        if routes.insert(model.clone(), route).is_some() {
            return Err(invalid(
                "routing.routes.model",
                format!("duplicate virtual model `{model}`"),
            ));
        }
    }
    Ok(routes)
}

fn validate_url(field: &str, value: &str, https_only: bool) -> Result<Url, FabricConfigError> {
    let mut url = Url::parse(value).map_err(|_| invalid(field, "must be an absolute URL"))?;
    let valid_scheme = if https_only {
        url.scheme() == "https"
    } else {
        matches!(url.scheme(), "http" | "https")
    };
    if !valid_scheme || url.host_str().is_none() {
        return Err(invalid(
            field,
            if https_only {
                "must be an absolute HTTPS URL"
            } else {
                "must be an absolute HTTP or HTTPS URL"
            },
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            field,
            "must not include credentials, query, or fragment",
        ));
    }
    if !url.path().ends_with('/') {
        let normalized = format!("{}/", url.path());
        url.set_path(&normalized);
    }
    Ok(url)
}

fn validate_name(field: &str, value: &str) -> Result<(), FabricConfigError> {
    validate_nonempty(field, value)?;
    if value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid(
            field,
            "must use at most 128 ASCII letters, digits, dots, underscores, or hyphens",
        ));
    }
    Ok(())
}

fn validate_revision(field: &str, value: &str) -> Result<(), FabricConfigError> {
    validate_nonempty(field, value)?;
    if value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
    {
        return Err(invalid(
            field,
            "must use at most 128 visible ASCII bytes without whitespace",
        ));
    }
    Ok(())
}

fn validate_nonempty(field: &str, value: &str) -> Result<(), FabricConfigError> {
    if value.trim().is_empty() {
        Err(invalid(field, "must not be empty"))
    } else {
        Ok(())
    }
}

fn validate_model(field: &str, value: &str) -> Result<(), FabricConfigError> {
    validate_nonempty(field, value)?;
    if value.len() > MAX_MODEL_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
    {
        return Err(invalid(
            field,
            format!("must use at most {MAX_MODEL_BYTES} visible ASCII bytes without whitespace"),
        ));
    }
    Ok(())
}

fn validate_env_name(field: &str, value: &str) -> Result<(), FabricConfigError> {
    validate_nonempty(field, value)?;
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(invalid(field, "must not be empty"));
    };
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(invalid(field, "must be a valid environment variable name"));
    }
    Ok(())
}

fn validate_command(field: &str, command: &[String]) -> Result<(), FabricConfigError> {
    let total_bytes = command.iter().fold(0usize, |total, argument| {
        total.saturating_add(argument.len())
    });
    if command.is_empty()
        || command.len() > MAX_COMMAND_ARGUMENTS
        || total_bytes > MAX_COMMAND_BYTES
        || command.iter().any(|argument| {
            argument.trim().is_empty()
                || argument.len() > MAX_COMMAND_ARGUMENT_BYTES
                || argument.chars().any(char::is_control)
        })
    {
        return Err(invalid(
            field,
            format!(
                "must contain 1..={MAX_COMMAND_ARGUMENTS} non-empty arguments, each at most {MAX_COMMAND_ARGUMENT_BYTES} bytes without control characters, and at most {MAX_COMMAND_BYTES} bytes total"
            ),
        ));
    }
    Ok(())
}

fn validate_executable(field: &str, executable: &str) -> Result<(), FabricConfigError> {
    if executable.trim().is_empty()
        || executable.len() > 4096
        || executable.chars().any(char::is_control)
    {
        return Err(invalid(
            field,
            "must be a non-empty path of at most 4096 bytes without control characters",
        ));
    }
    Ok(())
}

fn validate_log_level(value: &str) -> Result<(), FabricConfigError> {
    if matches!(value, "trace" | "debug" | "info" | "warn" | "error") {
        Ok(())
    } else {
        Err(invalid(
            "observability.log_level",
            "must be trace, debug, info, warn, or error",
        ))
    }
}

fn safe_parse_error(input: &str, error: toml::de::Error) -> FabricConfigError {
    let (line, column) = error
        .span()
        .map(|span| line_column(input, span.start))
        .unwrap_or((1, 1));
    FabricConfigError::Parse { line, column }
}

fn line_column(input: &str, byte_index: usize) -> (usize, usize) {
    let prefix = &input[..byte_index.min(input.len())];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.len() + 1, |(_, tail)| tail.len() + 1);
    (line, column)
}

fn validate_usize_range(
    field: &str,
    value: usize,
    maximum: usize,
) -> Result<(), FabricConfigError> {
    if (1..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(invalid(field, format!("must be between 1 and {maximum}")))
    }
}

fn validate_u32_range(field: &str, value: u32, maximum: u32) -> Result<(), FabricConfigError> {
    if (1..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(invalid(field, format!("must be between 1 and {maximum}")))
    }
}

fn validate_u64_range(field: &str, value: u64, maximum: u64) -> Result<(), FabricConfigError> {
    if (1..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(invalid(field, format!("must be between 1 and {maximum}")))
    }
}

fn invalid(field: impl Into<String>, message: impl Into<String>) -> FabricConfigError {
    FabricConfigError::Invalid {
        field: field.into(),
        message: message.into(),
    }
}

const fn default_server_max_request_bytes() -> usize {
    DEFAULT_SERVER_MAX_REQUEST_BYTES
}

const fn default_server_max_header_bytes() -> usize {
    DEFAULT_SERVER_MAX_HEADER_BYTES
}

const fn default_server_max_in_flight() -> usize {
    DEFAULT_SERVER_MAX_IN_FLIGHT
}

const fn default_server_requests_per_minute() -> u32 {
    DEFAULT_SERVER_REQUESTS_PER_MINUTE
}

const fn default_member_max_in_flight() -> usize {
    DEFAULT_MEMBER_MAX_IN_FLIGHT
}

const fn default_provider_max_in_flight() -> usize {
    DEFAULT_PROVIDER_MAX_IN_FLIGHT
}

const fn default_context_safety_tokens() -> u32 {
    DEFAULT_CONTEXT_SAFETY_TOKENS
}

const fn default_max_output_tokens() -> u32 {
    DEFAULT_MAX_OUTPUT_TOKENS
}

const fn default_upstream_timeout_ms() -> u64 {
    DEFAULT_UPSTREAM_TIMEOUT_MS
}

const fn default_provider_timeout_ms() -> u64 {
    DEFAULT_PROVIDER_TIMEOUT_MS
}

const fn default_provider_readiness_ttl_ms() -> u64 {
    DEFAULT_PROVIDER_READINESS_TTL_MS
}

const fn default_provider_readiness_timeout_ms() -> u64 {
    DEFAULT_PROVIDER_READINESS_TIMEOUT_MS
}

const fn default_priority() -> u16 {
    DEFAULT_PRIORITY
}

const fn default_true() -> bool {
    true
}

const fn default_reasoning_effort() -> ReasoningEffort {
    ReasoningEffort::Medium
}

fn default_fallback_triggers() -> BTreeSet<FallbackTrigger> {
    BTreeSet::from([
        FallbackTrigger::Busy,
        FallbackTrigger::Unhealthy,
        FallbackTrigger::ContextOverflow,
        FallbackTrigger::Incompatible,
        FallbackTrigger::RateLimited,
        FallbackTrigger::PrecommitFailure,
    ])
}

fn default_log_level() -> String {
    "info".to_string()
}
