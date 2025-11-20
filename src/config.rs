//! Configuration management for Octoroute
//!
//! Parses TOML configuration files and provides typed access to settings.

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
    pub router_model: String,
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
    #[serde(default)]
    pub metrics_enabled: bool,
    #[serde(default = "default_metrics_port")]
    pub metrics_port: u16,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
            metrics_enabled: false,
            metrics_port: default_metrics_port(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_metrics_port() -> u16 {
    9090
}

/// Per-tier timeout overrides
///
/// Allows configuring different timeouts for each model tier.
/// If a tier timeout is None, the global `server.request_timeout_seconds` is used.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TimeoutsConfig {
    /// Timeout for fast tier (8B models) in seconds
    #[serde(default)]
    pub fast: Option<u64>,
    /// Timeout for balanced tier (30B models) in seconds
    #[serde(default)]
    pub balanced: Option<u64>,
    /// Timeout for deep tier (120B models) in seconds
    #[serde(default)]
    pub deep: Option<u64>,
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> crate::error::AppResult<Self> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(|e| {
            crate::error::AppError::Config(format!(
                "Failed to read config file {}: {}",
                path.as_ref().display(),
                e
            ))
        })?;
        let config = Self::from_str(&content).map_err(|e| {
            crate::error::AppError::Config(format!(
                "Failed to parse config file {}: {}",
                path.as_ref().display(),
                e
            ))
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Get timeout for a specific model tier
    ///
    /// Returns the per-tier timeout if configured, otherwise falls back to
    /// the global `server.request_timeout_seconds`.
    pub fn timeout_for_tier(&self, tier: crate::router::TargetModel) -> u64 {
        let tier_timeout = match tier {
            crate::router::TargetModel::Fast => self.timeouts.fast,
            crate::router::TargetModel::Balanced => self.timeouts.balanced,
            crate::router::TargetModel::Deep => self.timeouts.deep,
        };
        tier_timeout.unwrap_or(self.server.request_timeout_seconds)
    }

    /// Validate configuration after parsing
    fn validate(&self) -> crate::error::AppResult<()> {
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

                // Validate max_tokens: must not exceed u32::MAX (required for open-agent-sdk)
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

        // Validate that each model tier has at least one endpoint
        if self.models.fast.is_empty() {
            return Err(crate::error::AppError::Config(
                "Configuration error: models.fast must contain at least one model endpoint"
                    .to_string(),
            ));
        }
        if self.models.balanced.is_empty() {
            return Err(crate::error::AppError::Config(
                "Configuration error: models.balanced must contain at least one model endpoint"
                    .to_string(),
            ));
        }
        if self.models.deep.is_empty() {
            return Err(crate::error::AppError::Config(
                "Configuration error: models.deep must contain at least one model endpoint"
                    .to_string(),
            ));
        }

        // Validate router_model is valid
        if !["fast", "balanced", "deep"].contains(&self.routing.router_model.as_str()) {
            return Err(crate::error::AppError::Config(format!(
                "Configuration error: routing.router_model must be 'fast', 'balanced', or 'deep', got '{}'",
                self.routing.router_model
            )));
        }

        // Validate port conflict
        if self.observability.metrics_enabled && self.observability.metrics_port == self.server.port
        {
            return Err(crate::error::AppError::Config(format!(
                "Configuration error: metrics_port ({}) cannot be the same as server port ({})",
                self.observability.metrics_port, self.server.port
            )));
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

        // Validate per-tier timeout overrides
        for (tier_name, timeout_opt) in [
            ("fast", self.timeouts.fast),
            ("balanced", self.timeouts.balanced),
            ("deep", self.timeouts.deep),
        ] {
            if let Some(timeout) = timeout_opt {
                if timeout == 0 {
                    return Err(crate::error::AppError::Config(format!(
                        "Configuration error: timeouts.{} must be greater than 0, got {}",
                        tier_name, timeout
                    )));
                }
                if timeout > 300 {
                    return Err(crate::error::AppError::Config(format!(
                        "Configuration error: timeouts.{} cannot exceed 300 seconds (5 minutes), got {}",
                        tier_name, timeout
                    )));
                }
            }
        }

        Ok(())
    }
}

impl FromStr for Config {
    type Err = crate::error::AppError;

    fn from_str(toml_str: &str) -> Result<Self, Self::Err> {
        toml::from_str(toml_str)
            .map_err(|e| crate::error::AppError::Config(format!("Invalid TOML: {}", e)))
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
router_model = "balanced"

[observability]
log_level = "info"
metrics_enabled = false
metrics_port = 9090
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
        assert_eq!(config.routing.router_model, "balanced");
    }

    #[test]
    fn test_config_parses_observability() {
        let config = Config::from_str(TEST_CONFIG).expect("should parse config");
        assert_eq!(config.observability.log_level, "info");
        assert!(!config.observability.metrics_enabled);
        assert_eq!(config.observability.metrics_port, 9090);
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
router_model = "balanced"
"#;

        let config = Config::from_str(minimal_config).expect("should parse minimal config");
        assert_eq!(config.observability.log_level, "info");
        assert!(!config.observability.metrics_enabled);
        assert_eq!(config.observability.metrics_port, 9090);

        // Verify defaults for weight and priority
        assert_eq!(config.models.fast[0].weight, 1.0);
        assert_eq!(config.models.fast[0].priority, 1);
    }

    #[test]
    fn test_config_validation_empty_fast_tier_fails() {
        // Create a config with empty fast tier programmatically
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.models.fast.clear(); // Empty the fast tier

        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("models.fast"));
    }

    #[test]
    fn test_config_validation_invalid_router_model_fails() {
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
router_model = "invalid"
"#;

        let config = Config::from_str(config_str).unwrap();
        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("router_model"));
        assert!(err_msg.contains("invalid"));
    }

    #[test]
    fn test_config_validation_port_conflict_fails() {
        let config_str = r#"
[server]
host = "127.0.0.1"
port = 3000

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
router_model = "balanced"

[observability]
metrics_enabled = true
metrics_port = 3000
"#;

        let config = Config::from_str(config_str).unwrap();
        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("metrics_port"));
        assert!(err_msg.contains("3000"));
    }

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
router_model = "balanced"

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
router_model = "balanced"

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

    #[test]
    fn test_config_validation_per_tier_timeout_too_low_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.timeouts.fast = Some(0); // Invalid: zero timeout

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("fast") && err_msg.contains("timeout"));
    }

    #[test]
    fn test_config_validation_per_tier_timeout_too_high_fails() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.timeouts.deep = Some(301); // Invalid: exceeds 300 second limit

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("deep") && err_msg.contains("300"));
    }

    #[test]
    fn test_config_validation_per_tier_timeouts_valid_succeeds() {
        let mut config = Config::from_str(TEST_CONFIG).unwrap();
        config.timeouts.fast = Some(15);
        config.timeouts.balanced = Some(30);
        config.timeouts.deep = Some(60);

        assert!(config.validate().is_ok());
    }

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
}
