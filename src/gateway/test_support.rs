use super::{
    config::{Environment, GatewayConfig},
    request::GatewayRequest,
};
use serde_json::{Value, json};
use std::collections::HashMap;

const TEST_MODEL_REVISION: &str = "test-model-revision";

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

pub(super) fn trajectory_tool_call(id: &str) -> Value {
    json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": id,
            "type": "function",
            "function": {"name": "run", "arguments": "{}"}
        }]
    })
}

pub(super) fn trajectory_tool_result(id: &str, trajectory: Value) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": id,
        "content": serde_json::to_string(&json!({
            "type": "octoroute.trajectory/v1",
            "trajectory": trajectory,
            "result": {"bounded": true}
        }))
        .expect("serialize typed tool result")
    })
}

pub(super) fn gateway_config(
    local_base_url: &str,
    extra_local: &str,
    extra_openrouter: &str,
    extra_routing: &str,
) -> GatewayConfig {
    gateway_config_complete(
        local_base_url,
        "",
        r#"["chat", "stream"]"#,
        TEST_MODEL_REVISION,
        extra_local,
        extra_openrouter,
        extra_routing,
    )
}

pub(super) fn gateway_config_with_local_capabilities(
    local_base_url: &str,
    capabilities: &str,
    extra_openrouter: &str,
    extra_routing: &str,
) -> GatewayConfig {
    gateway_config_complete(
        local_base_url,
        "",
        capabilities,
        TEST_MODEL_REVISION,
        "",
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
    gateway_config_complete(
        local_base_url,
        extra_server,
        r#"["chat", "stream"]"#,
        TEST_MODEL_REVISION,
        extra_local,
        extra_openrouter,
        extra_routing,
    )
}

pub(super) fn gateway_config_with_model_revision(
    local_base_url: &str,
    model_revision: &str,
) -> GatewayConfig {
    gateway_config_complete(
        local_base_url,
        "",
        r#"["chat", "stream"]"#,
        model_revision,
        "",
        "",
        "",
    )
}

fn gateway_config_complete(
    local_base_url: &str,
    extra_server: &str,
    capabilities: &str,
    model_revision: &str,
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
name = "local"
base_url = "{local_base_url}"
model = "example-local-model"
model_revision = "{model_revision}"
context_window = 65536
context_safety_tokens = 1024
default_max_output_tokens = 4096
max_in_flight = 1
capabilities = {capabilities}
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
