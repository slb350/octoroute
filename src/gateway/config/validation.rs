use super::*;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use std::{collections::BTreeSet, net::IpAddr, str::FromStr};

const DEFAULT_MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_HEADER_BYTES: usize = 1024 * 1024;
const DEFAULT_SERVER_MAX_IN_FLIGHT: usize = 32;
const DEFAULT_REQUESTS_PER_MINUTE: u32 = 120;
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4096;
const DEFAULT_HEALTH_CACHE_TTL_MS: u64 = 1000;
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 2000;
const DEFAULT_CLOUD_MAX_IN_FLIGHT: usize = 8;
const DEFAULT_CLOUD_HEALTH_CACHE_TTL_MS: u64 = 10_000;
const DEFAULT_CLOUD_PROBE_TIMEOUT_MS: u64 = 3000;
const MAX_CONCURRENCY: usize = 10_000;
const MAX_REQUESTS_PER_MINUTE: u32 = 1_000_000;

pub(super) fn parse(
    input: &str,
    environment: &impl Environment,
) -> Result<GatewayConfig, GatewayConfigError> {
    let value: toml::Value =
        toml::from_str(input).map_err(|error| safe_parse_error(input, error))?;
    let Some(version) = value
        .get("config_version")
        .and_then(toml::Value::as_integer)
    else {
        return Err(GatewayConfigError::MigrationRequired);
    };
    if version != i64::from(CONFIG_VERSION) {
        return Err(GatewayConfigError::UnsupportedVersion { version });
    }

    let raw: RawGatewayConfig =
        toml::from_str(input).map_err(|error| safe_parse_error(input, error))?;
    raw.validate(environment)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGatewayConfig {
    #[serde(rename = "config_version")]
    _config_version: i64,
    server: RawServerConfig,
    upstreams: RawUpstreamsConfig,
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
    #[serde(default = "default_max_request_bytes")]
    max_request_bytes: usize,
    #[serde(default = "default_max_header_bytes")]
    max_header_bytes: usize,
    #[serde(default = "default_server_max_in_flight")]
    max_in_flight: usize,
    #[serde(default = "default_requests_per_minute")]
    requests_per_minute: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUpstreamsConfig {
    local: RawLocalUpstreamConfig,
    openrouter: RawOpenRouterConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocalUpstreamConfig {
    #[serde(rename = "kind")]
    _kind: LocalKind,
    name: String,
    base_url: String,
    model: String,
    context_window: u32,
    context_safety_tokens: u32,
    #[serde(default = "default_max_output_tokens")]
    default_max_output_tokens: u32,
    max_in_flight: usize,
    #[serde(default = "default_health_cache_ttl_ms")]
    health_cache_ttl_ms: u64,
    #[serde(default = "default_probe_timeout_ms")]
    probe_timeout_ms: u64,
    first_byte_timeout_ms: Option<u64>,
    capabilities: BTreeSet<LocalCapability>,
    health_path: String,
    slots_path: String,
    input_tokens_path: String,
    api_key_env: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LocalKind {
    LlamaCpp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOpenRouterConfig {
    base_url: String,
    api_key_env: String,
    #[serde(default = "default_auto_model")]
    auto_model: String,
    #[serde(default = "default_cost_quality_tradeoff")]
    cost_quality_tradeoff: u8,
    #[serde(default)]
    allowed_models: Vec<String>,
    #[serde(default = "default_app_title")]
    app_title: String,
    #[serde(default = "default_cloud_max_in_flight")]
    max_in_flight: usize,
    #[serde(default = "default_cloud_health_cache_ttl_ms")]
    health_cache_ttl_ms: u64,
    #[serde(default = "default_cloud_probe_timeout_ms")]
    probe_timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoutingConfig {
    #[serde(default = "default_route")]
    default: RouteDefault,
    #[serde(default = "default_true")]
    fallback_before_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawObservabilityConfig {
    #[serde(default = "default_log_level")]
    log_level: LogLevel,
}

impl Default for RawObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
        }
    }
}

impl RawGatewayConfig {
    fn validate(self, environment: &impl Environment) -> Result<GatewayConfig, GatewayConfigError> {
        let server = validate_server(self.server, environment)?;
        let local = validate_local(self.upstreams.local, environment)?;
        let openrouter = validate_openrouter(self.upstreams.openrouter, environment)?;

        Ok(GatewayConfig {
            server,
            local,
            openrouter,
            routing: RoutingConfig {
                default: self.routing.default,
                fallback_before_commit: self.routing.fallback_before_commit,
            },
            observability: ObservabilityConfig {
                log_level: self.observability.log_level,
            },
        })
    }
}

fn validate_server(
    raw: RawServerConfig,
    environment: &impl Environment,
) -> Result<ServerConfig, GatewayConfigError> {
    let host = parse_ip("server.host", &raw.host)?;
    if raw.port == 0 {
        return Err(invalid("server.port", "must be greater than zero"));
    }
    if !(1..=MAX_REQUEST_BYTES).contains(&raw.max_request_bytes) {
        return Err(invalid(
            "server.max_request_bytes",
            format!("must be between 1 and {MAX_REQUEST_BYTES}"),
        ));
    }
    validate_usize_range(
        "server.max_header_bytes",
        raw.max_header_bytes,
        MAX_HEADER_BYTES,
    )?;
    validate_usize_range("server.max_in_flight", raw.max_in_flight, MAX_CONCURRENCY)?;
    if !(1..=MAX_REQUESTS_PER_MINUTE).contains(&raw.requests_per_minute) {
        return Err(invalid(
            "server.requests_per_minute",
            format!("must be between 1 and {MAX_REQUESTS_PER_MINUTE}"),
        ));
    }
    let api_key = resolve_secret(environment, "server.api_key_env", &raw.api_key_env)?;

    Ok(ServerConfig {
        host,
        port: raw.port,
        api_key_env: raw.api_key_env,
        api_key,
        max_request_bytes: raw.max_request_bytes,
        max_header_bytes: raw.max_header_bytes,
        max_in_flight: raw.max_in_flight,
        requests_per_minute: raw.requests_per_minute,
    })
}

fn validate_local(
    raw: RawLocalUpstreamConfig,
    environment: &impl Environment,
) -> Result<LocalUpstreamConfig, GatewayConfigError> {
    let base_url = parse_base_url("upstreams.local.base_url", &raw.base_url, false)?;
    validate_nonempty("upstreams.local.name", &raw.name)?;
    validate_header_value("upstreams.local.name", &raw.name)?;
    validate_nonempty("upstreams.local.model", &raw.model)?;
    if raw.context_window == 0 {
        return Err(invalid(
            "upstreams.local.context_window",
            "must be greater than zero",
        ));
    }
    if raw.context_safety_tokens >= raw.context_window {
        return Err(invalid(
            "upstreams.local.context_safety_tokens",
            "must be smaller than context_window",
        ));
    }
    let usable_context = raw.context_window - raw.context_safety_tokens;
    if raw.default_max_output_tokens == 0 || raw.default_max_output_tokens >= usable_context {
        return Err(invalid(
            "upstreams.local.default_max_output_tokens",
            "must be greater than zero and leave context capacity for input",
        ));
    }
    if raw.max_in_flight == 0 {
        return Err(invalid(
            "upstreams.local.max_in_flight",
            "must be greater than zero",
        ));
    }
    for (field, value) in [
        (
            "upstreams.local.health_cache_ttl_ms",
            raw.health_cache_ttl_ms,
        ),
        ("upstreams.local.probe_timeout_ms", raw.probe_timeout_ms),
    ] {
        if value == 0 {
            return Err(invalid(field, "must be greater than zero"));
        }
    }
    if raw.first_byte_timeout_ms == Some(0) {
        return Err(invalid(
            "upstreams.local.first_byte_timeout_ms",
            "must be greater than zero when configured",
        ));
    }
    if !raw.capabilities.contains(&LocalCapability::Chat) {
        return Err(invalid(
            "upstreams.local.capabilities",
            "must include `chat`",
        ));
    }
    validate_path("upstreams.local.health_path", &raw.health_path)?;
    validate_path("upstreams.local.slots_path", &raw.slots_path)?;
    validate_path("upstreams.local.input_tokens_path", &raw.input_tokens_path)?;
    let api_key = raw
        .api_key_env
        .as_deref()
        .map(|name| resolve_secret(environment, "upstreams.local.api_key_env", name))
        .transpose()?;

    Ok(LocalUpstreamConfig {
        name: raw.name,
        base_url,
        model: raw.model,
        context_window: raw.context_window,
        context_safety_tokens: raw.context_safety_tokens,
        default_max_output_tokens: raw.default_max_output_tokens,
        max_in_flight: raw.max_in_flight,
        health_cache_ttl_ms: raw.health_cache_ttl_ms,
        probe_timeout_ms: raw.probe_timeout_ms,
        first_byte_timeout_ms: raw.first_byte_timeout_ms,
        capabilities: raw.capabilities,
        health_path: raw.health_path,
        slots_path: raw.slots_path,
        input_tokens_path: raw.input_tokens_path,
        api_key_env: raw.api_key_env,
        api_key,
    })
}

fn validate_openrouter(
    raw: RawOpenRouterConfig,
    environment: &impl Environment,
) -> Result<OpenRouterConfig, GatewayConfigError> {
    let base_url = parse_base_url("upstreams.openrouter.base_url", &raw.base_url, true)?;
    validate_nonempty("upstreams.openrouter.auto_model", &raw.auto_model)?;
    if raw.cost_quality_tradeoff > 10 {
        return Err(invalid(
            "upstreams.openrouter.cost_quality_tradeoff",
            "must be between 0 and 10",
        ));
    }
    for model in &raw.allowed_models {
        validate_nonempty("upstreams.openrouter.allowed_models", model)?;
    }
    validate_nonempty("upstreams.openrouter.app_title", &raw.app_title)?;
    validate_header_value("upstreams.openrouter.app_title", &raw.app_title)?;
    validate_usize_range(
        "upstreams.openrouter.max_in_flight",
        raw.max_in_flight,
        MAX_CONCURRENCY,
    )?;
    for (field, value) in [
        (
            "upstreams.openrouter.health_cache_ttl_ms",
            raw.health_cache_ttl_ms,
        ),
        (
            "upstreams.openrouter.probe_timeout_ms",
            raw.probe_timeout_ms,
        ),
    ] {
        if value == 0 {
            return Err(invalid(field, "must be greater than zero"));
        }
    }
    let api_key = resolve_secret(
        environment,
        "upstreams.openrouter.api_key_env",
        &raw.api_key_env,
    )?;

    Ok(OpenRouterConfig {
        base_url,
        api_key_env: raw.api_key_env,
        api_key,
        auto_model: raw.auto_model,
        cost_quality_tradeoff: raw.cost_quality_tradeoff,
        allowed_models: raw.allowed_models,
        app_title: raw.app_title,
        max_in_flight: raw.max_in_flight,
        health_cache_ttl_ms: raw.health_cache_ttl_ms,
        probe_timeout_ms: raw.probe_timeout_ms,
    })
}

fn safe_parse_error(input: &str, error: toml::de::Error) -> GatewayConfigError {
    let (line, column) = error
        .span()
        .map(|span| line_column(input, span.start))
        .unwrap_or((1, 1));
    GatewayConfigError::Parse {
        message: "document does not match the Octoroute v2 schema; values omitted".to_string(),
        line,
        column,
    }
}

fn line_column(input: &str, byte_index: usize) -> (usize, usize) {
    let prefix = &input[..byte_index.min(input.len())];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.len() + 1, |(_, tail)| tail.len() + 1);
    (line, column)
}

fn resolve_secret(
    environment: &impl Environment,
    field: &str,
    name: &str,
) -> Result<SecretString, GatewayConfigError> {
    validate_environment_name(field, name)?;
    let value = environment
        .get(name)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| GatewayConfigError::MissingEnvironmentVariable {
            field: field.to_string(),
            name: name.to_string(),
        })?;
    if !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(invalid(
            field,
            "credential must contain only visible ASCII characters without whitespace",
        ));
    }
    Ok(SecretString::from(value))
}

fn validate_environment_name(field: &str, name: &str) -> Result<(), GatewayConfigError> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_tail = chars.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if valid_start && valid_tail {
        Ok(())
    } else {
        Err(invalid(field, "must be a valid environment variable name"))
    }
}

fn parse_ip(field: &str, value: &str) -> Result<IpAddr, GatewayConfigError> {
    IpAddr::from_str(value).map_err(|_| invalid(field, "must be an IP address"))
}

fn parse_base_url(
    field: &str,
    value: &str,
    require_https: bool,
) -> Result<Url, GatewayConfigError> {
    let mut url = Url::parse(value).map_err(|_| invalid(field, "must be an absolute URL"))?;
    if require_https && url.scheme() != "https" {
        return Err(invalid(field, "must use HTTPS"));
    }
    if !require_https && !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(field, "must use HTTP or HTTPS"));
    }
    if url.host_str().is_none() {
        return Err(invalid(field, "must include a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(field, "must not contain embedded credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid(field, "must not include a query or fragment"));
    }
    if !url.path().ends_with('/') {
        let normalized = format!("{}/", url.path());
        url.set_path(&normalized);
    }
    Ok(url)
}

fn validate_path(field: &str, value: &str) -> Result<(), GatewayConfigError> {
    if !value.starts_with('/') || value.starts_with("//") || Url::parse(value).is_ok() {
        return Err(invalid(
            field,
            "must be a single-origin absolute path beginning with `/`",
        ));
    }
    Ok(())
}

fn validate_nonempty(field: &str, value: &str) -> Result<(), GatewayConfigError> {
    if value.trim().is_empty() {
        Err(invalid(field, "must not be empty"))
    } else {
        Ok(())
    }
}

fn validate_header_value(field: &str, value: &str) -> Result<(), GatewayConfigError> {
    HeaderValue::from_str(value)
        .map(|_| ())
        .map_err(|_| invalid(field, "must be safe for use as an HTTP header value"))
}

fn invalid(field: impl Into<String>, message: impl Into<String>) -> GatewayConfigError {
    GatewayConfigError::Invalid {
        field: field.into(),
        message: message.into(),
    }
}

fn validate_usize_range(
    field: &str,
    value: usize,
    maximum: usize,
) -> Result<(), GatewayConfigError> {
    if (1..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(invalid(field, format!("must be between 1 and {maximum}")))
    }
}

const fn default_max_request_bytes() -> usize {
    DEFAULT_MAX_REQUEST_BYTES
}

const fn default_max_header_bytes() -> usize {
    DEFAULT_MAX_HEADER_BYTES
}

const fn default_server_max_in_flight() -> usize {
    DEFAULT_SERVER_MAX_IN_FLIGHT
}

const fn default_requests_per_minute() -> u32 {
    DEFAULT_REQUESTS_PER_MINUTE
}

const fn default_max_output_tokens() -> u32 {
    DEFAULT_MAX_OUTPUT_TOKENS
}

const fn default_health_cache_ttl_ms() -> u64 {
    DEFAULT_HEALTH_CACHE_TTL_MS
}

const fn default_probe_timeout_ms() -> u64 {
    DEFAULT_PROBE_TIMEOUT_MS
}

fn default_auto_model() -> String {
    "openrouter/auto-beta".to_string()
}

const fn default_cost_quality_tradeoff() -> u8 {
    9
}

fn default_app_title() -> String {
    "Octoroute".to_string()
}

const fn default_cloud_max_in_flight() -> usize {
    DEFAULT_CLOUD_MAX_IN_FLIGHT
}

const fn default_cloud_health_cache_ttl_ms() -> u64 {
    DEFAULT_CLOUD_HEALTH_CACHE_TTL_MS
}

const fn default_cloud_probe_timeout_ms() -> u64 {
    DEFAULT_CLOUD_PROBE_TIMEOUT_MS
}

const fn default_route() -> RouteDefault {
    RouteDefault::PreferLocal
}

const fn default_true() -> bool {
    true
}

const fn default_log_level() -> LogLevel {
    LogLevel::Info
}
