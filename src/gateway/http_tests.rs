use super::{
    http::gateway_app,
    local::LocalLease,
    openrouter::OpenRouterRequest,
    service::GatewayService,
    test_support::gateway_config,
    transport::{PreparedUpstreamResponse, UpstreamTransport},
};
use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{HeaderValue, Request, Response, StatusCode, header::AUTHORIZATION},
};
use serde_json::Value;
use std::fmt;
use tower::ServiceExt;
use wiremock::MockServer;

#[derive(Debug)]
struct StubError;

impl fmt::Display for StubError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("stub transport should not dispatch")
    }
}

impl std::error::Error for StubError {}

struct ReadyTransport;

#[async_trait]
impl UpstreamTransport for ReadyTransport {
    type Error = StubError;

    async fn local(&self, _lease: LocalLease) -> Result<PreparedUpstreamResponse, Self::Error> {
        Err(StubError)
    }

    async fn openrouter(
        &self,
        _request: OpenRouterRequest,
    ) -> Result<PreparedUpstreamResponse, Self::Error> {
        Err(StubError)
    }

    async fn openrouter_ready(&self) -> bool {
        true
    }
}

fn request(method: &str, uri: &str, body: Body, authenticated: bool) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if authenticated {
        builder = builder.header(AUTHORIZATION, "Bearer inbound-secret");
    }
    builder.body(body).expect("HTTP request")
}

async fn json(response: Response<Body>) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body"),
    )
    .expect("JSON")
}

#[tokio::test]
async fn metadata_endpoints_distinguish_oversized_headers_from_bad_authentication() {
    let local = MockServer::start().await;
    let config = super::test_support::gateway_config_with_server(
        &local.uri(),
        "max_header_bytes = 64",
        "",
        "",
        "",
    );
    let app = gateway_app(GatewayService::new(config, ReadyTransport).expect("service"));
    let mut request = request("GET", "/v1/models", Body::empty(), true);
    request.headers_mut().insert(
        "x-oversized",
        HeaderValue::from_str(&"x".repeat(100)).expect("large test header"),
    );

    let response = app.oneshot(request).await.expect("response");

    assert_eq!(
        response.status(),
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
    );
    assert_eq!(json(response).await["error"]["code"], "headers_too_large");
}

#[tokio::test]
async fn models_are_authenticated_and_advertise_virtual_and_exact_local_ids() {
    let local = MockServer::start().await;
    let config = gateway_config(&local.uri(), "", "", "");
    let app = gateway_app(GatewayService::new(config, ReadyTransport).expect("service"));

    let unauthorized = app
        .clone()
        .oneshot(request("GET", "/v1/models", Body::empty(), false))
        .await
        .expect("response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(request("GET", "/v1/models", Body::empty(), true))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let ids: Vec<String> = json(response).await["data"]
        .as_array()
        .expect("model data")
        .iter()
        .map(|model| model["id"].as_str().expect("model id").to_string())
        .collect();
    assert_eq!(ids, ["auto", "local", "cloud", "puzzle-75b"]);
}

#[tokio::test]
async fn liveness_and_readiness_have_distinct_semantics_and_health_alias() {
    let local = MockServer::start().await;
    let config = gateway_config(&local.uri(), "", "", "");
    let app = gateway_app(GatewayService::new(config, ReadyTransport).expect("service"));

    let live = app
        .clone()
        .oneshot(request("GET", "/health/live", Body::empty(), false))
        .await
        .expect("liveness");
    assert_eq!(live.status(), StatusCode::OK);

    for uri in ["/health/ready", "/health"] {
        let ready = app
            .clone()
            .oneshot(request("GET", uri, Body::empty(), false))
            .await
            .expect("readiness");
        assert_eq!(ready.status(), StatusCode::OK);
        assert_eq!(json(ready).await["openrouter"], "ready");
    }
}

#[tokio::test]
async fn all_http_responses_apply_api_security_headers_without_enabling_cors() {
    let local = MockServer::start().await;
    let config = gateway_config(&local.uri(), "", "", "");
    let app = gateway_app(GatewayService::new(config, ReadyTransport).expect("service"));

    let response = app
        .oneshot(request("GET", "/health/live", Body::empty(), false))
        .await
        .expect("liveness");

    assert!(response.headers().contains_key("x-request-id"));
    assert_eq!(
        response.headers()["x-octoroute-request-id"],
        response.headers()["x-request-id"]
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(
        response.headers()["permissions-policy"],
        "camera=(), microphone=(), geolocation=(), payment=()"
    );
    assert_eq!(
        response.headers()["content-security-policy"],
        "default-src 'none'; frame-ancestors 'none'"
    );
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}

#[tokio::test]
async fn chat_authentication_occurs_before_reading_or_validating_the_body() {
    let local = MockServer::start().await;
    let config = gateway_config(&local.uri(), "", "", "");
    let app = gateway_app(GatewayService::new(config, ReadyTransport).expect("service"));

    let response = app
        .oneshot(request(
            "POST",
            "/v1/chat/completions",
            Body::from("not-json"),
            false,
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json(response).await["error"]["code"],
        "authentication_error"
    );
}

#[tokio::test]
async fn chat_body_limit_returns_openai_error_envelope() {
    let local = MockServer::start().await;
    let config = gateway_config_with_small_body(&local);
    let app = gateway_app(GatewayService::new(config, ReadyTransport).expect("service"));

    let response = app
        .oneshot(request(
            "POST",
            "/v1/chat/completions",
            Body::from("x".repeat(65)),
            true,
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json(response).await["error"]["code"], "request_too_large");
}

fn gateway_config_with_small_body(server: &MockServer) -> super::config::GatewayConfig {
    use super::test_support::gateway_config_with_server;
    gateway_config_with_server(&server.uri(), "max_request_bytes = 64", "", "", "")
}
