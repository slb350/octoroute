//! Configuration management for Octoroute
//!
//! Parses TOML configuration files and provides typed access to settings.

use crate::router::TargetModel;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// Root configuration structure
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    pub models: ModelsConfig,
    pub routing: RoutingConfig,
    #[serde(default)]
    pub observability: ObservabilityConfig,
    #[serde(default)]
    pub timeouts: TimeoutsConfig,
}

/// Server configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_seconds: u64,
}

fn default_request_timeout() -> u64 {
    30
}

/// Models configuration (multi-model support)
///
/// Each tier (fast, balanced, deep) can have multiple model endpoints
/// for load balancing and failover.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelsConfig {
    pub fast: Vec<ModelEndpoint>,
    pub balanced: Vec<ModelEndpoint>,
    pub deep: Vec<ModelEndpoint>,
}

/// Individual model endpoint configuration
///
/// All fields are private to enforce invariants. Configuration is loaded via
/// deserialization and validated via Config::validate(). After construction,
/// fields cannot be mutated, ensuring validated data remains valid.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelEndpoint {
    name: String,
    base_url: String,
    max_tokens: usize,
    #[serde(default = "default_temperature")]
    temperature: f64,
    /// Load balancing weight for weighted random selection within priority tier
    #[serde(default = "default_weight")]
    weight: f64,
    /// Priority level - higher priority endpoints are tried first
    #[serde(default = "default_priority")]
    priority: u8,
}

impl ModelEndpoint {
    /// Get the endpoint name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the endpoint base URL
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Get the maximum number of tokens for this endpoint
    pub fn max_tokens(&self) -> usize {
        self.max_tokens
    }

    /// Get the temperature parameter for this endpoint
    pub fn temperature(&self) -> f64 {
        self.temperature
    }

    /// Get the load balancing weight for this endpoint
    pub fn weight(&self) -> f64 {
        self.weight
    }

    /// Get the priority level for this endpoint (higher = tried first)
    pub fn priority(&self) -> u8 {
        self.priority
    }
}

fn default_temperature() -> f64 {
    0.7
}

fn default_weight() -> f64 {
    1.0
}

fn default_priority() -> u8 {
    1
}

/// Routing configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RoutingConfig {
    pub strategy: RoutingStrategy,
    #[serde(default)]
    pub default_importance: crate::router::Importance,
    /// Which tier (Fast/Balanced/Deep) to use for LLM routing decisions
    ///
    /// Defaults to Balanced if not specified (recommended for most use cases).
    ///
    /// # Validation (Two-Phase)
    ///
    /// **Phase 1 - Format Validation (Deserialization Time)**:
    /// - **When**: During `toml::from_str::<Config>()` call (config file loading)
    /// - **Who**: Serde's `TargetModel` enum deserializer
    /// - **What**: Validates format matches "fast", "balanced", or "deep"
    ///   (case-sensitive lowercase). Invalid formats like "FAST" or "fasst" are
    ///   rejected immediately with clear serde deserialization errors.
    ///
    /// **Phase 2 - Availability Validation (Router Construction Time)**:
    /// - **When**: During `LlmBasedRouter::new()` call
    /// - **Who**: `TierSelector::new()` validation logic
    /// - **What**: Verifies at least one endpoint exists for the specified tier.
    ///   Prevents runtime failures by catching misconfiguration (e.g., router_tier="deep"
    ///   but no [[models.deep]] endpoints) at router construction time.
    ///
    /// Field is private to prevent post-validation mutation. Use `router_tier()` accessor.
    #[serde(default)]
    router_tier: TargetModel,
}

impl RoutingConfig {
    /// Get the router tier for LLM-based routing decisions
    ///
    /// The router tier determines which model tier (Fast/Balanced/Deep) is used
    /// to make routing decisions in LLM and Hybrid strategies.
    ///
    /// # Returns
    /// The configured router tier (validated during config loading)
    pub fn router_tier(&self) -> TargetModel {
        self.router_tier
    }
}

/// Routing strategy enum
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RoutingStrategy {
    Rule,
    Llm,
    Hybrid,
    Tool,
}

/// Observability configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObservabilityConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Per-tier timeout overrides
///
/// Allows configuring different timeouts for each model tier.
/// If a tier timeout is not specified in the config file, the global
/// `server.request_timeout_seconds` is used as the default.
///
/// # Custom Deserialization
///
/// This type implements custom `Deserialize` to enforce validation at parse time.
/// All timeout values must be in range (0, 300] seconds. Invalid values are rejected
/// immediately during TOML parsing, not later during `Config::validate()`.
///
/// This prevents the temporal gap where invalid `TimeoutsConfig` instances could exist
/// between deserialization and validation, upholding the principle: "make invalid states
/// unrepresentable."
#[derive(Debug, Clone, Default, Serialize)]
pub struct TimeoutsConfig {
    /// Timeout for fast tier (8B models) in seconds
    fast: Option<u64>,
    /// Timeout for balanced tier (30B models) in seconds
    balanced: Option<u64>,
    /// Timeout for deep tier (120B models) in seconds
    deep: Option<u64>,
}

impl TimeoutsConfig {
    /// Create a new TimeoutsConfig with validated timeout values
    ///
    /// # Arguments
    ///
    /// * `fast` - Optional timeout for fast tier in seconds
    /// * `balanced` - Optional timeout for balanced tier in seconds
    /// * `deep` - Optional timeout for deep tier in seconds
    ///
    /// # Errors
    ///
    /// Returns an error if any timeout is zero or exceeds 300 seconds.
    ///
    /// # Defense-in-Depth
    ///
    /// The upper bound check (timeout > 300) also handles extreme values like `u64::MAX`
    /// (18446744073709551615), which would fail this check. This prevents arithmetic
    /// overflow in timeout calculations and ensures reasonable timeout values.
    pub fn new(
        fast: Option<u64>,
        balanced: Option<u64>,
        deep: Option<u64>,
    ) -> crate::error::AppResult<Self> {
        // Validate each timeout (0, 300] seconds
        // NOTE: Upper bound also prevents u64::MAX and other extreme values
        for (tier_name, timeout_opt) in [("fast", fast), ("balanced", balanced), ("deep", deep)] {
            if let Some(timeout) = timeout_opt {
                if timeout == 0 {
                    return Err(crate::error::AppError::Config(format!(
                        "timeouts.{} must be greater than 0, got {}",
                        tier_name, timeout
                    )));
                }
                if timeout > 300 {
                    return Err(crate::error::AppError::Config(format!(
                        "timeouts.{} cannot exceed 300 seconds (5 minutes), got {}. \
                        This limit prevents connection pool exhaustion and arithmetic overflow.",
                        tier_name, timeout
                    )));
                }
            }
        }
        Ok(Self {
            fast,
            balanced,
            deep,
        })
    }

    /// Get the fast tier timeout (if configured)
    pub fn fast(&self) -> Option<u64> {
        self.fast
    }

    /// Get the balanced tier timeout (if configured)
    pub fn balanced(&self) -> Option<u64> {
        self.balanced
    }

    /// Get the deep tier timeout (if configured)
    pub fn deep(&self) -> Option<u64> {
        self.deep
    }
}

/// Custom Deserialize implementation for TimeoutsConfig
///
/// Enforces validation at deserialization time by calling the validated `new()` constructor.
/// This eliminates the temporal gap where invalid instances could exist between parsing
/// and validation.
impl<'de> Deserialize<'de> for TimeoutsConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        // Helper struct for deserializing raw values before validation
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Fast,
            Balanced,
            Deep,
        }

        struct TimeoutsConfigVisitor;

        impl<'de> Visitor<'de> for TimeoutsConfigVisitor {
            type Value = TimeoutsConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a struct with optional timeout fields (fast, balanced, deep)")
            }

            fn visit_map<V>(self, mut map: V) -> Result<TimeoutsConfig, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut fast = None;
                let mut balanced = None;
                let mut deep = None;

                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Fast => {
                            if fast.is_some() {
                                return Err(de::Error::duplicate_field("fast"));
                            }
                            fast = Some(map.next_value()?);
                        }
                        Field::Balanced => {
                            if balanced.is_some() {
                                return Err(de::Error::duplicate_field("balanced"));
                            }
                            balanced = Some(map.next_value()?);
                        }
                        Field::Deep => {
                            if deep.is_some() {
                                return Err(de::Error::duplicate_field("deep"));
                            }
                            deep = Some(map.next_value()?);
                        }
                    }
                }

                // Call validated constructor - this is where validation happens!
                TimeoutsConfig::new(fast, balanced, deep)
                    .map_err(|e| de::Error::custom(format!("Invalid timeout configuration: {}", e)))
            }
        }

        deserializer.deserialize_struct(
            "TimeoutsConfig",
            &["fast", "balanced", "deep"],
            TimeoutsConfigVisitor,
        )
    }
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> crate::error::AppResult<Self> {
        let path_display = path.as_ref().display().to_string();

        // Phase 1: Read file (preserves io::Error context)
        let content = std::fs::read_to_string(path.as_ref()).map_err(|source| {
            crate::error::AppError::ConfigFileRead {
                path: path_display.clone(),
                source,
            }
        })?;

        // Phase 2: Parse TOML (preserves toml::de::Error context)
        let config: Self = toml::from_str(&content).map_err(|source| {
            crate::error::AppError::ConfigParseFailed {
                path: path_display.clone(),
                source,
            }
        })?;

        // Phase 3: Validate parsed config (provides contextual reason)
        config
            .validate()
            .map_err(|e| crate::error::AppError::ConfigValidationFailed {
                path: path_display,
                reason: e.to_string(),
            })?;

        Ok(config)
    }

    /// Get timeout for a specific model tier
    ///
    /// Returns the per-tier timeout if configured, otherwise falls back to
    /// the global `server.request_timeout_seconds`.
    pub fn timeout_for_tier(&self, tier: crate::router::TargetModel) -> u64 {
        let tier_timeout = match tier {
            crate::router::TargetModel::Fast => self.timeouts.fast(),
            crate::router::TargetModel::Balanced => self.timeouts.balanced(),
            crate::router::TargetModel::Deep => self.timeouts.deep(),
        };

        match tier_timeout {
            Some(timeout) => {
                tracing::debug!(
                    tier = ?tier,
                    timeout_seconds = timeout,
                    "Using tier-specific timeout override"
                );
                timeout
            }
            None => {
                tracing::debug!(
                    tier = ?tier,
                    timeout_seconds = self.server.request_timeout_seconds,
                    "No tier-specific timeout configured, using global default"
                );
                self.server.request_timeout_seconds
            }
        }
    }

    /// Validate configuration after parsing
    ///
    /// This is called automatically by `from_file()`, but can also be called
    /// explicitly when constructing Config via other means (e.g., in tests).
    pub fn validate(&self) -> crate::error::AppResult<()> {
        // ═══════════════════════════════════════════════════════════════════════
        // Phase 1: Model Endpoint Field Validation
        // ═══════════════════════════════════════════════════════════════════════
        //
        // Validates individual endpoint configuration fields across all tiers:
        //   - max_tokens: must fit in u32, must have defensive upper bound
        //   - base_url: must start with http:// or https://, must end with /v1
        //   - temperature: must be finite number between 0.0 and 2.0
        //
        // Validate ModelEndpoint fields across all tiers
        for (tier_name, endpoints) in [
            ("fast", &self.models.fast),
            ("balanced", &self.models.balanced),
            ("deep", &self.models.deep),
        ] {
            for endpoint in endpoints {
                // Validate weight: must be positive and not NaN
                if endpoint.weight <= 0.0
                    || endpoint.weight.is_nan()
                    || endpoint.weight.is_infinite()
                {
                    return Err(crate::error::AppError::Config(format!(
                        "Configuration error: Endpoint '{}' in tier '{}' has invalid weight {}. \
                        Weight must be a positive finite number.",
                        endpoint.name, tier_name, endpoint.weight
                    )));
                }

                // Validate max_tokens: must be positive
                if endpoint.max_tokens == 0 {
                    return Err(crate::error::AppError::Config(format!(
                        "Configuration error: Endpoint '{}' in tier '{}' has max_tokens=0. \
                        max_tokens must be greater than 0.",
                        endpoint.name, tier_name
                    )));
                }

                // Validate max_tokens: must not exceed u32::MAX
                //
                // This validation serves two purposes:
                //
                // 1. **SDK Compatibility**: open-agent-sdk requires max_tokens to fit in u32
                //    for API compatibility. Values must be <= 4,294,967,295 (u32::MAX).
                //
                // 2. **Defensive Upper Bound**: Most LLMs don't support >4 billion tokens.
                //    This check prevents unreasonable values (like usize::MAX on 64-bit systems)
                //    from being silently truncated or causing unexpected behavior.
                //
                // Combined, these checks ensure both API correctness and reasonable limits.
                if endpoint.max_tokens > u32::MAX as usize {
                    return Err(crate::error::AppError::Config(format!(
                        "Configuration error: Endpoint '{}' in tier '{}' has max_tokens={} which exceeds u32::MAX ({}). \
                        max_tokens must fit in u32 for compatibility with open-agent-sdk.",
                        endpoint.name,
                        tier_name,
                        endpoint.max_tokens,
                        u32::MAX
                    )));
                }

                // Validate base_url: must start with http:// or https://
                if !endpoint.base_url.starts_with("http://")
                    && !endpoint.base_url.starts_with("https://")
                {
                    return Err(crate::error::AppError::Config(format!(
                        "Configuration error: Endpoint '{}' in tier '{}' has invalid base_url '{}'. \
                        base_url must start with 'http://' or 'https://'.",
                        endpoint.name, tier_name, endpoint.base_url
                    )));
                }

                // Validate base_url: must end with /v1
                // This is required because health checks append "/models" to get "/v1/models"
                // Without this validation, users might configure "http://host:port" which would
                // result in health checks trying "/models" (404) instead of "/v1/models"
                if !endpoint.base_url.ends_with("/v1") {
                    return Err(crate::error::AppError::Config(format!(
                        "Configuration error: Endpoint '{}' in tier '{}' has invalid base_url '{}'. \
                        base_url must end with '/v1' (e.g., 'http://host:port/v1'). \
                        This is required for health checks to work correctly.",
                        endpoint.name, tier_name, endpoint.base_url
                    )));
                }

                // Validate temperature: must be between 0.0 and 2.0 (standard LLM range)
                if endpoint.temperature < 0.0
                    || endpoint.temperature > 2.0
                    || endpoint.temperature.is_nan()
                    || endpoint.temperature.is_infinite()
                {
                    return Err(crate::error::AppError::Config(format!(
                        "Configuration error: Endpoint '{}' in tier '{}' has invalid temperature {}. \
                        temperature must be a finite number between 0.0 and 2.0.",
                        endpoint.name, tier_name, endpoint.temperature
                    )));
                }
            }
        }

        // ═══════════════════════════════════════════════════════════════════════
        // Phase 2: All-Tier Availability Validation
        // ═══════════════════════════════════════════════════════════════════════
        //
        // P1 FIX: Require ALL tiers (Fast/Balanced/Deep) to have at least one endpoint.
        //
        // RATIONALE: Both RuleBasedRouter and LlmBasedRouter can route to ANY tier:
        //   - RuleBasedRouter routes CasualChat → Fast, High importance → Deep, etc.
        //   - LlmBasedRouter can return any tier based on prompt analysis
        //   - If a tier is empty and gets selected, request fails at runtime with
        //     "No available healthy endpoints" instead of failing at startup validation.
        //
        // This validation ensures config errors are caught at startup, not runtime.
        //
        // Note: router_tier format is already validated during deserialization (Phase 1).

        // Check Fast tier
        if self.models.fast.is_empty() {
            return Err(crate::error::AppError::Config(
                "Configuration error: models.fast has no endpoints. \
                All three tiers (fast, balanced, deep) must have at least one endpoint \
                because routers can select any tier based on request characteristics.\n\n\
                Example fix - add to config.toml:\n\
                [[models.fast]]\n\
                name = \"my-fast-model\"\n\
                base_url = \"http://localhost:11434/v1\"\n\
                max_tokens = 2048\n\
                temperature = 0.7\n\
                weight = 1.0\n\
                priority = 1"
                    .to_string(),
            ));
        }

        // Check Balanced tier
        if self.models.balanced.is_empty() {
            return Err(crate::error::AppError::Config(
                "Configuration error: models.balanced has no endpoints. \
                All three tiers (fast, balanced, deep) must have at least one endpoint \
                because routers can select any tier based on request characteristics.\n\n\
                Example fix - add to config.toml:\n\
                [[models.balanced]]\n\
                name = \"my-balanced-model\"\n\
                base_url = \"http://localhost:1234/v1\"\n\
                max_tokens = 4096\n\
                temperature = 0.7\n\
                weight = 1.0\n\
                priority = 1"
                    .to_string(),
            ));
        }

        // Check Deep tier
        if self.models.deep.is_empty() {
            return Err(crate::error::AppError::Config(
                "Configuration error: models.deep has no endpoints. \
                All three tiers (fast, balanced, deep) must have at least one endpoint \
                because routers can select any tier based on request characteristics.\n\n\
                Example fix - add to config.toml:\n\
                [[models.deep]]\n\
                name = \"my-deep-model\"\n\
                base_url = \"http://localhost:8080/v1\"\n\
                max_tokens = 8192\n\
                temperature = 0.7\n\
                weight = 1.0\n\
                priority = 1"
                    .to_string(),
            ));
        }

        // Validate request timeout
        if self.server.request_timeout_seconds == 0 {
            return Err(crate::error::AppError::Config(
                "Configuration error: request_timeout_seconds must be greater than 0".to_string(),
            ));
        }
        if self.server.request_timeout_seconds > 300 {
            return Err(crate::error::AppError::Config(format!(
                "Configuration error: request_timeout_seconds cannot exceed 300 seconds (5 minutes), got {}",
                self.server.request_timeout_seconds
            )));
        }

        // Per-tier timeout validation is now handled by TimeoutsConfig's custom Deserialize
        // implementation, which calls the validated constructor at parse time.
        // No duplicate validation needed here.

        Ok(())
    }
}

impl FromStr for Config {
    type Err = crate::error::AppError;

    fn from_str(toml_str: &str) -> Result<Self, Self::Err> {
        let config: Config = toml::from_str(toml_str).map_err(|source| {
            crate::error::AppError::ConfigParseFailed {
                path: "<string>".to_string(),
                source,
            }
        })?;

        // Validate config before returning
        config.validate()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CONFIG: &str = r#"
[server]
host = "0.0.0.0"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "qwen/qwen3-vl-8b"
base_url = "http://192.168.1.67:1234/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.fast]]
name = "qwen/qwen3-vl-8b"
base_url = "http://192.168.1.72:1234/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "qwen/qwen3-30b-a3b-2507"
base_url = "http://192.168.1.61:1234/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "/home/steve/dev/llama.cpp/models/gpt-oss-120b-mxfp4.gguf"
base_url = "https://strix-ai.localbrandonfamily.com/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    #[test]
    fn test_config_from_str_parses_successfully() {
        let config = Config::from_str(TEST_CONFIG).expect("should parse config");
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 3000);
        assert_eq!(config.server.request_timeout_seconds, 30);
    }

    #[test]
    fn test_config_parses_model_endpoints() {
        let config = Config::from_str(TEST_CONFIG).expect("should parse config");

        // Fast tier - 2 models
        assert_eq!(config.models.fast.len(), 2);
        assert_eq!(config.models.fast[0].name, "qwen/qwen3-vl-8b");
        assert_eq!(
            config.models.fast[0].base_url,
            "http://192.168.1.67:1234/v1"
        );
        assert_eq!(config.models.fast[0].max_tokens, 4096);
        assert_eq!(config.models.fast[0].weight, 1.0);
        assert_eq!(config.models.fast[0].priority, 1);

        assert_eq!(
            config.models.fast[1].base_url,
            "http://192.168.1.72:1234/v1"
        );

        // Balanced tier - 1 model
        assert_eq!(config.models.balanced.len(), 1);
        assert_eq!(config.models.balanced[0].name, "qwen/qwen3-30b-a3b-2507");

        // Deep tier - 1 model
        assert_eq!(config.models.deep.len(), 1);
        assert_eq!(config.models.deep[0].max_tokens, 16384);
    }

    #[test]
    fn test_config_parses_routing_strategy() {
        let config = Config::from_str(TEST_CONFIG).expect("should parse config");
        assert_eq!(config.routing.strategy, RoutingStrategy::Hybrid);
        assert_eq!(
            config.routing.default_importance,
            crate::router::Importance::Normal
        );
        assert_eq!(config.routing.router_tier(), TargetModel::Balanced);
    }

    #[test]
    fn test_config_parses_observability() {
        let config = Config::from_str(TEST_CONFIG).expect("should parse config");
        assert_eq!(config.observability.log_level, "info");
    }

    #[test]
    fn test_routing_strategy_enum_values() {
        assert_eq!(
            serde_json::from_str::<RoutingStrategy>(r#""rule""#).unwrap(),
            RoutingStrategy::Rule
        );
        assert_eq!(
            serde_json::from_str::<RoutingStrategy>(r#""llm""#).unwrap(),
            RoutingStrategy::Llm
        );
        assert_eq!(
            serde_json::from_str::<RoutingStrategy>(r#""hybrid""#).unwrap(),
            RoutingStrategy::Hybrid
        );
        assert_eq!(
            serde_json::from_str::<RoutingStrategy>(r#""tool""#).unwrap(),
            RoutingStrategy::Tool
        );
    }

    #[test]
    fn test_config_with_missing_observability_uses_defaults() {
        let minimal_config = r#"
[server]
host = "127.0.0.1"
port = 8080

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
default_importance = "normal"
router_tier = "balanced"
"#;

        let config = Config::from_str(minimal_config).expect("should parse minimal config");
        assert_eq!(config.observability.log_level, "info");

        // Verify defaults for weight and priority
        assert_eq!(config.models.fast[0].weight, 1.0);
        assert_eq!(config.models.fast[0].priority, 1);
    }

    // REMOVED: test_config_validation_empty_fast_tier_allowed_for_non_router_tier
    // This test was validating the OLD buggy behavior where empty tiers were allowed.
    // P1 FIX: All tiers must have endpoints because routers can select any tier.
    // See tests/all_tier_validation.rs for new comprehensive validation tests.

    #[test]
    fn test_config_validation_invalid_router_tier_fails() {
        let config_str = r#"
[server]
host = "127.0.0.1"
port = 8080

[[models.fast]]
name = "test"
base_url = "http://localhost:1234/v1"
max_tokens = 4096

[[models.balanced]]
name = "test"
base_url = "http://localhost:1235/v1"
max_tokens = 8192

[[models.deep]]
name = "test"
base_url = "http://localhost:1236/v1"
max_tokens = 16384

[routing]
strategy = "rule"
router_tier = "invalid"
"#;

        // Serde should reject invalid router_tier at deserialization time
        let result = Config::from_str(config_str);
        assert!(
            result.is_err(),
            "Should fail to deserialize invalid router_tier"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("router_tier") || err_msg.contains("invalid"));
        assert!(
            err_msg.contains("fast") || err_msg.contains("balanced") || err_msg.contains("deep"),
            "Error should list valid values"
        );
    }

    #[test]
    fn test_config_validation_router_tier_with_no_endpoints_fails() {
        // Parse config with router_tier="deep" and LLM strategy
        let config_str = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "llm"
default_importance = "normal"
router_tier = "deep"
"#;
        let mut config = Config::from_str(config_str).unwrap();

        // Clear deep endpoints to test validation
        config.models.deep.clear();

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("deep") || err_msg.contains("Deep"));
        assert!(err_msg.contains("endpoint"));
    }

    // Note: Format validation tests removed - serde now validates router_tier format
    // at deserialization time. Invalid values like "" or "FAST" are caught by serde's
    // enum deserializer, not by Config::validate(). This provides stronger type safety
    // - invalid configs can't even be deserialized, preventing the need for runtime
    // validation.

    #[test]
    fn test_config_router_tier_defaults_to_balanced() {
        // Test that configs without router_tier field use Balanced as default
        // This ensures backward compatibility with configs that don't specify router_tier
        let config_str = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
# router_tier omitted - should default to balanced
"#;

        let config: Config =
            toml::from_str(config_str).expect("should parse config without router_tier");

        assert_eq!(
            config.routing.router_tier(),
            TargetModel::Balanced,
            "router_tier should default to Balanced when omitted"
        );
    }

    // REMOVED: test_config_validation_rule_strategy_allows_empty_router_tier
    // This test was validating the OLD buggy behavior where Rule strategy could have empty tiers.
    // P1 FIX: RuleBasedRouter routes to ANY tier (Fast/Balanced/Deep), not just router_tier.
    // Example: CasualChat routes to Fast, High importance routes to Deep.
    // If those tiers are empty, requests fail at runtime with "No available healthy endpoints".
    // See tests/all_tier_validation.rs for new comprehensive validation tests.

    #[test]
    fn test_config_validation_negative_weight_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.models.fast[0].weight = -1.0; // Invalid: negative weight

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("weight"));
        assert!(err_msg.contains("positive"));
    }

    #[test]
    fn test_config_validation_zero_weight_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.models.balanced[0].weight = 0.0; // Invalid: zero weight

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("weight"));
        assert!(err_msg.contains("positive"));
    }

    #[test]
    fn test_config_validation_nan_weight_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.models.deep[0].weight = f64::NAN; // Invalid: NaN weight

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("weight"));
    }

    #[test]
    fn test_config_validation_zero_max_tokens_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.models.fast[0].max_tokens = 0; // Invalid: zero max_tokens

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("max_tokens"));
        assert!(err_msg.contains("greater than 0"));
    }

    #[test]
    fn test_config_validation_invalid_base_url_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.models.balanced[0].base_url = "ftp://invalid.com".to_string(); // Invalid: not http/https

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("base_url"));
        assert!(err_msg.contains("http"));
    }

    #[test]
    fn test_config_validation_missing_protocol_base_url_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.models.deep[0].base_url = "localhost:1234/v1".to_string(); // Invalid: missing protocol

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("base_url"));
        assert!(err_msg.contains("http"));
    }

    #[test]
    fn test_config_validation_base_url_must_end_with_v1() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.models.fast[0].base_url = "http://localhost:1234".to_string(); // Invalid: missing /v1

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("base_url"));
        assert!(err_msg.contains("/v1"));
        assert!(err_msg.contains("health checks"));
    }

    #[test]
    fn test_config_validation_zero_timeout_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.server.request_timeout_seconds = 0; // Invalid: zero timeout

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("request_timeout_seconds") && err_msg.contains("greater than 0"),
            "Expected error about request_timeout_seconds > 0, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_config_validation_excessive_timeout_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.server.request_timeout_seconds = 301; // Invalid: exceeds 300 second limit

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("request_timeout_seconds") && err_msg.contains("300"),
            "Expected error about request_timeout_seconds max 300, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_config_validation_valid_timeout_succeeds() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();

        // Test lower bound (1 second)
        config.server.request_timeout_seconds = 1;
        assert!(config.validate().is_ok());

        // Test upper bound (300 seconds)
        config.server.request_timeout_seconds = 300;
        assert!(config.validate().is_ok());

        // Test typical value (30 seconds)
        config.server.request_timeout_seconds = 30;
        assert!(config.validate().is_ok());
    }

    // ===== Per-Tier Timeout Tests (RED phase) =====

    #[test]
    fn test_config_parses_per_tier_timeouts() {
        let config_with_timeouts = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 4096

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 8192

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 16384

[routing]
strategy = "rule"
router_tier = "balanced"

[timeouts]
fast = 15
balanced = 30
deep = 60
"#;

        let config =
            Config::from_str(config_with_timeouts).expect("should parse config with timeouts");
        assert_eq!(config.timeouts.fast, Some(15));
        assert_eq!(config.timeouts.balanced, Some(30));
        assert_eq!(config.timeouts.deep, Some(60));
    }

    #[test]
    fn test_config_timeouts_optional_fields_default_to_none() {
        let config_partial_timeouts = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 4096

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 8192

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 16384

[routing]
strategy = "rule"
router_tier = "balanced"

[timeouts]
fast = 15
# balanced and deep use global default
"#;

        let config =
            Config::from_str(config_partial_timeouts).expect("should parse partial timeouts");
        assert_eq!(config.timeouts.fast, Some(15));
        assert_eq!(config.timeouts.balanced, None); // Uses global default
        assert_eq!(config.timeouts.deep, None); // Uses global default
    }

    #[test]
    fn test_config_timeouts_section_optional() {
        // Config without [timeouts] section should work
        let config = Config::from_str(TEST_CONFIG).expect("should parse without timeouts section");
        assert_eq!(config.timeouts.fast, None);
        assert_eq!(config.timeouts.balanced, None);
        assert_eq!(config.timeouts.deep, None);
    }

    // REMOVED: test_config_validation_per_tier_timeout_too_low_fails
    // REMOVED: test_config_validation_per_tier_timeout_too_high_fails
    // REMOVED: test_config_validation_per_tier_timeouts_valid_succeeds
    //
    // These tests tested the OLD behavior where timeouts.fast/balanced/deep fields were
    // public and could be modified after construction, with validation happening later
    // during validate(). This created a temporal gap where invalid TimeoutsConfig
    // instances could exist.
    //
    // With Issue #3 fix (custom Deserialize), these fields are now PRIVATE and validation
    // happens DURING deserialization. Invalid values are rejected immediately at parse time.
    // See new tests: test_timeouts_config_deserialization_* for the CORRECT behavior.

    #[test]
    fn test_config_timeout_for_tier_uses_override() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.server.request_timeout_seconds = 30; // Global default
        config.timeouts.fast = Some(15);
        config.timeouts.balanced = Some(45);
        config.timeouts.deep = Some(60);

        // Should use per-tier overrides
        assert_eq!(
            config.timeout_for_tier(crate::router::TargetModel::Fast),
            15
        );
        assert_eq!(
            config.timeout_for_tier(crate::router::TargetModel::Balanced),
            45
        );
        assert_eq!(
            config.timeout_for_tier(crate::router::TargetModel::Deep),
            60
        );
    }

    #[test]
    fn test_config_timeout_for_tier_uses_global_default() {
        let config = Config::from_str(TEST_CONFIG).unwrap();
        // No per-tier overrides, should use global default (30s)

        assert_eq!(
            config.timeout_for_tier(crate::router::TargetModel::Fast),
            30
        );
        assert_eq!(
            config.timeout_for_tier(crate::router::TargetModel::Balanced),
            30
        );
        assert_eq!(
            config.timeout_for_tier(crate::router::TargetModel::Deep),
            30
        );
    }

    #[test]
    fn test_config_timeout_for_tier_mixed_overrides() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.server.request_timeout_seconds = 40; // Global default
        config.timeouts.fast = Some(20); // Override only fast tier

        // Fast tier uses override
        assert_eq!(
            config.timeout_for_tier(crate::router::TargetModel::Fast),
            20
        );
        // Balanced and deep use global default
        assert_eq!(
            config.timeout_for_tier(crate::router::TargetModel::Balanced),
            40
        );
        assert_eq!(
            config.timeout_for_tier(crate::router::TargetModel::Deep),
            40
        );
    }

    // ===== Issue #3 Fix: TimeoutsConfig Custom Deserialize Tests =====
    // Tests written FIRST (TDD RED phase) - these should fail until custom Deserialize is implemented

    #[test]
    fn test_timeouts_config_deserialization_rejects_zero_timeout() {
        // Test that zero timeout is rejected DURING deserialization, not later during validate()
        let config_with_zero_timeout = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 4096

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 8192

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 16384

[routing]
strategy = "rule"
router_tier = "balanced"

[timeouts]
fast = 0
"#;

        let result = Config::from_str(config_with_zero_timeout);
        assert!(
            result.is_err(),
            "Config parsing should fail with zero timeout (custom Deserialize should reject it)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("fast") && err_msg.contains("timeout"),
            "Error should mention which timeout field is invalid"
        );
    }

    #[test]
    fn test_timeouts_config_deserialization_rejects_timeout_too_high() {
        // Test that timeout > 300 is rejected DURING deserialization
        let config_with_high_timeout = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 4096

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 8192

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 16384

[routing]
strategy = "rule"
router_tier = "balanced"

[timeouts]
deep = 301
"#;

        let result = Config::from_str(config_with_high_timeout);
        assert!(
            result.is_err(),
            "Config parsing should fail with timeout > 300 (custom Deserialize should reject it)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("deep") && err_msg.contains("300"),
            "Error should mention which timeout field exceeds limit"
        );
    }

    #[test]
    fn test_timeouts_config_deserialization_accepts_valid_timeouts() {
        // Test that valid timeouts are accepted during deserialization
        let config_with_valid_timeouts = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 4096

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 8192

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 16384

[routing]
strategy = "rule"
router_tier = "balanced"

[timeouts]
fast = 15
balanced = 30
deep = 60
"#;

        let result = Config::from_str(config_with_valid_timeouts);
        assert!(
            result.is_ok(),
            "Config parsing should succeed with valid timeouts (1-300)"
        );
        let config = result.unwrap();
        assert_eq!(config.timeouts.fast(), Some(15));
        assert_eq!(config.timeouts.balanced(), Some(30));
        assert_eq!(config.timeouts.deep(), Some(60));
    }

    #[test]
    fn test_timeouts_config_deserialization_accepts_boundary_values() {
        // Test that boundary values (1 and 300) are accepted
        let config_with_boundaries = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 4096

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 8192

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 16384

[routing]
strategy = "rule"
router_tier = "balanced"

[timeouts]
fast = 1
deep = 300
"#;

        let result = Config::from_str(config_with_boundaries);
        assert!(
            result.is_ok(),
            "Config parsing should succeed with boundary values 1 and 300"
        );
        let config = result.unwrap();
        assert_eq!(config.timeouts.fast(), Some(1));
        assert_eq!(config.timeouts.deep(), Some(300));
    }
}
