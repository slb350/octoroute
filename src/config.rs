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
    /// Load balancing weight (Phase 2 feature)
    #[serde(default = "default_weight")]
    pub weight: f64,
    /// Priority level (higher = tried first, Phase 2 feature)
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
    pub default_importance: String,
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
        Self::from_str(&content).map_err(|_| {
            crate::error::AppError::Config(format!(
                "Failed to parse config file {}",
                path.as_ref().display()
            ))
        })
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
        assert_eq!(config.routing.default_importance, "normal");
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
}
