//! Public contract for the generated v2 configuration.

use octoroute::{
    cli::generate_config_template,
    gateway::config::{Environment, GatewayConfig},
};
use std::fs;
use tempfile::TempDir;

struct TemplateEnvironment;

impl Environment for TemplateEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        match name {
            "OCTOROUTE_API_KEY" | "OPENROUTER_API_KEY" => Some("test-secret".to_string()),
            _ => None,
        }
    }
}

#[test]
fn generated_template_roundtrips_as_valid_v2_configuration() {
    let directory = TempDir::new().expect("temporary directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, generate_config_template()).expect("write template");
    let input = fs::read_to_string(&path).expect("read template");

    let config =
        GatewayConfig::from_toml(&input, &TemplateEnvironment).expect("valid v2 configuration");

    assert_eq!(config.version(), 2);
    assert_eq!(config.server().port(), 8081);
    assert_eq!(config.local().model(), "strixtea");
    assert_eq!(config.local().base_url().as_str(), "http://127.0.0.1:8080/");
    assert_eq!(config.openrouter().auto_model(), "openrouter/auto");
}

#[test]
fn generated_template_documents_env_secrets_without_raw_values() {
    let template = generate_config_template();

    assert!(template.contains("api_key_env = \"OCTOROUTE_API_KEY\""));
    assert!(template.contains("api_key_env = \"OPENROUTER_API_KEY\""));
    assert!(!template.contains("sk-or-"));
    assert!(template.contains("[upstreams.local]"));
    assert!(template.contains("[upstreams.openrouter]"));
}
