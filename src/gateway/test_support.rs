use super::{
    config::{Environment, GatewayConfig},
    request::GatewayRequest,
};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub(super) struct TestEnvironment {
    values: HashMap<String, String>,
}

impl TestEnvironment {
    pub(super) fn with(mut self, name: &str, value: &str) -> Self {
        self.values.insert(name.to_string(), value.to_string());
        self
    }

    pub(super) fn gateway() -> Self {
        Self {
            values: HashMap::from([
                (
                    "OCTOROUTE_API_KEY".to_string(),
                    "inbound-secret".to_string(),
                ),
                (
                    "OPENROUTER_API_KEY".to_string(),
                    "openrouter-secret".to_string(),
                ),
                ("LOCAL_API_KEY".to_string(), "local-secret".to_string()),
            ]),
        }
    }
}

impl Environment for TestEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.values.get(name).cloned()
    }
}

pub(super) fn gateway_request(body: Value) -> GatewayRequest {
    GatewayRequest::parse(&serde_json::to_vec(&body).expect("serialize request fixture"))
        .expect("valid request fixture")
}

pub(super) fn gateway_config(
    local_base_url: &str,
    extra_local: &str,
    extra_openrouter: &str,
    extra_routing: &str,
) -> GatewayConfig {
    gateway_config_with_server(
        local_base_url,
        "",
        extra_local,
        extra_openrouter,
        extra_routing,
    )
}

pub(super) fn gateway_config_with_server(
    local_base_url: &str,
    extra_server: &str,
    extra_local: &str,
    extra_openrouter: &str,
    extra_routing: &str,
) -> GatewayConfig {
    let input = format!(
        r#"
config_version = 2

[server]
host = "127.0.0.1"
port = 3000
api_key_env = "OCTOROUTE_API_KEY"
{extra_server}

[upstreams.local]
kind = "llama_cpp"
name = "strix"
base_url = "{local_base_url}"
model = "puzzle-75b"
context_window = 65536
context_safety_tokens = 1024
default_max_output_tokens = 4096
max_in_flight = 1
capabilities = ["chat", "stream"]
health_path = "/health"
slots_path = "/slots?fail_on_no_slot=1"
input_tokens_path = "/v1/chat/completions/input_tokens"
{extra_local}

[upstreams.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
{extra_openrouter}

[routing]
default = "prefer_local"
{extra_routing}
"#
    );
    GatewayConfig::from_toml(&input, &TestEnvironment::gateway())
        .expect("valid shared gateway fixture")
}
