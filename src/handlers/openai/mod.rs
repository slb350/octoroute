//! OpenAI-compatible API handlers
//!
//! Provides OpenAI-compatible endpoints for Octoroute:
//! - `POST /v1/chat/completions` - Chat completions with SSE streaming
//! - `GET /v1/models` - List available models

use crate::config::{Config, ModelEndpoint};
use crate::error::AppError;
use crate::router::TargetModel;

pub mod completions;
pub mod models;
pub mod streaming;
pub mod types;

/// Find an endpoint by name across all tiers
///
/// Searches through fast, balanced, and deep tiers to find an endpoint
/// with the specified name. Returns the tier and endpoint if found.
///
/// # Arguments
/// * `config` - The application configuration containing model endpoints
/// * `name` - The endpoint name to search for
///
/// # Returns
/// * `Ok((TargetModel, ModelEndpoint))` - The tier and endpoint if found
/// * `Err(AppError::Validation)` - If no endpoint with the name exists
pub(crate) fn find_endpoint_by_name(
    config: &Config,
    name: &str,
) -> Result<(TargetModel, ModelEndpoint), AppError> {
    // Search fast tier
    for endpoint in &config.models.fast {
        if endpoint.name() == name {
            return Ok((TargetModel::Fast, endpoint.clone()));
        }
    }

    // Search balanced tier
    for endpoint in &config.models.balanced {
        if endpoint.name() == name {
            return Ok((TargetModel::Balanced, endpoint.clone()));
        }
    }

    // Search deep tier
    for endpoint in &config.models.deep {
        if endpoint.name() == name {
            return Ok((TargetModel::Deep, endpoint.clone()));
        }
    }

    Err(AppError::Validation(format!(
        "Model '{}' not found. Available models: auto, fast, balanced, deep, or a specific endpoint name from config.",
        name
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config() -> Config {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "fast-model"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-model"
base_url = "http://localhost:1235/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-model"
base_url = "http://localhost:1236/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
"#;
        toml::from_str(toml).expect("should parse test config")
    }

    #[test]
    fn test_find_endpoint_by_name_fast() {
        let config = create_test_config();
        let (tier, endpoint) = find_endpoint_by_name(&config, "fast-model").unwrap();
        assert_eq!(tier, TargetModel::Fast);
        assert_eq!(endpoint.name(), "fast-model");
    }

    #[test]
    fn test_find_endpoint_by_name_balanced() {
        let config = create_test_config();
        let (tier, endpoint) = find_endpoint_by_name(&config, "balanced-model").unwrap();
        assert_eq!(tier, TargetModel::Balanced);
        assert_eq!(endpoint.name(), "balanced-model");
    }

    #[test]
    fn test_find_endpoint_by_name_deep() {
        let config = create_test_config();
        let (tier, endpoint) = find_endpoint_by_name(&config, "deep-model").unwrap();
        assert_eq!(tier, TargetModel::Deep);
        assert_eq!(endpoint.name(), "deep-model");
    }

    #[test]
    fn test_find_endpoint_by_name_not_found() {
        let config = create_test_config();
        let result = find_endpoint_by_name(&config, "nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
