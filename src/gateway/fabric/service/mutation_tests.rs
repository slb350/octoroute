//! Mutation discriminators for service-level boundaries and route decisions.

use super::*;
use crate::gateway::fabric::{FabricTransport, PoolLease, PreparedUpstreamResponse, ProviderLease};
use axum::http::{HeaderValue, header::AUTHORIZATION};
use secrecy::SecretString;
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[derive(Debug, Clone, Copy)]
struct InboundEnvironment;

impl Environment for InboundEnvironment {
    fn get(&self, name: &str) -> Option<SecretString> {
        (name == "OCTOROUTE_API_KEY").then(|| SecretString::from("inbound-test-key".to_string()))
    }
}

fn snapshot(
    pools: impl IntoIterator<Item = (&'static str, PoolAdmissionState)>,
    providers: impl IntoIterator<Item = (&'static str, ProviderAdmissionState)>,
) -> FabricReadiness {
    FabricReadiness {
        pools: pools
            .into_iter()
            .map(|(name, state)| (name.to_string(), state))
            .collect(),
        providers: providers
            .into_iter()
            .map(|(name, state)| (name.to_string(), state))
            .collect(),
    }
}

#[test]
fn readiness_accessors_and_aggregate_states_preserve_each_target_class() {
    let pool_ready = snapshot(
        [("workers", PoolAdmissionState::Ready)],
        [("cloud", ProviderAdmissionState::Unavailable)],
    );
    assert_eq!(
        pool_ready.pools(),
        &BTreeMap::from([("workers".to_string(), PoolAdmissionState::Ready)])
    );
    assert!(pool_ready.is_ready());
    assert!(pool_ready.is_degraded());

    let provider_ready = snapshot(
        [("workers", PoolAdmissionState::Busy)],
        [("cloud", ProviderAdmissionState::Ready)],
    );
    assert!(provider_ready.is_ready());
    assert!(!provider_ready.is_degraded());

    let unavailable = snapshot(
        [("workers", PoolAdmissionState::Unhealthy)],
        [("cloud", ProviderAdmissionState::Unavailable)],
    );
    assert!(!unavailable.is_ready());
    assert!(!unavailable.is_degraded());
}

#[test]
fn readiness_snapshot_expires_exactly_at_its_ttl() {
    let probed_at = Instant::now();
    let readiness = snapshot([("workers", PoolAdmissionState::Ready)], std::iter::empty());
    let cached = (probed_at, readiness);

    assert!(fresh_readiness_snapshot(Some(&cached), probed_at).is_some());
    assert!(
        fresh_readiness_snapshot(
            Some(&cached),
            probed_at + READINESS_SNAPSHOT_TTL - Duration::from_nanos(1),
        )
        .is_some()
    );
    assert!(fresh_readiness_snapshot(Some(&cached), probed_at + READINESS_SNAPSHOT_TTL).is_none());
    assert!(
        fresh_readiness_snapshot(
            Some(&cached),
            probed_at + READINESS_SNAPSHOT_TTL + Duration::from_nanos(1),
        )
        .is_none()
    );
    assert!(fresh_readiness_snapshot(None, probed_at).is_none());
}

fn authenticated_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer inbound-test-key"),
    );
    headers
}

fn config() -> FabricConfig {
    FabricConfig::from_toml(include_str!("../../../../config.toml")).expect("repository config")
}

fn service(config: FabricConfig) -> FabricGatewayService<FabricTransport> {
    FabricGatewayService::from_config(config, InboundEnvironment).expect("service")
}

#[tokio::test]
async fn direct_chat_body_size_boundary_is_exact() {
    let body = Bytes::from_static(b"{}");

    let mut exact_config = config();
    exact_config.server.max_request_bytes = body.len();
    let exact = service(exact_config)
        .handle_chat(&authenticated_headers(), body.clone())
        .await;
    assert_eq!(exact.status(), StatusCode::BAD_REQUEST);

    let mut over_config = config();
    over_config.server.max_request_bytes = body.len() - 1;
    let over = service(over_config)
        .handle_chat(&authenticated_headers(), body)
        .await;
    assert_eq!(over.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[test]
fn metadata_header_size_boundary_is_exact() {
    let exact_headers = authenticated_headers();
    let exact_bytes = header_bytes(&exact_headers);
    let mut exact_config = config();
    exact_config.server.max_header_bytes = exact_bytes;
    assert!(
        service(exact_config)
            .authorize_metadata(&exact_headers)
            .is_ok()
    );

    let mut over_headers = exact_headers;
    over_headers.insert("x-extra", HeaderValue::from_static("x"));
    let mut over_config = config();
    over_config.server.max_header_bytes = exact_bytes;
    assert!(matches!(
        service(over_config).authorize_metadata(&over_headers),
        Err(MetadataAuthorizationError::HeadersTooLarge)
    ));
}

#[derive(Debug, Clone, Copy)]
struct FailingTransport;

#[async_trait::async_trait]
impl FabricUpstreamTransport for FailingTransport {
    type Error = FabricTransportError;

    async fn local(&self, _lease: PoolLease) -> Result<PreparedUpstreamResponse, Self::Error> {
        Err(FabricTransportError::InvalidProviderResponse)
    }

    async fn provider(
        &self,
        _lease: ProviderLease,
    ) -> Result<PreparedUpstreamResponse, Self::Error> {
        Err(FabricTransportError::InvalidProviderResponse)
    }
}

async fn mount_local_admission(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 32})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn terminal_local_transport_failure_never_falls_forward() {
    let server = MockServer::start().await;
    mount_local_admission(&server).await;
    let mut config = config();
    let workers = config.local_pools.get_mut("workers").expect("workers");
    workers.members.truncate(1);
    workers.members[0].base_url = reqwest::Url::parse(&server.uri()).expect("mock URL");
    let service =
        FabricGatewayService::new(config, InboundEnvironment, FailingTransport).expect("service");
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "worker",
            "messages": [{"role": "user", "content": "stay on this step"}]
        }))
        .expect("request JSON"),
    );

    let response = service.handle_chat(&authenticated_headers(), body).await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(service.metrics_text().contains(
        "octoroute_fabric_pool_fallbacks_total{pool=\"workers\",trigger=\"precommit_failure\"} 0"
    ));
}
