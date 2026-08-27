//! Public contract for the generated v3 configuration.

use octoroute::{cli::generate_config_template, gateway::fabric::FabricConfig};
use std::fs;
use tempfile::TempDir;

#[test]
fn generated_template_roundtrips_as_valid_v3_configuration() {
    let directory = TempDir::new().expect("temporary directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, generate_config_template()).expect("write template");
    let input = fs::read_to_string(&path).expect("read template");

    let config = FabricConfig::from_toml(&input).expect("valid v3 configuration");

    assert_eq!(config.server.port, 8081);
    assert_eq!(config.default_model, "auto-route");
    assert_eq!(config.local_pools["workers"].model, "coding-worker-model");
    assert_eq!(
        config.local_pools["workers"].model_revision,
        "example-worker-revision"
    );
    assert_eq!(config.local_pools["workers"].members.len(), 3);
    assert_eq!(config.providers["openrouter"].model, "openrouter/auto");
}

#[test]
fn generated_template_documents_env_secrets_without_raw_values() {
    let template = generate_config_template();

    assert!(template.contains("api_key_env = \"OCTOROUTE_API_KEY\""));
    assert!(template.contains("api_key_env = \"OPENROUTER_API_KEY\""));
    assert!(!template.contains("sk-or-"));
    assert!(template.contains("[[fabric.local_pools]]"));
    assert!(template.contains("[[fabric.providers]]"));
}
