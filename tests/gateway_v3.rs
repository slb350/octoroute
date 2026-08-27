//! Public end-to-end contracts for the Octoroute v3 HTTP application.

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use octoroute::gateway::{
    env::Environment,
    fabric::{FabricConfig, FabricGatewayService, fabric_gateway_app},
};
use reqwest::Url;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

#[derive(Clone)]
struct TestEnvironment {
    values: BTreeMap<String, String>,
    reads: Arc<Mutex<Vec<String>>>,
}

impl TestEnvironment {
    fn gateway() -> Self {
        Self {
            values: BTreeMap::from([(
                "OCTOROUTE_API_KEY".to_string(),
                "inbound-secret".to_string(),
            )]),
            reads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn reads(&self) -> Vec<String> {
        self.reads.lock().expect("reads mutex").clone()
    }
}

impl Environment for TestEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.reads
            .lock()
            .expect("reads mutex")
            .push(name.to_string());
        self.values.get(name).cloned()
    }
}

fn config(server: &MockServer) -> FabricConfig {
    let mut config =
        FabricConfig::from_toml(include_str!("../config.toml")).expect("repository config");
    let workers = config.local_pools.get_mut("workers").expect("workers pool");
    workers.members.truncate(1);
    workers.members[0].base_url = Url::parse(&server.uri()).expect("mock URL");
    config
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
async fn local_pool_sse_is_forwarded_opaquely_with_unknown_request_fields() {
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
            "model": "coding-worker-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true,
            "future_field": {"preserved": true}
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_bytes(
                    b": keepalive\n\ndata: {\"model\":\"coding-worker-model\"}\n\ndata: [DONE]\n\n",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let environment = TestEnvironment::gateway();
    let app = fabric_gateway_app(
        FabricGatewayService::from_config(config(&server), environment).expect("service"),
    );

    let response = app
        .oneshot(chat_request("worker", true))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "local");
    assert_eq!(response.headers()["x-octoroute-reason"], "local_pool");
    assert_eq!(response.headers()["x-octoroute-route"], "worker");
    assert_eq!(response.headers()["x-octoroute-target"], "pool:workers");
    assert_eq!(
        to_bytes(response.into_body(), 4096)
            .await
            .expect("SSE body"),
        ": keepalive\n\ndata: {\"model\":\"coding-worker-model\"}\n\ndata: [DONE]\n\n"
    );
}

#[tokio::test]
async fn local_only_busy_response_never_resolves_provider_credentials() {
    let server = MockServer::start().await;
    mount_admission(
        &server,
        ResponseTemplate::new(200).set_body_json(json!([{"is_processing": true}])),
    )
    .await;
    let environment = TestEnvironment::gateway();
    let reads = environment.clone();
    let app = fabric_gateway_app(
        FabricGatewayService::from_config(config(&server), environment).expect("service"),
    );

    let response = app
        .oneshot(chat_request("auto", false))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reads.reads(), vec!["OCTOROUTE_API_KEY"]);
    let body: serde_json::Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 4096)
            .await
            .expect("error body"),
    )
    .expect("JSON error");
    assert_eq!(body["error"]["code"], "local_pool_disabled");
}
