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
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelEndpoint {
    pub name: String,
    pub base_url: String,
    pub max_tokens: usize,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    /// Load balancing weight (Phase 2b/2c feature)
    #[serde(default = "default_weight")]
    pub weight: f64,
    /// Priority level (higher = tried first, Phase 2b/2c feature)
    #[serde(default = "default_priority")]
    pub priority: u8,
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
}
