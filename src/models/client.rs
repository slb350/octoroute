//! Model client wrapper around open-agent-sdk
//!
//! Provides a higher-level interface for interacting with LLM endpoints
//! configured via ModelEndpoint.

use crate::config::ModelEndpoint;
use crate::error::{AppError, AppResult};
use open_agent::{AgentOptions, Client};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Wrapper around open_agent::Client with endpoint configuration
///
/// Client is wrapped in Arc<Mutex<>> to make it Send + Sync for use in async handlers
pub struct ModelClient {
    endpoint: ModelEndpoint,
    client: Arc<Mutex<Client>>,
}

impl ModelClient {
    /// Create a new ModelClient from a ModelEndpoint configuration
    pub fn new(endpoint: ModelEndpoint) -> AppResult<Self> {
        // Build AgentOptions from ModelEndpoint
        let options = AgentOptions::builder()
            .model(&endpoint.name)
            .base_url(&endpoint.base_url)
            .max_tokens(endpoint.max_tokens as u32)
            .temperature(endpoint.temperature as f32)
            .build()
            .map_err(|e| AppError::Internal(format!("Failed to build AgentOptions: {}", e)))?;

        // Create the Client
        let client = Client::new(options).map_err(|e| {
            AppError::Internal(format!(
                "Failed to create client for {}: {}",
                endpoint.name, e
            ))
        })?;

        Ok(Self {
            endpoint,
            client: Arc::new(Mutex::new(client)),
        })
    }

    /// Get reference to the underlying endpoint configuration
    pub fn endpoint(&self) -> &ModelEndpoint {
        &self.endpoint
    }

    /// Get arc-mutex wrapped client for thread-safe access
    pub fn client(&self) -> &Arc<Mutex<Client>> {
        &self.client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_endpoint() -> ModelEndpoint {
        ModelEndpoint {
            name: "test-model".to_string(),
            base_url: "http://localhost:1234/v1".to_string(),
            max_tokens: 2048,
            temperature: 0.7,
            weight: 1.0,
            priority: 1,
        }
    }

    #[test]
    fn test_model_client_new_creates_client() {
        let endpoint = create_test_endpoint();
        let result = ModelClient::new(endpoint.clone());

        assert!(result.is_ok(), "ModelClient::new should succeed");
        let client = result.unwrap();
        assert_eq!(client.endpoint().name, "test-model");
        assert_eq!(client.endpoint().base_url, "http://localhost:1234/v1");
    }

    #[test]
    fn test_model_client_stores_endpoint_config() {
        let endpoint = create_test_endpoint();
        let client = ModelClient::new(endpoint.clone()).expect("client creation should succeed");

        assert_eq!(client.endpoint().max_tokens, 2048);
        assert_eq!(client.endpoint().temperature, 0.7);
        assert_eq!(client.endpoint().weight, 1.0);
        assert_eq!(client.endpoint().priority, 1);
    }

    #[test]
    fn test_model_client_has_underlying_client() {
        let endpoint = create_test_endpoint();
        let client = ModelClient::new(endpoint).expect("client creation should succeed");

        // Just verify we can access the underlying client
        let _ = client.client();
    }
}
