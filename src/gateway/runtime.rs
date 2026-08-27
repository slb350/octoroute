//! Version-aware startup configuration for the v2 gateway and v3 inference fabric.

use crate::gateway::{
    config::{Environment, GatewayConfig, GatewayConfigError},
    fabric::{FabricConfig, FabricConfigError},
};
use thiserror::Error;

/// Validated runtime configuration selected from the declared schema version.
#[derive(Debug, Clone)]
pub enum RuntimeConfig {
    V2(Box<GatewayConfig>),
    V3(Box<FabricConfig>),
}

impl RuntimeConfig {
    /// Parse either a v2 or v3 configuration without weakening either schema.
    pub fn from_toml(
        input: &str,
        environment: &impl Environment,
    ) -> Result<Self, RuntimeConfigError> {
        let value: toml::Value =
            toml::from_str(input).map_err(|error| safe_parse_error(input, error))?;
        let Some(version) = value
            .get("config_version")
            .and_then(toml::Value::as_integer)
        else {
            return GatewayConfig::from_toml(input, environment)
                .map(|config| Self::V2(Box::new(config)))
                .map_err(RuntimeConfigError::V2);
        };

        match version {
            2 => GatewayConfig::from_toml(input, environment)
                .map(|config| Self::V2(Box::new(config)))
                .map_err(RuntimeConfigError::V2),
            3 => FabricConfig::from_toml(input)
                .map(|config| Self::V3(Box::new(config)))
                .map_err(RuntimeConfigError::V3),
            _ => Err(RuntimeConfigError::UnsupportedVersion { version }),
        }
    }

    /// Declared configuration schema version.
    pub fn version(&self) -> u8 {
        match self {
            Self::V2(_) => 2,
            Self::V3(_) => 3,
        }
    }
}

/// Safe startup failures across supported configuration versions.
#[derive(Debug, Error)]
pub enum RuntimeConfigError {
    #[error(transparent)]
    V2(#[from] GatewayConfigError),
    #[error(transparent)]
    V3(#[from] FabricConfigError),
    #[error("unsupported Octoroute configuration version {version}; expected 2 or 3")]
    UnsupportedVersion { version: i64 },
    #[error("invalid TOML at line {line}, column {column}; configuration values omitted")]
    Parse { line: usize, column: usize },
}

fn safe_parse_error(input: &str, error: toml::de::Error) -> RuntimeConfigError {
    let (line, column) = error
        .span()
        .map(|span| line_column(input, span.start))
        .unwrap_or((1, 1));
    RuntimeConfigError::Parse { line, column }
}

fn line_column(input: &str, byte_index: usize) -> (usize, usize) {
    let prefix = &input[..byte_index.min(input.len())];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.len() + 1, |(_, tail)| tail.len() + 1);
    (line, column)
}
