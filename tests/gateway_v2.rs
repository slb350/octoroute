//! Public end-to-end contracts for the Octoroute v2 HTTP application.

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use octoroute::gateway::{
    config::{Environment, GatewayConfig},
    http::gateway_app,
    service::GatewayService,
};
use serde_json::json;
use std::collections::HashMap;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

struct TestEnvironment {
    values: HashMap<String, String>,
}

impl Environment for TestEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.values.get(name).cloned()
    }
}

fn config(server: &MockServer) -> GatewayConfig {
    let input = format!(
        r#"
config_version = 2

[server]
host = "127.0.0.1"
port = 3000
api_key_env = "OCTOROUTE_API_KEY"

[upstreams.local]
kind = "llama_cpp"
name = "local"
base_url = "{}"
model = "example-local-model"
model_revision = "test-model-revision"
context_window = 65536
context_safety_tokens = 1024
max_in_flight = 1
capabilities = ["chat", "stream"]
health_path = "/health"
slots_path = "/slots?fail_on_no_slot=1"
input_tokens_path = "/v1/chat/completions/input_tokens"

[upstreams.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

[routing]
default = "prefer_local"
"#,
        server.uri()
    );
    let environment = TestEnvironment {
        values: HashMap::from([
            (
                "OCTOROUTE_API_KEY".to_string(),
                "inbound-secret".to_string(),
            ),
            (
                "OPENROUTER_API_KEY".to_string(),
                "openrouter-secret".to_string(),
            ),
        ]),
    };
    GatewayConfig::from_toml(&input, &environment).expect("valid integration config")
}

async fn mount_admission(server: &MockServer, slots: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .and(query_param("fail_on_no_slot", "1"))
        .respond_with(slots)
        .expect(1)
        .mount(server)
        .await;
}

fn chat_request(model: &str, stream: bool) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(AUTHORIZATION, "Bearer inbound-secret")
        .header("x-octoroute-privacy", "local-only")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "model": model,
                "messages": [{"role": "user", "content": "hello"}],
                "stream": stream,
                "future_field": {"preserved": true}
            }))
            .expect("serialize request"),
        ))
        .expect("request")
}

#[tokio::test]
async fn explicit_local_sse_is_forwarded_opaquely_with_unknown_request_fields() {
    let server = MockServer::start().await;
    mount_admission(
        &server,
        ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 12})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({
            "model": "example-local-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true,
            "future_field": {"preserved": true}
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_bytes(
                    b": keepalive\n\ndata: {\"model\":\"example-local-model\"}\n\ndata: [DONE]\n\n",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let app = gateway_app(GatewayService::from_config(config(&server)).expect("service"));

    let response = app
        .oneshot(chat_request("local", true))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "local");
    assert_eq!(response.headers()["x-octoroute-reason"], "local_only");
    assert_eq!(
        to_bytes(response.into_body(), 4096)
            .await
            .expect("SSE body"),
        ": keepalive\n\ndata: {\"model\":\"example-local-model\"}\n\ndata: [DONE]\n\n"
    );
}

#[tokio::test]
async fn local_only_busy_response_never_contacts_openrouter() {
    let server = MockServer::start().await;
    mount_admission(&server, ResponseTemplate::new(503)).await;
    let app = gateway_app(GatewayService::from_config(config(&server)).expect("service"));

    let response = app
        .oneshot(chat_request("auto", false))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 4096)
            .await
            .expect("error body"),
    )
    .expect("JSON error");
    assert_eq!(body["error"]["code"], "routing_error");
}
