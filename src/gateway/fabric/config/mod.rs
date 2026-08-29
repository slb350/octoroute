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
    pub request_body_timeout_ms: u64,
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
    /// Deadline for one `/v1/chat/completions/input_tokens` call.
    ///
    /// Tokenizing a prompt near the request-size ceiling on a busy server takes
    /// materially longer than a `/health` or `/slots` probe, so this has its own
    /// budget rather than sharing the fixed probe deadline.
    pub token_count_timeout_ms: u64,
    /// Optional deadline for the first upstream body byte.
    ///
    /// `timeout_ms` covers the whole response, which is legitimately long for a
    /// large generation. Without a separate first-byte bound a hung member holds
    /// its member permit and the inbound permit for that entire window before
    /// the route can fall forward. Left unset, Octoroute invents no deadline.
    pub first_byte_timeout_ms: Option<u64>,
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
    /// Which runtime this provider uses, and the fields only that runtime has.
    ///
    /// The validator has always enforced a sum type here: an `http` provider
    /// must have an endpoint, a protocol, and exactly one credential source; a
    /// `codex_cli` provider must have an executable and none of those. Encoding
    /// it as a product of `Option`s meant the runtime had to recover the
    /// discriminant with `expect`, including on the request path.
    pub runtime: ProviderRuntimeConfig,
    pub model: String,
    pub max_in_flight: usize,
    pub timeout_ms: u64,
    /// Optional deadline for the first upstream body byte. See
    /// [`LocalPoolConfig::first_byte_timeout_ms`].
    pub first_byte_timeout_ms: Option<u64>,
    pub readiness_ttl_ms: u64,
    pub readiness_timeout_ms: u64,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub profile: ProviderProfile,
}

/// The runtime-specific half of a provider, as the validator enforces it.
#[derive(Debug, Clone)]
pub enum ProviderRuntimeConfig {
    /// An HTTP API reached over one of the supported wire protocols.
    Http {
        endpoint: Url,
        protocol: ProviderProtocol,
        credential: ProviderCredentialConfig,
    },
    /// A locally installed, subscription-backed Codex CLI.
    CodexCli { executable: String },
}

impl ProviderRuntimeConfig {
    /// The declared kind, for metrics and configuration reporting.
    pub const fn kind(&self) -> ProviderKind {
        match self {
            Self::Http { .. } => ProviderKind::Http,
            Self::CodexCli { .. } => ProviderKind::CodexCli,
        }
    }
}

/// Where a provider's credential comes from. Exactly one source, never both.
#[derive(Debug, Clone)]
pub enum ProviderCredentialConfig {
    /// Name of an environment variable holding the credential.
    Environment(String),
    /// Argv of a command whose stdout is the credential.
    Command(Vec<String>),
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

impl FromStr for RouteTarget {
    type Err = FabricConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // The raw step is never interpolated into the error. It is unvalidated
        // configuration text, and the safety contract keeps it out of logs and
        // messages regardless of what an operator put there.
        const EXPECTED: &str = "must use `pool:<name>` or `provider:<name>`";
        let (kind, name) = value
            .split_once(':')
            .ok_or_else(|| invalid("routing.routes.steps", EXPECTED))?;
        validate_name("routing.routes.steps", name)?;
        match kind {
            "pool" => Ok(Self::LocalPool(name.to_string())),
            "provider" => Ok(Self::Provider(name.to_string())),
            _ => Err(invalid("routing.routes.steps", EXPECTED)),
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
    /// A provider rejected, or could not supply, its credential.
    ///
    /// Not in the default set: falling forward on it turns an expired key into
    /// silently redirected traffic and spend, which an operator only discovers
    /// on the bill.
    Unauthenticated,
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

mod validation;

#[cfg(test)]
mod tests;

use validation::{RawFabricConfig, invalid, safe_parse_error, validate_name};
