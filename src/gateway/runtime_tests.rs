use super::runtime::{RuntimeConfig, RuntimeConfigError};
use crate::gateway::config::Environment;
use std::collections::BTreeMap;

#[derive(Debug, Default)]
struct MapEnvironment(BTreeMap<String, String>);

impl MapEnvironment {
    fn with(mut self, name: &str, value: &str) -> Self {
        self.0.insert(name.to_string(), value.to_string());
        self
    }
}

impl Environment for MapEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

#[test]
fn runtime_loader_preserves_v2_secret_resolution() {
    let environment = MapEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("OPENROUTER_API_KEY", "cloud-test-key");
    let config =
        RuntimeConfig::from_toml(include_str!("../../config.toml"), &environment).expect("v2");
    assert_eq!(config.version(), 2);
    assert!(matches!(config, RuntimeConfig::V2(_)));
}

#[test]
fn runtime_loader_accepts_v3_without_resolving_provider_credentials() {
    let config = RuntimeConfig::from_toml(
        include_str!("../../config.v3.toml"),
        &MapEnvironment::default(),
    )
    .expect("v3");
    assert_eq!(config.version(), 3);
    assert!(matches!(config, RuntimeConfig::V3(_)));
}

#[test]
fn runtime_loader_rejects_unknown_versions_before_schema_guessing() {
    let input = include_str!("../../config.v3.toml").replacen(
        "config_version = 3",
        "config_version = 99",
        1,
    );
    assert!(matches!(
        RuntimeConfig::from_toml(&input, &MapEnvironment::default()),
        Err(RuntimeConfigError::UnsupportedVersion { version: 99 })
    ));
}

#[test]
fn runtime_parse_errors_do_not_echo_configuration_values() {
    let input = "config_version = 3\nsecret = \"do-not-echo\"\n[server\n";
    let error = RuntimeConfig::from_toml(input, &MapEnvironment::default())
        .expect_err("malformed TOML must fail");
    assert!(!error.to_string().contains("do-not-echo"));
}
