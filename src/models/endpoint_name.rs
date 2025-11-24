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
/// # Usage Patterns
///
/// **Production code** (validation enforced):
/// - `EndpointName::new(name, config)` - Validated construction, returns `Result`
/// - `EndpointName::from(&endpoint)` - Always valid (endpoint comes from config)
///
/// **Test code only** (no validation):
/// - `EndpointName::from("test-endpoint")` - Test-only unvalidated construction
/// - `EndpointName::from(String::from("test"))` - Test-only unvalidated construction
///
/// # Type Safety Guarantees
///
/// - `new()`: Validates at construction time, returns `Result`
/// - `From<&ModelEndpoint>`: Always valid (endpoint comes from config)
/// - `From<String>` and `From<&str>`: **Test-only** (`#[cfg(test)]`) to prevent
///   production code from bypassing validation
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

/// Test-only unvalidated conversions
///
/// These conversions are intentionally restricted to test code to prevent
/// production code from bypassing validation. In tests, we often need to
/// create EndpointNames with arbitrary values for testing error paths.
///
/// **Production code should use**:
/// - `EndpointName::from(&endpoint)` for validated ModelEndpoint references
/// - `EndpointName::new(name, config)` for validated string construction
#[cfg(test)]
impl From<String> for EndpointName {
    /// Create an EndpointName from a String (test-only, no validation)
    fn from(name: String) -> Self {
        Self(name)
    }
}

#[cfg(test)]
impl From<&str> for EndpointName {
    /// Create an EndpointName from a string slice (test-only, no validation)
    fn from(name: &str) -> Self {
        Self(name.to_string())
    }
}

/// Type alias for exclusion sets used in retry logic
///
/// **IMPORTANT**: Exclusions are request-scoped, NOT global. An ExclusionSet
/// exists only for the duration of a single request (function call) and is
/// discarded when the function returns. Endpoints excluded during one request's
/// retries are available again for the next request.
///
/// This prevents retry loops from hitting the same failed endpoint repeatedly
/// within a single request, while allowing the health checker to independently
/// track endpoint health and recover failed endpoints across requests.
pub type ExclusionSet = HashSet<EndpointName>;
