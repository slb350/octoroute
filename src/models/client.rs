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
/// **NOTE**: Unused through Phase 2c. The `/chat` handler uses the standalone
/// `open_agent::query()` function to avoid `!Sync` issues with the stateful Client.
///
/// **Retention Rationale**: Required for Phase 3 tool-based routing (see CLAUDE.md § 2.2.3).
/// The tool-based router architecture requires stateful Client instances for each target
/// model endpoint. Tools like `call_fast_8b`, `call_balanced_30b`, and `call_deep_120b`
/// each execute a separate Client to enable multi-turn conversations and streaming context
/// management within tool invocations.
///
/// If Phase 3 adopts a different architecture that doesn't require stateful clients,
/// this type should be removed in favor of the standalone `open_agent::query()` pattern.
///
/// Client is wrapped in Arc<Mutex<>> to make it Send + Sync for use in async handlers.
#[allow(dead_code)]
pub struct ModelClient {
    endpoint: ModelEndpoint,
    client: Arc<Mutex<Client>>,
}

impl ModelClient {
    /// Create a new ModelClient from a ModelEndpoint configuration
    #[allow(dead_code)]
    pub fn new(endpoint: ModelEndpoint) -> AppResult<Self> {
        // Build AgentOptions from ModelEndpoint
        let options = AgentOptions::builder()
            .model(endpoint.name())
            .base_url(endpoint.base_url())
            .max_tokens(endpoint.max_tokens() as u32)
            .temperature(endpoint.temperature() as f32)
            .build()
            .map_err(|e| AppError::Internal(format!("Failed to build AgentOptions: {}", e)))?;

        // Create the Client
        let client = Client::new(options).map_err(|e| {
            AppError::Internal(format!(
                "Failed to create client for {}: {}",
                endpoint.name(),
                e
            ))
        })?;

        Ok(Self {
            endpoint,
            client: Arc::new(Mutex::new(client)),
        })
    }

    /// Get reference to the underlying endpoint configuration
    #[allow(dead_code)]
    pub fn endpoint(&self) -> &ModelEndpoint {
        &self.endpoint
    }

    /// Get arc-mutex wrapped client for thread-safe access
    #[allow(dead_code)]
    pub fn client(&self) -> &Arc<Mutex<Client>> {
        &self.client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: ModelClient tests have been removed because they require instantiating
    // a real open_agent::Client, which fails in test environments (macOS/CI) due to
    // SystemConfiguration API access. The ModelClient is a thin wrapper around
    // open-agent-sdk - the SDK's own tests verify Client creation works.
    //
    // Integration with actual models is tested via the /chat endpoint integration tests.

    #[test]
    fn test_model_endpoint_deserialization() {
        // ModelEndpoint fields are private and can only be created via deserialization.
        // This test verifies that deserialized instances have proper getter access.
        let json = r#"{
            "name": "test-model",
            "base_url": "http://localhost:1234/v1",
            "max_tokens": 2048,
            "temperature": 0.7,
            "weight": 1.0,
            "priority": 1
        }"#;

        let endpoint: ModelEndpoint =
            serde_json::from_str(json).expect("should deserialize ModelEndpoint");

        // Verify getters work
        assert_eq!(endpoint.name(), "test-model");
        assert_eq!(endpoint.base_url(), "http://localhost:1234/v1");
        assert_eq!(endpoint.max_tokens(), 2048);
        assert_eq!(endpoint.temperature(), 0.7);
        assert_eq!(endpoint.weight(), 1.0);
        assert_eq!(endpoint.priority(), 1);
    }
}
