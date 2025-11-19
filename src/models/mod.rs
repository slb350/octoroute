//! Model client and selection logic
//!
//! Provides wrappers around `open-agent-sdk` clients and intelligent
//! model selection from multi-model configuration.

pub mod client;
pub mod endpoint_name;
pub mod health;
pub mod selector;

pub use client::ModelClient;
pub use endpoint_name::{EndpointName, ExclusionSet};
pub use health::{EndpointHealth, HealthChecker, HealthError};
pub use selector::ModelSelector;
