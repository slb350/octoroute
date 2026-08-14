//! Validated v2 gateway configuration.

use reqwest::Url;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, net::IpAddr};
use thiserror::Error;

mod validation;

const CONFIG_VERSION: u8 = 2;
pub(crate) const MAX_SEMANTIC_BOUNDARY_STEPS: u8 = 2;

pub(crate) fn is_probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

/// Source used to resolve secret-bearing environment variables.
pub trait Environment {
    /// Return an environment variable without logging its value.
    fn get(&self, name: &str) -> Option<String>;
}

/// Environment source backed by the current process.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// Errors raised before the gateway binds a listener.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GatewayConfigError {
    /// The configuration uses the v1 tier schema.
    #[error(
        "Octoroute v1 configuration is not accepted by the v2 gateway; \
         add `config_version = 2` and follow the v2 migration guide"
    )]
    MigrationRequired,

    /// The declared schema version is not supported.
    #[error("unsupported Octoroute configuration version {version}; expected 2")]
    UnsupportedVersion {
        /// Version found in the document.
        version: i64,
    },

    /// TOML could not be deserialized. Source excerpts are intentionally omitted
    /// because they may contain secrets entered in an invalid field.
    #[error("invalid TOML at line {line}, column {column}: {message}")]
    Parse {
        /// Parser message without a source excerpt.
        message: String,
        /// One-based line.
        line: usize,
        /// One-based column.
        column: usize,
    },

    /// A required secret was not present in the environment.
    #[error("environment variable `{name}` required by `{field}` is missing or empty")]
    MissingEnvironmentVariable {
        /// Configuration field containing the variable name.
        field: String,
        /// Missing variable name.
        name: String,
    },

    /// A parsed value violated a gateway invariant.
    #[error("invalid `{field}`: {message}")]
    Invalid {
        /// Invalid field path.
        field: String,
        /// Safe operator-facing explanation.
        message: String,
    },
}

/// Fully validated gateway configuration, including redacted secrets.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    server: ServerConfig,
    local: LocalUpstreamConfig,
    openrouter: OpenRouterConfig,
    routing: RoutingConfig,
    observability: ObservabilityConfig,
}

impl GatewayConfig {
    /// Parse and validate v2 TOML using the supplied environment.
    pub fn from_toml(
        input: &str,
        environment: &impl Environment,
    ) -> Result<Self, GatewayConfigError> {
        validation::parse(input, environment)
    }

    /// Configuration schema version.
    pub fn version(&self) -> u8 {
        CONFIG_VERSION
    }

    /// Validated server configuration.
    pub fn server(&self) -> &ServerConfig {
        &self.server
    }

    /// Validated local llama.cpp configuration.
    pub fn local(&self) -> &LocalUpstreamConfig {
        &self.local
    }

    /// Validated OpenRouter configuration.
    pub fn openrouter(&self) -> &OpenRouterConfig {
        &self.openrouter
    }

    /// Validated route policy configuration.
    pub fn routing(&self) -> &RoutingConfig {
        &self.routing
    }

    /// Validated observability configuration.
    pub fn observability(&self) -> &ObservabilityConfig {
        &self.observability
    }
}

/// HTTP listener and inbound security settings.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    host: IpAddr,
    port: u16,
    api_key_env: String,
    api_key: SecretString,
    max_request_bytes: usize,
    max_header_bytes: usize,
    max_in_flight: usize,
    requests_per_minute: u32,
}

impl ServerConfig {
    /// Listener address.
    pub fn host(&self) -> IpAddr {
        self.host
    }

    /// Listener port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Environment variable holding the inbound bearer key.
    pub fn api_key_env(&self) -> &str {
        &self.api_key_env
    }

    /// Redacted inbound bearer key.
    pub fn api_key(&self) -> &SecretString {
        &self.api_key
    }

    /// Maximum accepted request body size.
    pub fn max_request_bytes(&self) -> usize {
        self.max_request_bytes
    }

    /// Maximum aggregate inbound HTTP header bytes.
    pub fn max_header_bytes(&self) -> usize {
        self.max_header_bytes
    }

    /// Maximum concurrent authenticated requests for the configured credential.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }

    /// Fixed-window authenticated request allowance.
    pub fn requests_per_minute(&self) -> u32 {
        self.requests_per_minute
    }
}

/// Capabilities which may be admitted to the local model.
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

impl LocalCapability {
    pub(crate) const ALL: [Self; 8] = [
        Self::Chat,
        Self::Stream,
        Self::Tools,
        Self::StructuredOutput,
        Self::ImageInput,
        Self::AudioInput,
        Self::VideoInput,
        Self::Reasoning,
    ];
}

/// llama.cpp upstream and admission settings.
#[derive(Debug, Clone)]
pub struct LocalUpstreamConfig {
    name: String,
    base_url: Url,
    model: String,
    context_window: u32,
    context_safety_tokens: u32,
    default_max_output_tokens: u32,
    max_in_flight: usize,
    health_cache_ttl_ms: u64,
    probe_timeout_ms: u64,
    first_byte_timeout_ms: Option<u64>,
    capabilities: BTreeSet<LocalCapability>,
    health_path: String,
    slots_path: String,
    input_tokens_path: String,
    api_key_env: Option<String>,
    api_key: Option<SecretString>,
}

impl LocalUpstreamConfig {
    /// Operator-facing upstream name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// llama.cpp API origin.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Exact llama.cpp model alias.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Configured context window.
    pub fn context_window(&self) -> u32 {
        self.context_window
    }

    /// Reserved context headroom.
    pub fn context_safety_tokens(&self) -> u32 {
        self.context_safety_tokens
    }

    /// Output tokens reserved when the request supplies no explicit limit.
    pub fn default_max_output_tokens(&self) -> u32 {
        self.default_max_output_tokens
    }

    /// Maximum requests Octoroute admits concurrently.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }

    /// Duration a successful or failed health probe remains authoritative.
    pub fn health_cache_ttl_ms(&self) -> u64 {
        self.health_cache_ttl_ms
    }

    /// Per-request deadline for llama.cpp admission probes.
    pub fn probe_timeout_ms(&self) -> u64 {
        self.probe_timeout_ms
    }

    /// Optional deadline from local dispatch through the first body byte.
    pub fn first_byte_timeout_ms(&self) -> Option<u64> {
        self.first_byte_timeout_ms
    }

    /// Whether this local model may receive a capability.
    pub fn supports(&self, capability: LocalCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// Relative liveness path.
    pub fn health_path(&self) -> &str {
        &self.health_path
    }

    /// Relative slot-admission path.
    pub fn slots_path(&self) -> &str {
        &self.slots_path
    }

    /// Relative exact-token-count path.
    pub fn input_tokens_path(&self) -> &str {
        &self.input_tokens_path
    }

    /// Optional environment variable holding a llama.cpp bearer key.
    pub fn api_key_env(&self) -> Option<&str> {
        self.api_key_env.as_deref()
    }

    /// Optional redacted llama.cpp bearer key.
    pub fn api_key(&self) -> Option<&SecretString> {
        self.api_key.as_ref()
    }
}

/// OpenRouter connection and Auto Router policy.
#[derive(Debug, Clone)]
pub struct OpenRouterConfig {
    base_url: Url,
    api_key_env: String,
    api_key: SecretString,
    auto_model: String,
    cost_quality_tradeoff: u8,
    allowed_models: Vec<String>,
    app_title: String,
    max_in_flight: usize,
    health_cache_ttl_ms: u64,
    probe_timeout_ms: u64,
}

impl OpenRouterConfig {
    /// OpenRouter API base URL.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Environment variable holding the OpenRouter bearer key.
    pub fn api_key_env(&self) -> &str {
        &self.api_key_env
    }

    /// Redacted OpenRouter bearer key.
    pub fn api_key(&self) -> &SecretString {
        &self.api_key
    }

    /// Cloud model used for automatic routing.
    pub fn auto_model(&self) -> &str {
        &self.auto_model
    }

    /// Auto Router cost/quality control.
    pub fn cost_quality_tradeoff(&self) -> u8 {
        self.cost_quality_tradeoff
    }

    /// Optional Auto Router model allowlist patterns.
    pub fn allowed_models(&self) -> &[String] {
        &self.allowed_models
    }

    /// App attribution sent to OpenRouter.
    pub fn app_title(&self) -> &str {
        &self.app_title
    }

    /// Global cloud requests allowed concurrently.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }

    /// Duration a cloud credential probe remains authoritative.
    pub fn health_cache_ttl_ms(&self) -> u64 {
        self.health_cache_ttl_ms
    }

    /// Deadline for the OpenRouter readiness probe.
    pub fn probe_timeout_ms(&self) -> u64 {
        self.probe_timeout_ms
    }
}

/// Default automatic-route preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteDefault {
    /// Prefer compatible local work, applying the configured semantic mode.
    PreferLocal,
    /// Use cloud unless the caller explicitly requests local.
    Cloud,
}

/// Whether local semantic routing is skipped, observed, or enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRoutingMode {
    /// Skip the semantic classifier and use deterministic local admission.
    Disabled,
    /// Observe classifier decisions without letting them select a destination.
    Shadow,
    /// Enforce classifier decisions for compatible automatic requests.
    Enforced,
}

impl SemanticRoutingMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Shadow => "shadow",
            Self::Enforced => "enforced",
        }
    }
}

/// Automatic routing behavior.
#[derive(Debug, Clone)]
pub struct RoutingConfig {
    default: RouteDefault,
    fallback_before_commit: bool,
    semantic_mode: SemanticRoutingMode,
    decision_timeout_ms: u64,
    local_success_threshold: f64,
    boundary_threshold_step: f64,
    shadow_sample_rate: f64,
    session_latch: Option<SessionLatchConfig>,
}

/// Bounded in-memory policy for repeated hard cloud evidence within one session.
#[derive(Debug, Clone)]
pub struct SessionLatchConfig {
    ttl_ms: u64,
    max_entries: usize,
    evidence_threshold: u8,
}

impl RoutingConfig {
    /// Default automatic route policy.
    pub fn default(&self) -> RouteDefault {
        self.default
    }

    /// Whether automatic local requests may spill before client commitment.
    pub fn fallback_before_commit(&self) -> bool {
        self.fallback_before_commit
    }

    /// Whether semantic routing is skipped, observed, or enforced.
    pub fn semantic_mode(&self) -> SemanticRoutingMode {
        self.semantic_mode
    }

    /// Deadline for the local semantic routing decision.
    pub fn decision_timeout_ms(&self) -> u64 {
        self.decision_timeout_ms
    }

    /// Minimum forecast probability for a supported request to remain local.
    pub fn local_success_threshold(&self) -> f64 {
        self.local_success_threshold
    }

    /// Additional probability required for each capability-boundary step.
    pub fn boundary_threshold_step(&self) -> f64 {
        self.boundary_threshold_step
    }

    /// Fraction of compatible automatic shadow requests that invoke the forecaster.
    pub fn shadow_sample_rate(&self) -> f64 {
        self.shadow_sample_rate
    }

    /// Optional repeated-evidence session latch policy.
    pub fn session_latch(&self) -> Option<&SessionLatchConfig> {
        self.session_latch.as_ref()
    }
}

impl SessionLatchConfig {
    /// Time-to-live for pending evidence and active session latches.
    pub fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }

    /// Maximum number of hashed session entries retained in memory.
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Consecutive hard forecasts required before a session is latched.
    pub fn evidence_threshold(&self) -> u8 {
        self.evidence_threshold
    }
}

/// Logging configuration.
#[derive(Debug, Clone)]
pub struct ObservabilityConfig {
    log_level: LogLevel,
}

impl ObservabilityConfig {
    /// Configured log filter level.
    pub fn log_level(&self) -> LogLevel {
        self.log_level
    }
}

/// Supported base log levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Trace logging.
    Trace,
    /// Debug logging.
    Debug,
    /// Informational logging.
    Info,
    /// Warning logging.
    Warn,
    /// Error logging.
    Error,
}

impl LogLevel {
    /// Stable tracing filter representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}
