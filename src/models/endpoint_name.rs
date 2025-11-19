//! Type-safe endpoint name wrapper
//!
//! Provides validation and type safety for endpoint names used throughout
//! the routing system, particularly in exclusion sets for retry logic.

use crate::config::{Config, ModelEndpoint};
use std::collections::HashSet;

/// Type-safe wrapper for endpoint names
///
/// Prevents typos and wrong-tier endpoint names in exclusion sets.
///
/// # Preferred Usage
/// - **Production code with Config**: Use `EndpointName::new(name, config)` for validated construction
/// - **Production code with ModelEndpoint**: Use `EndpointName::from(endpoint)` (always valid)
/// - **Test code**: Use string conversions `From<String>` or `From<&str>`, but be aware they don't validate
///
/// # Validation
/// - `new()`: Validates at construction time, returns `Result`
/// - `From<&ModelEndpoint>`: Always valid (endpoint comes from config)
/// - `From<String>` and `From<&str>`: No validation - invalid names cause runtime errors
///   (`HealthError::UnknownEndpoint`) when used with health checking methods
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct EndpointName(String);

impl EndpointName {
    /// Create a validated EndpointName from a string
    ///
    /// Validates that the endpoint name exists in the configuration.
    /// Use this in production code to catch invalid endpoint names early.
    ///
    /// # Errors
    /// Returns an error if the endpoint name doesn't match any configured endpoint.
    pub fn new(name: String, config: &Config) -> Result<Self, String> {
        let endpoint_name = Self(name.clone());
        if endpoint_name.is_valid(config) {
            Ok(endpoint_name)
        } else {
            Err(format!(
                "Unknown endpoint: '{}'. Available endpoints: {}",
                name,
                Self::list_available(config).join(", ")
            ))
        }
    }

    /// Get the inner string value
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validate that this endpoint name exists in the given configuration
    ///
    /// Returns true if this endpoint name matches any configured endpoint
    /// across all tiers (fast, balanced, deep).
    pub fn is_valid(&self, config: &Config) -> bool {
        config.models.fast.iter().any(|e| e.name() == self.0)
            || config.models.balanced.iter().any(|e| e.name() == self.0)
            || config.models.deep.iter().any(|e| e.name() == self.0)
    }

    /// List all available endpoint names in the configuration
    fn list_available(config: &Config) -> Vec<String> {
        let mut names = Vec::new();
        names.extend(config.models.fast.iter().map(|e| e.name().to_string()));
        names.extend(config.models.balanced.iter().map(|e| e.name().to_string()));
        names.extend(config.models.deep.iter().map(|e| e.name().to_string()));
        names
    }
}

impl From<&ModelEndpoint> for EndpointName {
    /// Create an EndpointName from a ModelEndpoint reference (always valid)
    fn from(endpoint: &ModelEndpoint) -> Self {
        Self(endpoint.name().to_string())
    }
}

impl From<String> for EndpointName {
    /// Create an EndpointName from a String
    ///
    /// Note: This does NOT validate that the endpoint exists in the configuration.
    /// Prefer `EndpointName::from(&endpoint)` in production code.
    fn from(name: String) -> Self {
        Self(name)
    }
}

impl From<&str> for EndpointName {
    /// Create an EndpointName from a string slice
    ///
    /// Note: This does NOT validate that the endpoint exists in the configuration.
    /// Prefer `EndpointName::from(&endpoint)` in production code.
    fn from(name: &str) -> Self {
        Self(name.to_string())
    }
}

/// Type alias for exclusion sets used in retry logic
pub type ExclusionSet = HashSet<EndpointName>;
