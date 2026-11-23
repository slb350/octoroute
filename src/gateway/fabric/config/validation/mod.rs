//! Raw v3 TOML shapes and the invariant checks that validate them.
//!
//! [`targets`] validates providers and routes; [`fields`] holds the shared
//! field-level validators and the safe error constructors. Nothing here is
//! public: the only way out is `FabricConfig::from_toml`, which returns the
//! validated types in the parent module.

mod fields;
mod targets;

#[cfg(test)]
mod tests;

pub(super) use fields::{invalid, safe_parse_error, validate_name};
use fields::{
    validate_env_name, validate_first_byte_timeout, validate_local_member_url, validate_log_level,
    validate_model, validate_positive_range, validate_revision, validate_url,
};
use targets::{validate_providers, validate_routes};

use super::{
    FABRIC_CONFIG_VERSION, FabricConfig, FabricConfigError, FabricObservabilityConfig,
    FabricServerConfig, FallbackTrigger, LocalCapability, LocalMemberConfig, LocalPoolConfig,
    PoolStrategy, ProviderKind, ProviderProfile, ProviderProtocol, ReasoningEffort, RoutePrivacy,
};
pub(super) const DEFAULT_SERVER_MAX_REQUEST_BYTES: usize = 8_388_608; // 8 MiB
pub(super) const DEFAULT_SERVER_REQUEST_BODY_TIMEOUT_MS: u64 = 30_000;
pub(super) const DEFAULT_SERVER_MAX_HEADER_BYTES: usize = 32_768; // 32 KiB
pub(super) const DEFAULT_SERVER_MAX_IN_FLIGHT: usize = 64;
pub(super) const DEFAULT_SERVER_REQUESTS_PER_MINUTE: u32 = 120;
pub(super) const DEFAULT_MEMBER_MAX_IN_FLIGHT: usize = 1;
pub(super) const DEFAULT_PROVIDER_MAX_IN_FLIGHT: usize = 1;
pub(super) const MAX_REQUEST_BYTES: usize = 67_108_864; // 64 MiB
pub(super) const MAX_SERVER_REQUEST_BODY_TIMEOUT_MS: u64 = 300_000;
pub(super) const MAX_HEADER_BYTES: usize = 1_048_576; // 1 MiB
pub(super) const MAX_CONCURRENCY: usize = 10_000;
pub(super) const MAX_REQUESTS_PER_MINUTE: u32 = 1_000_000;
pub(super) const DEFAULT_CONTEXT_SAFETY_TOKENS: u32 = 2_048;
pub(super) const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 16_384;
pub(super) const DEFAULT_UPSTREAM_TIMEOUT_MS: u64 = 1_800_000;
pub(super) const DEFAULT_TOKEN_COUNT_TIMEOUT_MS: u64 = 15_000;
pub(super) const MAX_TOKEN_COUNT_TIMEOUT_MS: u64 = 120_000;
pub(super) const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 1_800_000;
pub(super) const DEFAULT_PROVIDER_READINESS_TTL_MS: u64 = 30_000;
pub(super) const DEFAULT_PROVIDER_READINESS_TIMEOUT_MS: u64 = 30_000;
pub(super) const MAX_PROVIDER_READINESS_TTL_MS: u64 = 3_600_000;
pub(super) const MAX_PROVIDER_READINESS_TIMEOUT_MS: u64 = 300_000;
pub(super) const MAX_UPSTREAM_TIMEOUT_MS: u64 = 3_600_000;
pub(super) const MAX_COMMAND_ARGUMENTS: usize = 32;
pub(super) const MAX_COMMAND_ARGUMENT_BYTES: usize = 4_096; // 4 KiB
pub(super) const MAX_COMMAND_BYTES: usize = 16_384; // 16 KiB
pub(super) const MAX_MODEL_BYTES: usize = 512;
pub(super) const MAX_ENV_NAME_BYTES: usize = 128;
pub(super) const DEFAULT_PRIORITY: u16 = 100;
pub(super) const DEFAULT_CODEX_EXECUTABLE: &str = "codex";
pub(super) const DEFAULT_LOG_LEVEL: &str = "info";
pub(super) const DEFAULT_FALLBACK_TRIGGERS: [FallbackTrigger; 6] = [
    FallbackTrigger::Busy,
    FallbackTrigger::Unhealthy,
    FallbackTrigger::ContextOverflow,
    FallbackTrigger::Incompatible,
    FallbackTrigger::RateLimited,
    FallbackTrigger::PrecommitFailure,
];

use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawFabricConfig {
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
    #[serde(default)]
    max_request_bytes: Option<usize>,
    #[serde(default)]
    request_body_timeout_ms: Option<u64>,
    #[serde(default)]
    max_header_bytes: Option<usize>,
    #[serde(default)]
    max_in_flight: Option<usize>,
    #[serde(default)]
    requests_per_minute: Option<u32>,
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
    #[serde(default)]
    enabled: Option<bool>,
    model: String,
    model_revision: String,
    context_window: u32,
    #[serde(default)]
    context_safety_tokens: Option<u32>,
    #[serde(default)]
    default_max_output_tokens: Option<u32>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    token_count_timeout_ms: Option<u64>,
    #[serde(default)]
    first_byte_timeout_ms: Option<u64>,
    capabilities: BTreeSet<LocalCapability>,
    #[serde(default)]
    strategy: PoolStrategy,
    #[serde(default)]
    default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    members: Vec<RawLocalMemberConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocalMemberConfig {
    name: String,
    #[serde(default)]
    enabled: Option<bool>,
    base_url: String,
    api_key_env: Option<String>,
    #[serde(default)]
    max_in_flight: Option<usize>,
    #[serde(default)]
    priority: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProviderConfig {
    name: String,
    #[serde(default)]
    enabled: Option<bool>,
    kind: ProviderKind,
    endpoint: Option<String>,
    protocol: Option<ProviderProtocol>,
    model: String,
    api_key_env: Option<String>,
    api_key_command: Option<Vec<String>>,
    #[serde(default)]
    max_in_flight: Option<usize>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    first_byte_timeout_ms: Option<u64>,
    #[serde(default)]
    readiness_ttl_ms: Option<u64>,
    #[serde(default)]
    readiness_timeout_ms: Option<u64>,
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
    #[serde(default)]
    default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    fallback_on: Option<BTreeSet<FallbackTrigger>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawObservabilityConfig {
    #[serde(default)]
    log_level: Option<String>,
}

impl RawFabricConfig {
    pub(super) fn validate(self) -> Result<FabricConfig, FabricConfigError> {
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
        let log_level = self
            .observability
            .log_level
            .unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_string());
        validate_log_level(&log_level)?;

        Ok(FabricConfig {
            server,
            local_pools,
            providers,
            routes,
            default_model: self.routing.default_model,
            observability: FabricObservabilityConfig { log_level },
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
        let max_request_bytes = self
            .max_request_bytes
            .unwrap_or(DEFAULT_SERVER_MAX_REQUEST_BYTES);
        let request_body_timeout_ms = self
            .request_body_timeout_ms
            .unwrap_or(DEFAULT_SERVER_REQUEST_BODY_TIMEOUT_MS);
        let max_header_bytes = self
            .max_header_bytes
            .unwrap_or(DEFAULT_SERVER_MAX_HEADER_BYTES);
        let max_in_flight = self.max_in_flight.unwrap_or(DEFAULT_SERVER_MAX_IN_FLIGHT);
        let requests_per_minute = self
            .requests_per_minute
            .unwrap_or(DEFAULT_SERVER_REQUESTS_PER_MINUTE);
        validate_positive_range(
            "server.max_request_bytes",
            max_request_bytes,
            MAX_REQUEST_BYTES,
        )?;
        validate_positive_range(
            "server.request_body_timeout_ms",
            request_body_timeout_ms,
            MAX_SERVER_REQUEST_BODY_TIMEOUT_MS,
        )?;
        validate_positive_range(
            "server.max_header_bytes",
            max_header_bytes,
            MAX_HEADER_BYTES,
        )?;
        validate_positive_range("server.max_in_flight", max_in_flight, MAX_CONCURRENCY)?;
        validate_positive_range(
            "server.requests_per_minute",
            requests_per_minute,
            MAX_REQUESTS_PER_MINUTE,
        )?;
        Ok(FabricServerConfig {
            host,
            port: self.port,
            api_key_env: self.api_key_env,
            max_request_bytes,
            request_body_timeout_ms,
            max_header_bytes,
            max_in_flight,
            requests_per_minute,
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
        let context_safety_tokens = raw
            .context_safety_tokens
            .unwrap_or(DEFAULT_CONTEXT_SAFETY_TOKENS);
        let default_max_output_tokens = raw
            .default_max_output_tokens
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
        let timeout_ms = raw.timeout_ms.unwrap_or(DEFAULT_UPSTREAM_TIMEOUT_MS);
        let token_count_timeout_ms = raw
            .token_count_timeout_ms
            .unwrap_or(DEFAULT_TOKEN_COUNT_TIMEOUT_MS);
        if raw.context_window == 0 {
            return Err(invalid(
                "fabric.local_pools.context_window",
                "must be greater than zero",
            ));
        }
        if context_safety_tokens >= raw.context_window {
            return Err(invalid(
                "fabric.local_pools.context_safety_tokens",
                "must be smaller than context_window",
            ));
        }
        let usable_context = raw.context_window - context_safety_tokens;
        if default_max_output_tokens == 0 || default_max_output_tokens >= usable_context {
            return Err(invalid(
                "fabric.local_pools.default_max_output_tokens",
                "must be positive and leave room for input",
            ));
        }
        validate_positive_range(
            "fabric.local_pools.timeout_ms",
            timeout_ms,
            MAX_UPSTREAM_TIMEOUT_MS,
        )?;
        validate_positive_range(
            "fabric.local_pools.token_count_timeout_ms",
            token_count_timeout_ms,
            MAX_TOKEN_COUNT_TIMEOUT_MS,
        )?;
        validate_first_byte_timeout(
            "fabric.local_pools.first_byte_timeout_ms",
            timeout_ms,
            raw.first_byte_timeout_ms,
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
            let max_in_flight = member.max_in_flight.unwrap_or(DEFAULT_MEMBER_MAX_IN_FLIGHT);
            validate_positive_range(
                "fabric.local_pools.members.max_in_flight",
                max_in_flight,
                MAX_CONCURRENCY,
            )?;
            if let Some(name) = member.api_key_env.as_deref() {
                validate_env_name("fabric.local_pools.members.api_key_env", name)?;
            }
            members.push(LocalMemberConfig {
                name: member.name,
                enabled: member.enabled.unwrap_or(true),
                base_url: {
                    let url = validate_url(
                        "fabric.local_pools.members.base_url",
                        &member.base_url,
                        false,
                    )?;
                    validate_local_member_url("fabric.local_pools.members.base_url", &url)?;
                    url
                },
                api_key_env: member.api_key_env,
                max_in_flight,
                priority: member.priority.unwrap_or(DEFAULT_PRIORITY),
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
            enabled: raw.enabled.unwrap_or(true),
            model: raw.model,
            model_revision: raw.model_revision,
            context_window: raw.context_window,
            context_safety_tokens,
            default_max_output_tokens,
            timeout_ms,
            token_count_timeout_ms,
            first_byte_timeout_ms: raw.first_byte_timeout_ms,
            capabilities: raw.capabilities,
            strategy: raw.strategy,
            default_reasoning_effort: raw
                .default_reasoning_effort
                .unwrap_or(ReasoningEffort::Medium),
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
